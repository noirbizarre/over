//! Native inspection of private Windows storage; ACL initialization stays with its caller.

use std::io;
use std::mem::{align_of, offset_of, size_of};
use std::os::windows::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr::{null_mut, read_unaligned};
use std::slice;

use anyhow::{Context as _, Result, bail, ensure};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetTokenInformation, IsValidAcl, IsValidSecurityDescriptor, IsValidSid,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SID, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

// The SDK exposes these tags under SystemServices, outside our required feature set.
// Whitelisting only these layouts prevents a future allow encoding from being skipped.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;

struct Token(HANDLE);

impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: this owns the successful OpenProcessToken result, closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

#[derive(Default)]
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: GetNamedSecurityInfoW allocates this with LocalAlloc; NULL is also accepted.
        unsafe { LocalFree(self.0) };
    }
}

/// Require process-user ownership and no allow ACE for another principal.
pub(crate) fn check_acl(path: &Path) -> Result<()> {
    inspect_acl(path)
        .with_context(|| format!("could not inspect private ACL at '{}'", path.display()))
}

fn inspect_acl(path: &Path) -> Result<()> {
    let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
    ensure!(!name.contains(&0), "private ACL path contains a NUL");
    name.push(0);

    let mut descriptor = SecurityDescriptor::default();
    let mut owner = null_mut();
    let mut dacl = null_mut();
    // SAFETY: name is NUL-terminated and all requested output pointers are writable.
    // The descriptor guard retains ownership of the returned owner and DACL storage.
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor.0,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32)).context("GetNamedSecurityInfoW");
    }
    ensure!(!descriptor.0.is_null(), "missing security descriptor");
    // SAFETY: the successful OS call supplied this live security descriptor allocation.
    ensure!(
        unsafe { IsValidSecurityDescriptor(descriptor.0) } != 0,
        "invalid private security descriptor"
    );
    ensure!(!owner.is_null(), "private storage has no owner SID");
    // SAFETY: owner is an OS-returned SID inside the still-owned security descriptor.
    ensure!(unsafe { IsValidSid(owner) } != 0, "invalid owner SID");

    let user = process_user()?;
    let current = token_sid(&user)?;
    // SAFETY: both SIDs have been validated and their owning allocations remain alive.
    ensure!(
        unsafe { EqualSid(owner, current) } != 0,
        "unlocked key storage has unexpected ownership"
    );

    // A NULL (including absent) DACL grants everyone access; an empty DACL grants nobody access.
    ensure!(!dacl.is_null(), "private storage has a NULL DACL");
    // SAFETY: dacl points into the live OS-returned descriptor allocation.
    ensure!(unsafe { IsValidAcl(dacl) } != 0, "invalid private ACL");
    // SAFETY: IsValidAcl accepted the OS-owned ACL, including its fixed header.
    let acl = unsafe { *dacl };
    for index in 0..u32::from(acl.AceCount) {
        let mut ace = null_mut();
        // SAFETY: dacl is valid and retained, index is within AceCount, and ace is writable.
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
            return Err(io::Error::last_os_error()).context("GetAce");
        }
        let offset = (ace as usize)
            .checked_sub(dacl as usize)
            .context("ACE precedes its ACL")?;
        let remaining = usize::from(acl.AclSize)
            .checked_sub(offset)
            .context("ACE is outside its ACL")?;
        ensure!(
            !ace.is_null() && offset >= size_of::<ACL>() && remaining >= size_of::<ACE_HEADER>(),
            "invalid ACE header bounds"
        );
        // SAFETY: the entire header is within the retained ACL; unaligned reads avoid assumptions
        // about an unexpected ACE layout before it is rejected below.
        let header = unsafe { read_unaligned(ace.cast::<ACE_HEADER>()) };
        let length = usize::from(header.AceSize);
        ensure!(
            length >= size_of::<ACE_HEADER>() && length <= remaining,
            "invalid ACE bounds"
        );
        match header.AceType {
            ACCESS_ALLOWED_ACE_TYPE | ACCESS_DENIED_ACE_TYPE => {
                // SAFETY: length has been bounded by the live ACL allocation above.
                let bytes = unsafe { slice::from_raw_parts(ace.cast::<u8>(), length) };
                let sid_bytes = bytes
                    .get(offset_of!(ACCESS_ALLOWED_ACE, SidStart)..)
                    .context("ACE has no SID")?;
                // Normal allow and deny ACEs share the same fixed prefix and SID layout.
                let sid = bounded_sid(sid_bytes)?;
                if header.AceType == ACCESS_ALLOWED_ACE_TYPE {
                    // SAFETY: bounded_sid validated the entire ACE SID; current is also valid,
                    // and both owning allocations remain alive throughout the comparison.
                    ensure!(
                        unsafe { EqualSid(sid, current) } != 0,
                        "unlocked key storage is accessible to other users; restrict its permissions"
                    );
                }
            }
            // Reject object, callback, compound, and unknown ACEs rather than accidentally
            // interpreting their variable SID offsets as the canonical initializer's layout.
            kind => bail!("unsupported private ACL ACE type {kind}"),
        }
    }
    Ok(())
}

