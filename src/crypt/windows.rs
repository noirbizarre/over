//! Native Windows access control keeps filesystem operations independent of shell startup.

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
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS,
    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION,
    EqualSid, GROUP_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorControl,
    GetTokenInformation, IsValidAcl, IsValidSecurityDescriptor, IsValidSid, OBJECT_INHERIT_ACE,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED, SID, SetFileSecurityW, TOKEN_QUERY, TOKEN_USER, TokenUser,
    UNPROTECTED_DACL_SECURITY_INFORMATION,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

// The SDK exposes these tags under SystemServices, outside our required feature set.
// Whitelisting only these layouts prevents a future allow encoding from being skipped.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;

// FILE_ALL_ACCESS is shared by files and directories; avoid another SDK feature for one mask.
const FILE_ALL_ACCESS: u32 = 0x001f_01ff;

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
        // SAFETY: security descriptor APIs allocate this with LocalAlloc; NULL is also accepted.
        unsafe { LocalFree(self.0) };
    }
}

struct OwnedAcl(*mut ACL);

impl Drop for OwnedAcl {
    fn drop(&mut self) {
        // SAFETY: SetEntriesInAclW allocates this with LocalAlloc, released exactly once.
        unsafe { LocalFree(self.0.cast()) };
    }
}

fn path_name(path: &Path) -> Result<Vec<u16>> {
    let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
    ensure!(!name.contains(&0), "ACL path contains a NUL");
    name.push(0);
    Ok(name)
}

/// Replace inherited/broad permissions with a protected, current-user-only policy.
pub(crate) fn initialize_acl(path: &Path) -> Result<()> {
    initialize_private_acl(path)
        .with_context(|| format!("could not initialize private ACL at '{}'", path.display()))
}

fn initialize_private_acl(path: &Path) -> Result<()> {
    let name = path_name(path)?;
    let user = process_user()?;
    let sid = token_sid(&user)?;
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        // Private directories must also protect subsequently created key and journal files.
        grfInheritance: if path.metadata()?.is_dir() {
            CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE
        } else {
            0
        },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: sid.cast(),
        },
    };
    let mut acl = OwnedAcl(null_mut());
    // SAFETY: entry's SID remains valid through the call, and acl is a writable output.
    // Starting from NULL intentionally drops all previous explicit and inherited grants.
    let status = unsafe { SetEntriesInAclW(1, &entry, null_mut(), &mut acl.0) };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32)).context("SetEntriesInAclW");
    }
    // SAFETY: name is NUL-terminated, sid and acl remain owned until the call completes.
    let status = unsafe {
        SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            sid,
            null_mut(),
            acl.0,
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32)).context("SetNamedSecurityInfoW");
    }
    Ok(())
}

/// Preserve worktree permissions without imposing private-key policy on public files.
pub(crate) fn copy_acl(source: &Path, destination: &Path) -> Result<()> {
    copy_access_control(source, destination).with_context(|| {
        format!(
            "could not preserve worktree access control from '{}' at '{}'",
            source.display(),
            destination.display()
        )
    })
}

fn copy_access_control(source: &Path, destination: &Path) -> Result<()> {
    let source = path_name(source)?;
    let destination = path_name(destination)?;
    let mut descriptor = SecurityDescriptor::default();
    let information =
        OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    // SAFETY: source is NUL-terminated; only descriptor is requested and it is writable.
    // Its owner/group/DACL storage is kept alive through SetFileSecurityW below.
    let status = unsafe {
        GetNamedSecurityInfoW(
            source.as_ptr(),
            SE_FILE_OBJECT,
            information,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor.0,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32)).context("GetNamedSecurityInfoW");
    }
    let control = descriptor_control(&descriptor)?;
    let protection = if control & SE_DACL_PROTECTED != 0 {
        PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        UNPROTECTED_DACL_SECURITY_INFORMATION
    };
    // SAFETY: destination is NUL-terminated; descriptor is an OS-allocated, validated descriptor.
    // Unlike SetNamedSecurityInfoW, this copies inherited ACEs without merging permissions
    // from the private staging directory, which is not the file's eventual parent.
    if unsafe { SetFileSecurityW(destination.as_ptr(), information | protection, descriptor.0) }
        == 0
    {
        return Err(io::Error::last_os_error()).context("SetFileSecurityW");
    }
    Ok(())
}

fn descriptor_control(descriptor: &SecurityDescriptor) -> Result<u16> {
    ensure!(!descriptor.0.is_null(), "missing security descriptor");
    // SAFETY: descriptor is an OS-returned allocation retained by its guard.
    ensure!(
        unsafe { IsValidSecurityDescriptor(descriptor.0) } != 0,
        "invalid security descriptor"
    );
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: the descriptor is valid and both output pointers are writable.
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error()).context("GetSecurityDescriptorControl");
    }
    Ok(control)
}

/// Require process-user ownership and no allow ACE for another principal.
pub(crate) fn check_acl(path: &Path) -> Result<()> {
    inspect_acl(path)
        .with_context(|| format!("could not inspect private ACL at '{}'", path.display()))
}

