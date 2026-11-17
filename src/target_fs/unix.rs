use std::ffi::{CString, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Serialize, de::DeserializeOwned};

use super::{Reply, Request, WirePath};

const MAX_FRAME: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub(super) enum State {
    #[default]
    Idle,
    Active {
        stream: UnixStream,
        child: Child,
        closed: bool,
    },
    Failed,
}

impl State {
    pub(super) fn stream(&self) -> Result<&UnixStream> {
        match self {
            Self::Active { stream, .. } => Ok(stream),
            _ => bail!("privileged helper is unavailable"),
        }
    }

    pub(super) fn request(
        &mut self,
        request: Request,
        no_prompt: bool,
        read_only: bool,
    ) -> Result<Reply> {
        if matches!(self, Self::Idle) {
            // Cache startup failure: retrying can repeat authentication or partially completed effects.
            *self = Self::Failed;
            let (stream, child) = start(no_prompt, read_only)?;
            *self = Self::Active {
                stream,
                child,
                closed: false,
            };
        }
        let result = (|| {
            let mut stream = self.stream()?;
            send(&mut stream, &request)?;
            if matches!(request, Request::Hook { .. }) {
                send_files(stream, &[0, 1, 2])?;
            }
            let reply = receive::<Reply>(&mut stream)?;
            ensure!(
                request.accepts(&reply),
                "unexpected privileged protocol response"
            );
            Ok(reply)
        })();
        match result {
            Ok(Reply::Error { kind, message }) => {
                let kind = match kind.as_deref() {
                    Some("NotFound") => std::io::ErrorKind::NotFound,
                    Some("PermissionDenied") => std::io::ErrorKind::PermissionDenied,
                    Some("AlreadyExists") => std::io::ErrorKind::AlreadyExists,
                    Some("NotADirectory") => std::io::ErrorKind::NotADirectory,
                    Some("DirectoryNotEmpty") => std::io::ErrorKind::DirectoryNotEmpty,
                    _ => std::io::ErrorKind::Other,
                };
                Err(std::io::Error::new(kind, message).into())
            }
            Ok(reply) => Ok(reply),
            Err(error) => {
                *self = Self::Failed;
                Err(error.context("privileged helper connection failed; operation was not retried"))
            }
        }
    }
}

impl State {
    pub(super) fn shutdown(&mut self) -> Result<()> {
        if let Self::Active {
            stream,
            child,
            closed,
        } = self
        {
            if *closed {
                return Ok(());
            }
            *closed = true;
            // macOS rejects socket options after both halves close, so configure the
            // timeout before EOF lets the helper exit and close its end of the socket.
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .context("could not configure privileged helper shutdown timeout")?;
            stream
                .shutdown(std::net::Shutdown::Write)
                .context("could not close privileged helper request stream")?;
            // Background sudo supervises the helper. EOF confirms filesystem cleanup
            // has finished before the caller returns and sudo reaps its command.
            let mut pending = [0_u8; 256];
            loop {
                match stream.read(&mut pending) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        return Err(error).context("privileged helper did not finish shutdown");
                    }
                }
            }
            let status = child.wait().context("could not reap sudo launcher")?;
            ensure!(status.success(), "sudo launcher failed ({status})");
        }
        Ok(())
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            tracing::warn!("could not finish privileged helper shutdown: {error}");
        }
    }
}

fn start(no_prompt: bool, read_only: bool) -> Result<(UnixStream, Child)> {
    let _pause = crate::ui::progress::pause()?;
    // A short private path avoids sockaddr_un limits for deeply nested workspaces.
    let directory = tempfile::Builder::new()
        .prefix("over-sudo-")
        .tempdir_in("/tmp")?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    let socket = directory.path().join("session");
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce).context("generate privileged session nonce")?;
    let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut command = Command::new("sudo");
    if no_prompt {
        command.arg("-n");
    }
    command
        // A persistent foreground sudo PTY would consume keys intended for caller-side prompts.
        .arg("-b")
        .arg("--")
        .arg(std::env::current_exe()?)
        .arg("--over-private-sudo-helper")
        .arg(&socket)
        .arg(&nonce)
        .arg(if read_only { "read-only" } else { "read-write" })
        // Authentication retains the controlling terminal, never the protocol socket.
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::inherit());
    let mut child = command
        .spawn()
        .context("start sudo; ensure sudo is installed and permitted")?;
    let mut launched_at = None;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                // BSD/macOS inherit listener O_NONBLOCK; framed reads require a blocking connection.
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
                let received = receive::<(u32, String)>(&mut stream);
                if !matches!(received, Ok((1, ref received)) if received == &nonce) {
                    continue;
                }
                stream.set_read_timeout(None)?;
                return Ok((stream, child));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(status) = child.try_wait()? {
                    ensure!(
                        status.success(),
                        "sudo authentication/helper startup failed ({status})"
                    );
                    // Background sudo can exit successfully just before its authenticated helper connects.
                    let launched = launched_at.get_or_insert_with(std::time::Instant::now);
                    ensure!(
                        launched.elapsed() < std::time::Duration::from_secs(5),
                        "sudo helper did not connect after successful authentication"
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        }
    }
}

fn send(stream: &mut impl Write, value: &impl Serialize) -> Result<()> {
    let bytes = zeroize::Zeroizing::new(serde_json::to_vec(value)?);
    ensure!(
        bytes.len() <= MAX_FRAME,
        "privileged protocol frame exceeds size limit"
    );
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}