struct ProcessUser {
    storage: Vec<usize>,
    length: usize,
}

fn process_user() -> Result<ProcessUser> {
    let mut handle = null_mut();
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle; handle is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) } == 0 {
        return Err(io::Error::last_os_error()).context("OpenProcessToken");
    }
    let token = Token(handle);
    let mut required = 0;
    // SAFETY: the token is live; a NULL buffer with zero size probes the required byte count.
    let result = unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut required) };
    let error = io::Error::last_os_error();
    ensure!(
        result == 0 && error.raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER as i32),
        "could not size process TokenUser: {error}"
    );
    ensure!(
        required as usize >= size_of::<TOKEN_USER>(),
        "short TokenUser"
    );
    // usize alignment covers TOKEN_USER's pointer as well as SID's DWORDs on Windows.
    let mut buffer = vec![0usize; (required as usize).div_ceil(size_of::<usize>())];
    let capacity = required;
    // SAFETY: buffer is aligned and writable for at least capacity bytes; token is live.
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            capacity,
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error()).context("GetTokenInformation(TokenUser)");
    }
    ensure!(
        required as usize >= size_of::<TOKEN_USER>() && required <= capacity,
        "invalid TokenUser bounds"
    );
    // Retain the exact returned length so allocation padding cannot complete a truncated SID.
    Ok(ProcessUser {
        storage: buffer,
        length: required as usize,
    })
}

fn token_sid(buffer: &ProcessUser) -> Result<PSID> {
    ensure!(buffer.length >= size_of::<TOKEN_USER>(), "short TokenUser");
    // SAFETY: buffer is initialized, pointer-aligned, and large enough for TOKEN_USER.
    let user = unsafe { &*buffer.storage.as_ptr().cast::<TOKEN_USER>() };
    let offset = (user.User.Sid as usize)
        .checked_sub(buffer.storage.as_ptr() as usize)
        .context("TokenUser SID precedes its buffer")?;
    ensure!(
        offset >= size_of::<TOKEN_USER>(),
        "invalid TokenUser SID offset"
    );
    // SAFETY: the usize slice is fully initialized and retained by the caller.
    let bytes =
        unsafe { slice::from_raw_parts(buffer.storage.as_ptr().cast::<u8>(), buffer.length) };
    bounded_sid(
        bytes
            .get(offset..)
            .context("TokenUser SID outside buffer")?,
    )
}

fn bounded_sid(bytes: &[u8]) -> Result<PSID> {
    // SID has an eight-byte fixed prefix followed by SubAuthorityCount DWORDs.
    // Check this before IsValidSid/EqualSid, which accept no buffer length.
    ensure!(bytes.len() >= 8, "truncated SID header");
    let count = usize::from(bytes[1]);
    ensure!(
        count <= 15 && 8 + count * size_of::<u32>() <= bytes.len(),
        "truncated or oversized SID"
    );
    ensure!(
        (bytes.as_ptr() as usize).is_multiple_of(align_of::<SID>()),
        "unaligned SID"
    );
    let sid = bytes.as_ptr().cast_mut().cast();
    // SAFETY: the full variable-length SID is in bounds and DWORD-aligned; IsValidSid
    // only reads it despite the Win32 API's mutable pointer type.
    ensure!(unsafe { IsValidSid(sid) } != 0, "invalid SID");
    Ok(sid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sid_cannot_claim_subauthorities_outside_its_ace() {
        let words = [0x0000_0201u32, 0, 0];
        // SAFETY: words is initialized and lives through the borrowed byte slice.
        let bytes =
            unsafe { slice::from_raw_parts(words.as_ptr().cast::<u8>(), size_of_val(&words)) };
        assert!(bounded_sid(bytes).is_err());
    }

    #[test]
    fn the_process_user_sid_is_valid_without_a_subprocess() {
        let user = process_user().unwrap();
        assert!(token_sid(&user).is_ok());
    }

    #[test]
    fn a_nul_in_a_path_cannot_inspect_a_different_file() {
        assert!(check_acl(Path::new("ignored\0suffix")).is_err());
    }
}