fn inspect_acl(path: &Path) -> Result<()> {
    let name = path_name(path)?;

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
    use std::fs;

    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };

    use super::*;

    fn security_descriptor(path: &Path) -> SecurityDescriptor {
        let name = path_name(path).unwrap();
        let mut descriptor = SecurityDescriptor::default();
        // SAFETY: name is terminated and descriptor is writable, owning the returned allocation.
        let status = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut descriptor.0,
            )
        };
        assert_eq!(status, ERROR_SUCCESS);
        descriptor
    }

    fn access_policy(path: &Path) -> String {
        let descriptor = security_descriptor(path);
        let mut text = null_mut();
        let mut length = 0;
        // SAFETY: descriptor is live; text and length are writable outputs for the SDDL allocation.
        assert_ne!(
            unsafe {
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor.0,
                    SDDL_REVISION_1,
                    OWNER_SECURITY_INFORMATION
                        | GROUP_SECURITY_INFORMATION
                        | DACL_SECURITY_INFORMATION,
                    &mut text,
                    &mut length,
                )
            },
            0
        );
        // SAFETY: the successful conversion returns length UTF-16 units, including the NUL.
        let policy = unsafe { slice::from_raw_parts(text, length as usize - 1) };
        let result = String::from_utf16(policy).unwrap();
        // SAFETY: the conversion allocated text with LocalAlloc; no borrowed view is used again.
        unsafe { LocalFree(text.cast()) };
        result
    }

    fn set_dacl(path: &Path, sddl: &str) {
        let text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor = SecurityDescriptor::default();
        // SAFETY: text is terminated and descriptor owns the successful conversion's allocation.
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor.0,
                    null_mut(),
                )
            },
            0
        );
        let name = path_name(path).unwrap();
        let protection = if descriptor_control(&descriptor).unwrap() & SE_DACL_PROTECTED != 0 {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
        // SAFETY: name is terminated and descriptor remains live through the call.
        assert_ne!(
            unsafe {
                SetFileSecurityW(
                    name.as_ptr(),
                    DACL_SECURITY_INFORMATION | protection,
                    descriptor.0,
                )
            },
            0
        );
    }

    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    fn private_initialization_replaces_broad_grants_with_user_only_full_control(
        #[case] directory: bool,
    ) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private-秘密");
        if directory {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, b"secret").unwrap();
        }
        // Everyone includes the test user, so the broad policy remains writable before repair.
        set_dacl(&path, "D:P(A;;FA;;;WD)");
        assert!(check_acl(&path).is_err());

        initialize_acl(&path).unwrap();

        check_acl(&path).unwrap();
        let descriptor = security_descriptor(&path);
        assert_ne!(
            descriptor_control(&descriptor).unwrap() & SE_DACL_PROTECTED,
            0
        );
        let mut dacl = null_mut();
        let mut present = 0;
        let mut defaulted = 0;
        // SAFETY: descriptor is live and all outputs are writable.
        assert_ne!(
            unsafe {
                windows_sys::Win32::Security::GetSecurityDescriptorDacl(
                    descriptor.0,
                    &mut present,
                    &mut dacl,
                    &mut defaulted,
                )
            },
            0
        );
        assert_ne!(present, 0);
        assert!(!dacl.is_null());
        // SAFETY: this DACL comes from the live, OS-produced descriptor.
        assert_eq!(unsafe { (*dacl).AceCount }, 1);
        let mut ace = null_mut();
        // SAFETY: the DACL has exactly one ACE and ace is a writable output.
        assert_ne!(unsafe { GetAce(dacl, 0, &mut ace) }, 0);
        // SAFETY: the initializer creates one normal ACCESS_ALLOWED_ACE, retained by descriptor.
        let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        assert_eq!(ace.Mask, FILE_ALL_ACCESS);
        assert_eq!(
            u32::from(ace.Header.AceFlags),
            if directory {
                CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE
            } else {
                0
            }
        );
    }

    #[test]
    fn private_directories_pass_user_only_access_to_nested_children() {
        let root = tempfile::tempdir().unwrap();
        initialize_acl(root.path()).unwrap();
        let nested = root.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let file = nested.join("key");
        fs::write(&file, b"secret").unwrap();
        check_acl(&nested).unwrap();
        check_acl(&file).unwrap();
        // Reading and writing must remain possible, not merely an empty (deny-all) DACL.
        fs::write(&file, b"updated secret").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"updated secret");
    }

    #[test]
    fn new_worktree_plaintext_does_not_inherit_a_public_parents_grants() {
        let root = tempfile::tempdir().unwrap();
        set_dacl(root.path(), "D:P(A;OICI;FA;;;WD)");
        let path = root.path().join("new-plaintext");

        super::super::transaction::worktree_write(&path, b"secret", None).unwrap();

        check_acl(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"secret");
    }

    #[rstest::rstest]
    #[case(false)]
    #[case(true)]
    fn worktree_publication_preserves_custom_rules_and_inheritance(#[case] protected: bool) {
        let root = tempfile::tempdir().unwrap();
        // Anonymous-write denial exercises non-allow ACEs without denying the test user writes.
        set_dacl(
            root.path(),
            "D:P(D;OICI;FW;;;AN)(A;OICI;FA;;;OW)(A;OICI;FR;;;WD)",
        );
        let path = root.path().join("worktree-秘密");
        fs::write(&path, b"original").unwrap();
        if protected {
            set_dacl(&path, "D:P(D;;FW;;;AN)(A;;FA;;;OW)(A;;FR;;;WD)");
        }
        let before = access_policy(&path);
        let descriptor = security_descriptor(&path);
        assert_eq!(
            descriptor_control(&descriptor).unwrap() & SE_DACL_PROTECTED != 0,
            protected
        );
        assert!(check_acl(&path).is_err());

        // Exercise the actual private staging directory and atomic replacement, not just copying.
        super::super::transaction::worktree_write(&path, b"replacement", None).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        assert_eq!(access_policy(&path), before);
    }

    #[test]
    fn nul_paths_cannot_initialize_or_copy_a_different_files_permissions() {
        assert!(initialize_acl(Path::new("ignored\0suffix")).is_err());
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, b"source").unwrap();
        assert!(copy_acl(&source, Path::new("ignored\0suffix")).is_err());
        assert!(copy_acl(Path::new("ignored\0suffix"), &source).is_err());
    }

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