fn receive<T: DeserializeOwned>(stream: &mut impl Read) -> Result<T> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    ensure!(
        length <= MAX_FRAME,
        "privileged protocol frame exceeds size limit"
    );
    let mut bytes = zeroize::Zeroizing::new(vec![0; length]);
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn send_files(stream: &UnixStream, files: &[RawFd]) -> Result<()> {
    ensure!(
        !files.is_empty() && files.len() <= 3,
        "invalid descriptor count"
    );
    let mut byte = [0_u8];
    let mut vector = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    // usize storage supplies cmsghdr alignment on supported Unix targets.
    let mut control = [0_usize; 16];
    // SAFETY: msghdr contains borrowed, correctly sized buffers throughout sendmsg.
    unsafe {
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = libc::CMSG_SPACE(std::mem::size_of_val(files) as _) as _;
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(files) as _) as _;
        std::ptr::copy_nonoverlapping(
            files.as_ptr().cast::<u8>(),
            libc::CMSG_DATA(header),
            std::mem::size_of_val(files),
        );
        if libc::sendmsg(stream.as_raw_fd(), &message, 0) != 1 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

fn receive_files(stream: &UnixStream, expected: usize) -> Result<Vec<OwnedFd>> {
    let mut byte = [0_u8];
    let mut vector = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0_usize; 16];
    // SAFETY: recvmsg writes only into the supplied buffers. Validated SCM_RIGHTS
    // descriptors are immediately owned so every subsequent failure closes them.
    unsafe {
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&control) as _;
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_CMSG_CLOEXEC;
        #[cfg(not(target_os = "linux"))]
        let flags = 0;
        // Atomic CLOEXEC prevents a concurrent caller-side Git spawn from inheriting target descriptors.
        ensure!(
            libc::recvmsg(stream.as_raw_fd(), &mut message, flags) == 1,
            "could not receive privileged descriptors"
        );
        let mut files = Vec::new();
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let length = (*header).cmsg_len as usize;
                ensure!(
                    length >= libc::CMSG_LEN(0) as usize,
                    "invalid descriptor message"
                );
                let count = (length - libc::CMSG_LEN(0) as usize) / std::mem::size_of::<RawFd>();
                let descriptors =
                    std::slice::from_raw_parts(libc::CMSG_DATA(header).cast::<RawFd>(), count);
                for &fd in descriptors {
                    let owned = OwnedFd::from_raw_fd(fd);
                    ensure!(
                        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) != -1,
                        "cannot protect received descriptor"
                    );
                    files.push(owned);
                }
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
        ensure!(
            message.msg_flags & libc::MSG_CTRUNC == 0 && files.len() == expected,
            "incorrect privileged descriptor count"
        );
        Ok(files)
    }
}

pub(super) fn receive_file(stream: &UnixStream) -> Result<File> {
    let mut files = receive_files(stream, 1)?;
    Ok(File::from(files.remove(0)))
}

pub(super) fn serve(args: Vec<OsString>) -> Result<()> {
    ensure!(args.len() == 3, "invalid private helper arguments");
    let read_only = match args[2].to_str() {
        Some("read-only") => true,
        Some("read-write") => false,
        _ => bail!("invalid private helper capability"),
    };
    let mut stream = UnixStream::connect(Path::new(&args[0]))?;
    send(
        &mut stream,
        &(1_u32, args[1].to_str().context("invalid session nonce")?),
    )?;
    let mut effects = std::collections::HashMap::new();
    let mut bindings = ParentBindings::default();
    loop {
        let request: Request = match receive(&mut stream) {
            Ok(request) => request,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::UnexpectedEof) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let descriptors = if matches!(request, Request::Hook { .. }) {
            receive_files(&stream, 3)?
        } else {
            Vec::new()
        };
        let result = if read_only && request.mutates() {
            Err(anyhow::anyhow!(
                "read-only privileged session cannot mutate files or execute hooks"
            ))
        } else {
            execute(request, descriptors, &mut effects, &stream, &mut bindings)
        };
        match result {
            Ok((reply, file)) => {
                send(&mut stream, &reply)?;
                if let Some(file) = file {
                    send_files(&stream, &[file.as_raw_fd()])?;
                }
            }
            Err(error) => {
                let kind = error
                    .downcast_ref::<std::io::Error>()
                    .map(|error| format!("{:?}", error.kind()));
                send(
                    &mut stream,
                    &Reply::Error {
                        kind,
                        message: error.to_string(),
                    },
                )?;
            }
        }
    }
}

fn execute(
    request: Request,
    mut descriptors: Vec<OwnedFd>,
    effects: &mut std::collections::HashMap<u64, Effects>,
    control: &UnixStream,
    bindings: &mut ParentBindings,
) -> Result<(Reply, Option<File>)> {
    match request {
        Request::PrepareFile { id, path, update } => {
            ensure!(
                !effects.contains_key(&id),
                "duplicate privileged preparation ID"
            );
            let anchored = Anchored::new(&path.path(), true, bindings)?;
            let _cwd = anchored.enter()?;
            if update.is_some() {
                ensure_content_metadata(&anchored.path)?;
            }
            anchored.record_directory(bindings)?;
            let destination = anchored.path.clone();
            let mut effect = Effects::new(anchored)?;
            effect.regular.prepare(destination, update)?;
            effects.insert(id, effect);
        }
        Request::Prepare { id, path, contents } => {
            use crate::materialize::virtual_checkout::initial::{Inventory, Node, prepare_update};
            ensure!(
                !effects.contains_key(&id),
                "duplicate privileged preparation ID"
            );
            let anchored = Anchored::new(&path.path(), true, bindings)?;
            let _cwd = anchored.enter()?;
            let mut inventory = Inventory::new();
            for (path, node) in contents {
                let path = path.path();
                ensure!(
                    path.components().all(|component| matches!(
                        component,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )),
                    "prepared inventory contains an escaping path"
                );
                inventory.insert(
                    path,
                    match node {
                        super::PreparedNode::Directory => Node::Directory,
                        super::PreparedNode::File(bytes, mode) => Node::File(bytes, mode),
                    },
                );
            }
            if inventory.contains_key(Path::new("")) {
                ensure_content_metadata(&anchored.path)?;
            }
            anchored.record_directory(bindings)?;
            let destination = anchored.path.clone();
            let mut effect = Effects::new(anchored)?;
            prepare_update(
                &mut effect.regular,
                &mut effect.replacements,
                destination,
                &inventory,
                false,
                None,
            )?;
            effects.insert(id, effect);
        }
        Request::Apply { id } => {
            let effect = effects
                .get_mut(&id)
                .context("unknown privileged preparation")?;
            effect.validate(bindings)?;
            let _cwd = effect.enter()?;
            effect.regular.apply()?;
            for replacement in &mut effect.replacements {
                replacement.apply()?;
            }
            bindings.forget(&effect.logical);
            effect._anchors[0].record_directory(bindings)?;
        }
        Request::Rollback { id } => {
            let effect = effects
                .get_mut(&id)
                .context("unknown privileged preparation")?;
            effect.rollback()?;
            bindings.forget(&effect.logical);
            effect._anchors[0].record_directory(bindings)?;
        }
        Request::Finish { id } => {
            let mut effect = effects
                .remove(&id)
                .context("unknown privileged preparation")?;
            let _cwd = effect.enter()?;
            ensure!(
                !effect.retain_backups,
                "privileged rollback failed; original backups were retained"
            );
            effect.finished = true;
            effect.regular.cleanup()?;
        }
        Request::Read { path } => {
            let anchored = Anchored::new(&path.path(), false, bindings)?;
            let _cwd = anchored.enter()?;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&anchored.path)?;
            ensure!(
                file.metadata()?.is_file(),
                "privileged reads require a regular file"
            );
            let mut bytes = Vec::new();
            file.take(MAX_FRAME as u64).read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() < MAX_FRAME,
                "privileged file exceeds transfer size limit"
            );
            return Ok((Reply::Bytes(bytes), None));
        }
        Request::Inspect { path, follow } => {
            let anchored = Anchored::new(&path.path(), false, bindings)?;
            let _cwd = anchored.enter()?;
            use std::os::unix::ffi::OsStrExt;
            let path = CString::new(anchored.path.as_os_str().as_bytes())?;
            #[cfg(target_os = "linux")]
            let flags = libc::O_PATH | libc::O_CLOEXEC | if follow { 0 } else { libc::O_NOFOLLOW };
            #[cfg(target_os = "macos")]
            let flags = libc::O_RDONLY | libc::O_CLOEXEC | if follow { 0 } else { libc::O_SYMLINK };
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let flags =
                libc::O_RDONLY | libc::O_CLOEXEC | if follow { 0 } else { libc::O_NOFOLLOW };
            // SAFETY: the NUL-terminated path lives through open; the descriptor is owned immediately.
            let fd = unsafe { libc::open(path.as_ptr(), flags) };
            if fd == -1 {
                return Err(std::io::Error::last_os_error().into());
            }
            let file = unsafe { File::from_raw_fd(fd) };
            if !follow && file.metadata()?.is_dir() {
                bindings.observe(&anchored.logical, &file)?;
            }
            return Ok((Reply::Descriptor, Some(file)));
        }
        Request::ReadLink { path } => {
            let anchored = Anchored::new(&path.path(), false, bindings)?;
            let _cwd = anchored.enter()?;
            return Ok((
                Reply::Paths(vec![WirePath::new(&std::fs::read_link(&anchored.path)?)]),
                None,
            ));
        }
        Request::ReadDir { path } => {
            let original = path.path();
            let anchored = Anchored::new(&original, false, bindings)?;
            let _cwd = anchored.enter()?;
            let directory = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
                .open(&anchored.path)?;
            bindings.observe(&original, &directory)?;
            let _directory = WorkingDirectory::enter(&directory)?;
            let paths = std::fs::read_dir(".")?
                .map(|entry| entry.map(|entry| WirePath::new(&original.join(entry.file_name()))))
                .collect::<std::io::Result<_>>()?;
            return Ok((Reply::Paths(paths), None));
        }
        Request::Location { path } => {
            let anchored = Anchored::resolve(&path.path(), false, true, bindings)?;
            let _cwd = anchored.enter()?;
            return Ok((
                Reply::Paths(vec![WirePath::new(&crate::overlays::mapping::absolute(
                    &std::env::current_dir()?.join(&anchored.path),
                )?)]),
                None,
            ));
        }
        Request::Physical { path } => {
            let anchored = Anchored::resolve(&path.path(), false, true, bindings)?;
            let _cwd = anchored.enter()?;
            let physical = match std::fs::symlink_metadata(&anchored.path) {
                Ok(metadata) if metadata.is_symlink() => std::fs::canonicalize(&anchored.path)?,
                Ok(_) => std::env::current_dir()?.join(&anchored.path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::env::current_dir()?.join(&anchored.path)
                }
                Err(error) => return Err(error.into()),
            };
            return Ok((
                Reply::Paths(vec![WirePath::new(&crate::overlays::mapping::absolute(
                    &physical,
                )?)]),
                None,
            ));
        }
        Request::CreateDir { path } => {
            let anchored = Anchored::new(&path.path(), true, bindings)?;
            let _cwd = anchored.enter()?;
            match std::fs::create_dir(&anchored.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => ensure!(
                    std::fs::symlink_metadata(&anchored.path)?.is_dir(),
                    "directory creation encountered a non-directory"
                ),
                Err(error) => return Err(error.into()),
            }
            anchored.record_directory(bindings)?;
        }
        Request::Symlink { source, target } => {
            let anchored = Anchored::new(&target.path(), false, bindings)?;
            let _cwd = anchored.enter()?;
            std::os::unix::fs::symlink(source.path(), &anchored.path)?;
        }
        Request::HardLink { source, target } => {
            let anchored = Anchored::new(&target.path(), false, bindings)?;
            let _cwd = anchored.enter()?;
            std::fs::hard_link(source.path(), &anchored.path)?;
        }
        Request::RemoveFile { path } => {
            let anchored = Anchored::new(&path.path(), false, bindings)?;
            let _cwd = anchored.enter()?;
            std::fs::remove_file(&anchored.path)?;
            bindings.forget(&anchored.logical);
        }
        Request::RemoveDir { path } => {
            let anchored = Anchored::new(&path.path(), false, bindings)?;
            let _cwd = anchored.enter()?;
            std::fs::remove_dir(&anchored.path)?;
            bindings.forget(&anchored.logical);
        }
        Request::Write { path, bytes, mode } => {
            let anchored = Anchored::new(&path.path(), false, bindings)?;
            let _cwd = anchored.enter()?;
            ensure_content_metadata(&anchored.path)?;
            super::replace(&anchored.path, &bytes, mode)?;
        }
        Request::SecureWrite {
            path,
            bytes,
            before,
            mode,
        } => {
            let bytes = crate::secrets::SecretBytes::new(bytes);
            let before = before.map(crate::secrets::SecretBytes::new);
            let anchored = Anchored::new(&path.path(), false, bindings)?;
            let _cwd = anchored.enter()?;
            secure_write(
                &anchored.path,
                bytes.expose(),
                before.as_ref().map(|bytes| bytes.expose()),
                mode,
            )?;
        }
        Request::Metadata {
            path,
            mode,
            owner,
            group,
            expected,
        } => {
            let anchored = Anchored::new(&path.path(), false, bindings)?;
            let _cwd = anchored.enter()?;
            repair_metadata_checked(&anchored.path, mode, owner, group, expected.as_ref())?;
        }
        Request::Hook {
            script,
            cwd,
            env,
            remove_env,
        } => {
            let stderr = descriptors.pop().context("missing hook stderr")?;
            let stdout = descriptors.pop().context("missing hook stdout")?;
            let stdin = descriptors.pop().context("missing hook stdin")?;
            let stdin = File::from(stdin);
            let terminal = stdin.try_clone()?;
            let mut command = Command::new("sh");
            let _signals = ForwardSignals::new()?;
            command.process_group(0);
            command
                .arg("-c")
                .arg(script)
                .current_dir(cwd.path())
                .envs(env.iter().map(|(name, value)| {
                    (name.path().into_os_string(), value.path().into_os_string())
                }))
                .stdin(Stdio::from(stdin))
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr));
            for name in remove_env {
                command.env_remove(name.path().into_os_string());
            }
            let mut child = command.spawn().map_err(|error| {
                anyhow::anyhow!("could not start elevated hook ({:?})", error.kind())
            })?;
            HOOK_GROUP.store(child.id() as i32, std::sync::atomic::Ordering::SeqCst);
            let _foreground = match Foreground::claim(terminal, child.id() as i32) {
                Ok(foreground) => foreground,
                Err(error) => {
                    // Do not strand a child that may already be stopped waiting for terminal ownership.
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                    child.wait()?;
                    return Err(error);
                }
            };
            let status = loop {
                if let Some(status) = child
                    .try_wait()
                    .context("could not wait for elevated hook")?
                {
                    break status;
                }
                let mut descriptor = libc::pollfd {
                    fd: control.as_raw_fd(),
                    events: 0,
                    revents: 0,
                };
                // SAFETY: poll borrows one initialized descriptor for this call only.
                let result = unsafe { libc::poll(&mut descriptor, 1, 20) };
                if result > 0 && descriptor.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                    // A dead caller cannot consume input or authorize further work.
                    // Terminate the whole hook group before releasing helper resources.
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGTERM);
                    }
                    for _ in 0..50 {
                        if child.try_wait()?.is_some() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    if child.try_wait()?.is_none() {
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        }
                    }
                    child
                        .wait()
                        .context("could not reap interrupted elevated hook")?;
                    bail!("caller disconnected while an elevated hook was running");
                }
                if result == -1
                    && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    child
                        .kill()
                        .context("could not stop elevated hook after connection failure")?;
                    child.wait()?;
                    bail!("could not monitor elevated hook connection");
                }
            };
            HOOK_GROUP.store(0, std::sync::atomic::Ordering::SeqCst);
            return Ok((Reply::Exit(status.into_raw()), None));
        }
    }
    Ok((Reply::Done, None))
}

static HOOK_GROUP: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

struct Foreground {
    terminal: File,
    previous: libc::pid_t,
}

impl Foreground {
    fn claim(terminal: File, group: libc::pid_t) -> Result<Option<Self>> {
        // A passed pipe or a terminal from a different session needs no job-control change.
        let previous = unsafe { libc::tcgetpgrp(terminal.as_raw_fd()) };
        if previous == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOTTY) {
                return Ok(None);
            }
            return Err(error.into());
        }
        // SAFETY: the descriptor is live, SIGTTOU is ignored for the scope, and
        // the hook's private group belongs to this terminal session.
        if unsafe { libc::tcsetpgrp(terminal.as_raw_fd(), group) } == -1 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(None);
            }
            return Err(error.into());
        }
        // The child may have tried reading before foreground ownership was transferred.
        unsafe {
            libc::kill(-group, libc::SIGCONT);
        }
        Ok(Some(Self { terminal, previous }))
    }
}

impl Drop for Foreground {
    fn drop(&mut self) {
        // SAFETY: restore the original foreground while the signal guard and fd are still live.
        if unsafe { libc::tcsetpgrp(self.terminal.as_raw_fd(), self.previous) } == -1 {
            eprintln!(
                "over privileged helper: could not restore terminal foreground: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

extern "C" fn forward_signal(signal: libc::c_int) {
    let group = HOOK_GROUP.load(std::sync::atomic::Ordering::SeqCst);
    if group > 0 {
        // SAFETY: kill is async-signal-safe. A private process group includes shell descendants.
        unsafe {
            libc::kill(-group, signal);
        }
    }
}

struct ForwardSignals(Vec<(libc::c_int, libc::sigaction)>);

impl ForwardSignals {
    fn new() -> Result<Self> {
        let mut installed = Self(Vec::new());
        for signal in [
            libc::SIGINT,
            libc::SIGTERM,
            libc::SIGHUP,
            libc::SIGQUIT,
            libc::SIGWINCH,
            libc::SIGTTOU,
        ] {
            // SAFETY: sigaction reads/writes valid initialized storage; this helper
            // is single-threaded and the previous handlers are restored by the guard.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = if signal == libc::SIGTTOU {
                    libc::SIG_IGN
                } else {
                    forward_signal as *const () as usize
                };
                libc::sigemptyset(&mut action.sa_mask);
                let mut previous = std::mem::zeroed();
                if libc::sigaction(signal, &action, &mut previous) == -1 {
                    return Err(std::io::Error::last_os_error().into());
                }
                installed.0.push((signal, previous));
            }
        }
        Ok(installed)
    }
}

impl Drop for ForwardSignals {
    fn drop(&mut self) {
        HOOK_GROUP.store(0, std::sync::atomic::Ordering::SeqCst);
        for (signal, action) in self.0.iter().rev() {
            // SAFETY: these saved handlers came from successful sigaction calls.
            unsafe {
                libc::sigaction(*signal, action, std::ptr::null_mut());
            }
        }
    }
}

struct Effects {
    logical: std::path::PathBuf,
    expected: Option<(u64, u64, u32)>,
    // Default constructs filesystem-only transactions; never call for_repository in the helper.
    regular: crate::materialize::virtual_checkout::git::SourceTransaction,
    replacements: Vec<crate::materialize::virtual_checkout::initial::Replacement>,
    _anchors: Vec<Anchored>,
    finished: bool,
    retain_backups: bool,
}

impl Effects {
    fn new(anchor: Anchored) -> Result<Self> {
        Ok(Self {
            logical: anchor.logical.clone(),
            expected: leaf_identity(&anchor.path)?,
            regular: Default::default(),
            replacements: Vec::new(),
            _anchors: vec![anchor],
            finished: false,
            retain_backups: false,
        })
    }
    fn validate(&self, bindings: &mut ParentBindings) -> Result<()> {
        let current = Anchored::new(&self.logical, false, bindings)?;
        let expected_parent = self._anchors[0].parent.metadata()?;
        let parent = current.parent.metadata()?;
        ensure!(
            (parent.dev(), parent.ino()) == (expected_parent.dev(), expected_parent.ino()),
            "target ancestor changed after preparation"
        );
        let _cwd = self.enter()?;
        ensure!(
            leaf_identity(&self._anchors[0].path)? == self.expected,
            "target changed after privileged preparation"
        );
        Ok(())
    }
    fn enter(&self) -> Result<WorkingDirectory> {
        self._anchors
            .first()
            .context("privileged preparation has no pinned parent")?
            .enter()
    }
    fn rollback(&mut self) -> Result<()> {
        let result = (|| {
            let _cwd = self.enter()?;
            for replacement in self.replacements.iter_mut().rev() {
                replacement.rollback()?;
            }
            self.regular.rollback()
        })();
        if result.is_err() {
            self.retain_backups = true;
        }
        result
    }
}

fn leaf_identity(path: &Path) -> Result<Option<(u64, u64, u32)>> {
    match std::fs::symlink_metadata(path) {
        // Permission repairs do not change the node's identity or Unix file type.
        Ok(metadata) => Ok(Some((
            metadata.dev(),
            metadata.ino(),
            metadata.mode() & 0o170000,
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

impl Drop for Effects {
    fn drop(&mut self) {
        let _cwd = match self.enter() {
            Ok(cwd) => cwd,
            Err(error) => {
                eprintln!(
                    "over privileged helper: cannot enter backup directory; backups retained: {error}"
                );
                // Relative TempDir destructors must never run in an unrelated directory.
                std::mem::forget(std::mem::take(&mut self.replacements));
                return;
            }
        };
        if !self.finished
            && let Err(error) = self.rollback()
        {
            eprintln!("over privileged helper: filesystem rollback failed: {error}");
        }
        if self.retain_backups {
            std::mem::forget(std::mem::take(&mut self.replacements));
            return;
        }
        if let Err(error) = self.regular.cleanup() {
            eprintln!("over privileged helper: preparation cleanup failed: {error}");
        }
        // Drop relative backup TempDirs while their pinned parent is still cwd.
        self.replacements.clear();
    }
}

pub(super) fn secure_write(
    path: &Path,
    bytes: &[u8],
    before: Option<&[u8]>,
    mode: u32,
) -> Result<()> {
    if before != Some(bytes) {
        ensure_content_metadata(path)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600);
    if before.is_none() {
        options.create_new(true);
    }
    let mut file = options
        .open(path)
        .context("could not open encrypted destination")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.nlink() == 1,
        "encrypted destinations must be unshared regular files"
    );
    if let Some(before) = before {
        let mut current = zeroize::Zeroizing::new(Vec::new());
        file.read_to_end(&mut current)?;
        ensure!(
            current.as_slice() == before,
            "encrypted target changed while awaiting authorization"
        );
        ensure!(
            before == bytes || !metadata.permissions().readonly(),
            "encrypted target is read-only"
        );
    }
    let observed = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.dev() == observed.dev() && metadata.ino() == observed.ino(),
        "encrypted target changed before writing"
    );
    file.set_permissions(std::fs::Permissions::from_mode(mode & 0o7777))?;
    if before != Some(bytes) {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))?;
        file.write_all(bytes)?;
        file.set_len(bytes.len() as u64)?;
    }
    file.sync_all()?;
    Ok(())
}

/// Root must not make a hardlink-detaching write succeed by silently retaining
/// or stripping privilege metadata that an ordinary in-place write refuses.
fn ensure_content_metadata(path: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Ok(());
    }
    ensure!(
        metadata.mode() & 0o6000 == 0,
        "cannot preserve set-id metadata while writing"
    );
    #[cfg(target_os = "linux")]
    for name in ["security.capability", "security.ima", "security.evm"] {
        match xattr::get(path, name) {
            Ok(None) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {}
            Ok(Some(_)) => bail!("cannot preserve privilege metadata while writing"),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Pin the resolved parent before touching a leaf: later ancestor renames or
/// symlink substitutions cannot redirect a privileged operation elsewhere.
struct Anchored {
    parent: File,
    path: std::path::PathBuf,
    logical: std::path::PathBuf,
}

#[derive(Default)]
struct ParentBindings {
    directories: std::collections::HashMap<std::path::PathBuf, (u64, u64)>,
    missing: std::collections::HashSet<std::path::PathBuf>,
}

impl ParentBindings {
    fn observe(&mut self, path: &Path, directory: &File) -> Result<()> {
        let metadata = directory.metadata()?;
        let identity = (metadata.dev(), metadata.ino());
        if let Some(expected) = self.directories.get(path) {
            ensure!(
                *expected == identity,
                "target ancestor '{}' changed during privileged execution; rerun to re-evaluate",
                path.display()
            );
        } else {
            self.directories.insert(path.to_owned(), identity);
        }
        self.missing.remove(path);
        Ok(())
    }

    fn forget(&mut self, path: &Path) {
        // Only our own authorized shape changes can replace a previously observed directory.
        self.directories.retain(|known, _| !known.starts_with(path));
        self.missing.retain(|known| !known.starts_with(path));
    }
}

impl Anchored {
    fn enter(&self) -> Result<WorkingDirectory> {
        WorkingDirectory::enter(&self.parent)
    }
    fn new(path: &Path, create_parents: bool, bindings: &mut ParentBindings) -> Result<Self> {
        Self::resolve(path, create_parents, false, bindings)
    }

    fn record_directory(&self, bindings: &mut ParentBindings) -> Result<()> {
        let _cwd = self.enter()?;
        let metadata = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if metadata.is_dir() {
            let directory = open_directory_at(&self.parent, self.path.as_os_str(), false)?;
            bindings.observe(&self.logical, &directory)?;
        }
        Ok(())
    }

    fn resolve(
        path: &Path,
        create_parents: bool,
        allow_missing: bool,
        bindings: &mut ParentBindings,
    ) -> Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        ensure!(
            path.is_absolute(),
            "privileged target paths must be absolute"
        );
        let path = crate::overlays::mapping::absolute(path)?;
        let (parent, leaf) = match (path.parent(), path.file_name()) {
            (Some(parent), Some(leaf)) => (parent, leaf),
            _ => (Path::new("/"), std::ffi::OsStr::new(".")),
        };
        let mut directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open("/")?;
        bindings.observe(Path::new("/"), &directory)?;
        let mut prefix = std::path::PathBuf::from("/");
        let mut relative = std::path::PathBuf::from(".");
        let mut missing = false;
        for name in parent.components().filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name),
            _ => None,
        }) {
            prefix.push(name);
            if missing {
                bindings.missing.insert(prefix.clone());
                relative.push(name);
                continue;
            }
            // Existing aliases are allowed once, then bound to their observed inode.
            // A missing directory must never appear later as an unobserved symlink.
            match open_directory_at(&directory, name, !bindings.missing.contains(&prefix)) {
                Ok(next) => directory = next,
                Err(error) if crate::target_fs::is_not_found(&error) => {
                    bindings.missing.insert(prefix.clone());
                    if create_parents {
                        let name_c = CString::new(name.as_bytes())?;
                        // SAFETY: the descriptor and NUL-terminated basename remain live.
                        if unsafe { libc::mkdirat(directory.as_raw_fd(), name_c.as_ptr(), 0o777) }
                            == -1
                        {
                            let error = std::io::Error::last_os_error();
                            if error.kind() != std::io::ErrorKind::AlreadyExists {
                                return Err(error.into());
                            }
                        }
                        directory = open_directory_at(&directory, name, false)?;
                    } else if allow_missing {
                        missing = true;
                        relative.push(name);
                        continue;
                    } else {
                        return Err(error);
                    }
                }
                Err(error) => return Err(error),
            }
            bindings.observe(&prefix, &directory)?;
        }
        relative.push(leaf);
        Ok(Self {
            parent: directory,
            path: relative,
            logical: path,
        })
    }
}

/// The helper is deliberately single-threaded. Scoped fchdir lets existing
/// metadata-preserving transactions use pinned parents portably, without /proc.
/// Hooks set cwd only on their child after this guard has restored helper cwd.
struct WorkingDirectory(File);

impl WorkingDirectory {
    fn enter(directory: &File) -> Result<Self> {
        let previous = File::open(".")?;
        // SAFETY: directory is live and fchdir is confined to the single-threaded helper.
        if unsafe { libc::fchdir(directory.as_raw_fd()) } == -1 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(previous))
    }
}

impl Drop for WorkingDirectory {
    fn drop(&mut self) {
        // SAFETY: the saved descriptor remains live until after cwd restoration.
        if unsafe { libc::fchdir(self.0.as_raw_fd()) } == -1 {
            eprintln!(
                "over privileged helper: could not restore working directory: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

fn open_directory_at(parent: &File, name: &std::ffi::OsStr, follow: bool) -> Result<File> {
    use std::os::unix::ffi::OsStrExt;
    let name = CString::new(name.as_bytes())?;
    // SAFETY: openat borrows a live parent and NUL-terminated name. The returned
    // descriptor is owned immediately and checked against its prior binding.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_CLOEXEC
                | if follow { 0 } else { libc::O_NOFOLLOW },
        )
    };
    if fd == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn resolve_name(name: &str, group: bool) -> Result<u32> {
    let name = CString::new(name).context("invalid ownership name")?;
    let mut length = 1024;
    loop {
        let mut buffer = vec![0_u8; length];
        // SAFETY: reentrant NSS calls use independent initialized output storage
        // and a live scratch buffer; no pointers escape this lookup.
        let (code, id) = unsafe {
            if group {
                let mut entry: libc::group = std::mem::zeroed();
                let mut result = std::ptr::null_mut();
                let code = libc::getgrnam_r(
                    name.as_ptr(),
                    &mut entry,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut result,
                );
                (code, (!result.is_null()).then_some(entry.gr_gid))
            } else {
                let mut entry: libc::passwd = std::mem::zeroed();
                let mut result = std::ptr::null_mut();
                let code = libc::getpwnam_r(
                    name.as_ptr(),
                    &mut entry,
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    &mut result,
                );
                (code, (!result.is_null()).then_some(entry.pw_uid))
            }
        };
        if code == libc::ERANGE && length < 1024 * 1024 {
            length *= 2;
            continue;
        }
        if code != 0 {
            return Err(std::io::Error::from_raw_os_error(code).into());
        }
        return id.context(if group {
            "unknown ownership group"
        } else {
            "unknown ownership user"
        });
    }
}

#[cfg(test)]
fn repair_metadata(
    path: &Path,
    mode: Option<u32>,
    owner: Option<u32>,
    group: Option<u32>,
) -> Result<()> {
    repair_metadata_checked(path, mode, owner, group, None)
}

pub(super) fn repair_metadata_checked(
    path: &Path,
    mode: Option<u32>,
    owner: Option<u32>,
    group: Option<u32>,
    expected: Option<&super::MetadataIdentity>,
) -> Result<()> {
    let mut metadata = std::fs::symlink_metadata(path)?;
    if let Some(expected) = expected {
        expected.verify(&metadata)?;
    }
    let owner = owner.filter(|id| *id != metadata.uid());
    let group = group.filter(|id| *id != metadata.gid());
    let mode = mode.filter(|mode| !metadata.is_symlink() && *mode != metadata.mode() & 0o7777);
    if owner.is_none() && group.is_none() && mode.is_none() {
        return Ok(());
    }
    ensure!(
        !metadata.is_symlink() || metadata.nlink() == 1,
        "cannot independently repair ownership of a hard-linked symlink"
    );
    if metadata.is_file() && metadata.nlink() > 1 {
        // Reuse the existing hardlink-safe repair path, including native metadata checks.
        crate::actions::fs::detach_for_metadata(path)?;
        // Detachment is authorized, but later races must still match the new owned inode.
        metadata = std::fs::symlink_metadata(path)?;
    }
    let file = if metadata.is_symlink() {
        None
    } else {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let current = std::fs::symlink_metadata(path)?;
        let opened = file.metadata()?;
        super::MetadataIdentity::capture(&metadata)?.verify(&opened)?;
        ensure!(
            current.dev() == opened.dev() && current.ino() == opened.ino(),
            "target changed during metadata repair"
        );
        Some(file)
    };
    if owner.is_some() || group.is_some() {
        ensure!(
            metadata.mode() & 0o6000 == 0,
            "cannot preserve set-id metadata during ownership repair"
        );
        #[cfg(target_os = "linux")]
        if !metadata.is_symlink() {
            for name in ["security.capability", "security.ima", "security.evm"] {
                match xattr::get(path, name) {
                    Ok(None) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Unsupported => {}
                    Ok(Some(_)) => {
                        bail!("cannot preserve privilege metadata during ownership repair")
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        if let Some(file) = &file {
            // SAFETY: fchown acts on the pinned inode, never a raced-in symlink referent.
            if unsafe {
                libc::fchown(
                    file.as_raw_fd(),
                    owner.unwrap_or(u32::MAX),
                    group.unwrap_or(u32::MAX),
                )
            } == -1
            {
                return Err(std::io::Error::last_os_error().into());
            }
        } else {
            super::MetadataIdentity::capture(&metadata)?
                .verify(&std::fs::symlink_metadata(path)?)?;
            std::os::unix::fs::lchown(path, owner, group)?;
        }
    }
    // Chown may clear mode bits: finalize the requested/preserved mode afterward.
    if !metadata.is_symlink() && (mode.is_some() || owner.is_some() || group.is_some()) {
        file.context("missing metadata repair descriptor")?
            .set_permissions(std::fs::Permissions::from_mode(
                mode.unwrap_or(metadata.mode() & 0o7777),
            ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_metadata_mutation_rejects_a_replaced_file_without_chmod() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        std::fs::write(&target, "owned").unwrap();
        let identity =
            super::super::MetadataIdentity::capture(&std::fs::symlink_metadata(&target).unwrap())
                .unwrap();
        std::fs::rename(&target, temp.path().join("old")).unwrap();
        std::fs::write(&target, "foreign").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Exercise the exact identity check used by privileged Metadata requests.
        assert!(
            repair_metadata_checked(&target, Some(0o600), None, None, Some(&identity)).is_err()
        );
        assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o644);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ownership_repair_never_changes_an_unowned_hardlinked_symlink_alias() {
        let temp = tempfile::tempdir().unwrap();
        let managed = temp.path().join("managed");
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink("missing", &managed).unwrap();
        std::fs::hard_link(&managed, &alias).unwrap();
        let original = std::fs::symlink_metadata(&alias).unwrap();
        let other = if original.uid() == 0 { 1 } else { 0 };
        let error = repair_metadata(&managed, None, Some(other), None).unwrap_err();
        assert!(error.to_string().contains("hard-linked symlink"));
        assert_eq!(
            std::fs::symlink_metadata(&alias).unwrap().uid(),
            original.uid()
        );
        assert_eq!(
            std::fs::symlink_metadata(&managed).unwrap().uid(),
            original.uid()
        );
    }

    #[test]
    fn oversized_protocol_frames_are_rejected_before_reading_the_payload() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        writer
            .write_all(&((MAX_FRAME + 1) as u32).to_be_bytes())
            .unwrap();
        assert!(
            receive::<Reply>(&mut reader)
                .err()
                .expect("oversized frame must fail")
                .to_string()
                .contains("size limit")
        );
    }

    #[test]
    fn shutdown_succeeds_when_the_peer_has_already_finished_writing() {
        let (mut stream, peer) = UnixStream::pair().unwrap();
        // Establish EOF before shutdown so macOS deterministically rejects setting
        // socket options after the caller also closes its write half.
        peer.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(stream.read(&mut [0_u8; 1]).unwrap(), 0);
        // A harmless child supplies lifecycle ownership without authenticating in this test.
        let child = Command::new("true").spawn().unwrap();
        let mut state = State::Active {
            stream,
            child,
            closed: false,
        };

        state.shutdown().unwrap();
        state.shutdown().unwrap();
    }

    #[test]
    fn protocol_corruption_permanently_disables_the_session_instead_of_replaying_work() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let _: Request = receive(&mut peer).unwrap();
            send(&mut peer, &Reply::Exit(0)).unwrap();
        });
        // A harmless child supplies lifecycle ownership without authenticating in this test.
        let child = Command::new("true").spawn().unwrap();
        let mut state = State::Active {
            stream,
            child,
            closed: false,
        };
        let request = || Request::Read {
            path: WirePath::new(Path::new("/unused")),
        };
        assert!(state.request(request(), true, true).is_err());
        assert!(matches!(state, State::Failed));
        assert!(state.request(request(), true, true).is_err());
        server.join().unwrap();
    }

    #[test]
    fn an_established_ancestor_alias_cannot_be_retargeted_between_helper_requests() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir(&original).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let mut bindings = ParentBindings::default();
        let selected = alias.join("file");
        let _first = Anchored::new(&selected, false, &mut bindings).unwrap();
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&foreign, &alias).unwrap();
        assert!(Anchored::new(&selected, false, &mut bindings).is_err());
    }

    #[test]
    fn a_missing_parent_cannot_appear_as_a_new_symlink_before_a_privileged_write() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let foreign = temp.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        let mut bindings = ParentBindings::default();
        let selected = missing.join("file");
        let _first = Anchored::resolve(&selected, false, true, &mut bindings).unwrap();
        std::os::unix::fs::symlink(&foreign, &missing).unwrap();
        assert!(Anchored::new(&selected, true, &mut bindings).is_err());
        assert!(!foreign.join("file").exists());
    }
}
