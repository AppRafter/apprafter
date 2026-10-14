// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Files and directories only this account can open (WI-472): the one place the CLI, the core
//! and AppRafter Desktop make them owner-only.
//!
//! - On Unix it is a mode: 0600 for a file, given at `open` (never a chmod after its first
//!   byte), and 0700 for a directory, which its callers give.
//! - On Windows it is a protected DACL with one ACE, full access for the process token's user:
//!   `D:P(A;;FA;;;<user SID>)` on a file ([`restrict_file`]) and `D:P(A;OICI;FA;;;<user SID>)`
//!   on a directory ([`restrict_dir`]), whose ACE the files and directories made in it
//!   inherit. A protected DACL takes nothing from its parent, so a store in a folder other
//!   users can read (`APPRAFTER_CONFIG_DIR` on a shared drive) keeps them out too. No SYSTEM or
//!   Administrators ACE: they can read the file either way (ADR 0068 Risks).
//! - A file gets its DACL on its open handle, before anything is written to it. A same-volume
//!   rename (`std::fs::rename`, `MoveFileExW`) keeps it; `ReplaceFileW`, which keeps the
//!   replaced file's DACL, is never used for these files.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// A new file at `path`, open for writing and owner-only before anything is written to it;
/// `AlreadyExists` when the name is taken (never replaced). Unix: mode 0600 at `open`.
/// Windows: opened unshared, with the right to set its DACL, and given the owner-only DACL
/// ([`restrict_file`]) before it is returned; a file that cannot be restricted is removed and
/// the error returned.
#[cfg(unix)]
pub fn create_new(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// A new file at `path`, open for writing and owner-only before anything is written to it;
/// `AlreadyExists` when the name is taken (never replaced). Unix: mode 0600 at `open`.
/// Windows: opened unshared, with the right to set its DACL, and given the owner-only DACL
/// ([`restrict_file`]) before it is returned; a file that cannot be restricted is removed and
/// the error returned.
#[cfg(windows)]
pub fn create_new(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::{READ_CONTROL, WRITE_DAC};
    // No sharing while it is ours, and the right to set its DACL.
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .access_mode(GENERIC_WRITE | WRITE_DAC | READ_CONTROL)
        .open(path)?;
    if let Err(e) = restrict_file(&file) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(file)
}

/// Replaces `file`'s DACL with the owner-only one, protected from inheritance:
/// `D:P(A;;FA;;;<user SID>)`, the analogue of mode 0600. The handle needs `WRITE_DAC`
/// ([`create_new`] opens with it).
#[cfg(windows)]
pub fn restrict_file(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    set_owner_only_dacl(file.as_raw_handle(), 0)
}

/// Replaces the directory `dir`'s DACL with the owner-only one, protected from inheritance,
/// whose ACE the files and directories made in it inherit: `D:P(A;OICI;FA;;;<user SID>)`, the
/// analogue of mode 0700. A reparse point at `dir` is opened itself, never followed.
#[cfg(windows)]
pub fn restrict_dir(dir: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Security::{CONTAINER_INHERIT_ACE, OBJECT_INHERIT_ACE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, READ_CONTROL, WRITE_DAC,
    };
    // A directory opens only with backup semantics.
    let handle = OpenOptions::new()
        .access_mode(WRITE_DAC | READ_CONTROL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir)?;
    set_owner_only_dacl(
        handle.as_raw_handle(),
        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
    )
}

/// Sets the protected one-ACE DACL on the object behind `handle`: `FILE_ALL_ACCESS` for the
/// process token's user, with `ace_flags` (0 for a file, the inherit flags for a directory).
#[cfg(windows)]
fn set_owner_only_dacl(handle: std::os::windows::io::RawHandle, ace_flags: u32) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{SetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        AddAccessAllowedAceEx, GetLengthSid, InitializeAcl, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION,
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    let user = CurrentUser::current()?;
    // SAFETY: the SID lives in `user`'s buffer for as long as `user` does.
    let sid_len = unsafe { GetLengthSid(user.sid()) } as usize;
    let acl_len = std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>()
        - std::mem::size_of::<u32>()
        + sid_len;
    // u64s keep the ACL DWORD-aligned.
    let mut acl_buf = vec![0u64; acl_len.div_ceil(8)];
    let acl = acl_buf.as_mut_ptr().cast::<ACL>();
    // SAFETY: `acl` points at `acl_len` writable, aligned bytes that outlive the calls below.
    if unsafe { InitializeAcl(acl, acl_len as u32, ACL_REVISION) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above; the SID is valid (`user`).
    if unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, ace_flags, FILE_ALL_ACCESS, user.sid()) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `handle` is an open file or directory handle with WRITE_DAC; owner and group are
    // left as they are (null), and `acl` is the DACL built above.
    let status = unsafe {
        SetSecurityInfo(
            handle as HANDLE,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// The process token's `TOKEN_USER`, in a buffer that owns the SID it points to.
#[cfg(windows)]
struct CurrentUser {
    buf: Vec<u64>,
}

#[cfg(windows)]
impl CurrentUser {
    fn current() -> io::Result<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::TOKEN_QUERY;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the current-process pseudo handle; `token` receives a new handle, closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let user = Self::of_token(token);
        // SAFETY: `token` was opened above and is closed once, here.
        unsafe { CloseHandle(token) };
        user
    }

    fn of_token(token: windows_sys::Win32::Foundation::HANDLE) -> io::Result<Self> {
        use windows_sys::Win32::Security::{GetTokenInformation, TokenUser};
        let mut len = 0u32;
        // SAFETY: a size query: no buffer; `len` receives the size needed.
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` holds at least `len` writable bytes, 8-aligned.
        let ok = unsafe {
            GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(CurrentUser { buf })
    }

    fn sid(&self) -> windows_sys::Win32::Security::PSID {
        // SAFETY: `buf` holds the TOKEN_USER that GetTokenInformation wrote.
        unsafe {
            (*self
                .buf
                .as_ptr()
                .cast::<windows_sys::Win32::Security::TOKEN_USER>())
            .User
            .Sid
        }
    }
}

/// Reading a DACL back, for the tests of every owner-only file and directory (this crate's,
/// and through the `test-support` feature the core's and the state's). Never in a shipped
/// binary: `test-support` is enabled only from `[dev-dependencies]`.
#[cfg(all(windows, any(test, feature = "test-support")))]
pub mod testing {
    use std::cell::RefCell;
    use std::fs::File;
    use std::io;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS};
    use windows_sys::Win32::Security::{
        AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, CONTAINER_INHERIT_ACE,
        OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    /// `ACCESS_ALLOWED_ACE_TYPE` (winnt.h; windows-sys has it only under SystemServices).
    const ACCESS_ALLOWED: u8 = 0;

    /// A DACL as the OS reports it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Dacl {
        /// `SE_DACL_PROTECTED`: it takes no ACE from its parent.
        pub protected: bool,
        pub aces: Vec<Ace>,
    }

    /// One ACE of a [`Dacl`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Ace {
        /// The ACE type (`ACCESS_ALLOWED_ACE_TYPE` is 0).
        pub kind: u8,
        /// The inheritance flags (`OBJECT_INHERIT_ACE`, `CONTAINER_INHERIT_ACE`,
        /// `INHERITED_ACE`, …).
        pub flags: u8,
        /// The access mask; only meaningful for the access-allowed and -denied types.
        pub mask: u32,
        /// Whether the ACE's SID is the process token's user; only for those types too.
        pub for_this_user: bool,
    }

    impl Dacl {
        /// Panics unless this is a file's owner-only DACL: protected, exactly one ACE,
        /// access-allowed, `FILE_ALL_ACCESS`, for the process token's user, inheriting nothing
        /// to anything.
        #[track_caller]
        pub fn assert_owner_only_file(&self, what: &str) {
            self.assert_owner_only(what, 0);
        }

        /// Panics unless this is a directory's owner-only DACL: as a file's, with the one ACE
        /// inherited by the files and directories made in it (`OBJECT_INHERIT_ACE |
        /// CONTAINER_INHERIT_ACE`, nothing else).
        #[track_caller]
        pub fn assert_owner_only_dir(&self, what: &str) {
            self.assert_owner_only(what, (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) as u8);
        }

        #[track_caller]
        fn assert_owner_only(&self, what: &str, flags: u8) {
            assert!(
                self.protected,
                "{what}: the DACL inherits from its parent: {self:?}"
            );
            assert_eq!(self.aces.len(), 1, "{what}: not exactly one ACE: {self:?}");
            let ace = &self.aces[0];
            assert_eq!(
                ace.kind, ACCESS_ALLOWED,
                "{what}: not an access-allowed ACE: {self:?}"
            );
            assert_eq!(
                ace.mask, FILE_ALL_ACCESS,
                "{what}: not full access: {self:?}"
            );
            assert!(
                ace.for_this_user,
                "{what}: the ACE is for another SID: {self:?}"
            );
            assert_eq!(
                ace.flags, flags,
                "{what}: the ACE's inheritance flags: {self:?}"
            );
        }
    }

    /// The DACL of the file or directory at `path`.
    pub fn of_path(path: &Path) -> io::Result<Dacl> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a NUL-terminated path; the out-pointers are valid; `descriptor` is freed by
        // `read`.
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        read(descriptor, dacl)
    }

    /// The DACL of the open `file`, read through its handle (which needs `READ_CONTROL`;
    /// every handle opened for reading or writing has it).
    pub fn of_file(file: &File) -> io::Result<Dacl> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: an open handle; the out-pointers are valid; `descriptor` is freed by `read`.
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        read(descriptor, dacl)
    }

    /// The [`Dacl`] in `descriptor` (whose DACL is `dacl`); frees `descriptor`.
    fn read(descriptor: PSECURITY_DESCRIPTOR, dacl: *mut ACL) -> io::Result<Dacl> {
        let result = (|| {
            let (mut control, mut revision) = (0u16, 0u32);
            // SAFETY: `descriptor` came from Get(Named)SecurityInfo.
            if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let protected = control & SE_DACL_PROTECTED != 0;
            if dacl.is_null() {
                // A NULL DACL grants everyone everything; no ACE to report.
                return Ok(Dacl {
                    protected,
                    aces: Vec::new(),
                });
            }
            let mut info = ACL_SIZE_INFORMATION {
                AceCount: 0,
                AclBytesInUse: 0,
                AclBytesFree: 0,
            };
            // SAFETY: `dacl` points into `descriptor`; `info` is the size class's struct.
            if unsafe {
                GetAclInformation(
                    dacl,
                    (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
                    std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let user = super::CurrentUser::current()?;
            let mut aces = Vec::new();
            for index in 0..info.AceCount {
                let mut ace = std::ptr::null_mut();
                // SAFETY: `index` is below AceCount.
                if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: every ACE starts with an ACE_HEADER.
                let header = unsafe { *ace.cast::<ACE_HEADER>() };
                let (mask, for_this_user) =
                    if header.AceType == ACCESS_ALLOWED || header.AceType == 1 {
                        // Access-allowed (0) and access-denied (1) share
                        // ACCESS_ALLOWED_ACE's layout.
                        let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
                        // SAFETY: an ACE of that layout, told by its type byte.
                        unsafe {
                            let sid: PSID = std::ptr::addr_of_mut!((*ace).SidStart).cast();
                            ((*ace).Mask, EqualSid(sid, user.sid()) != 0)
                        }
                    } else {
                        (0, false)
                    };
                aces.push(Ace {
                    kind: header.AceType,
                    flags: header.AceFlags,
                    mask,
                    for_this_user,
                });
            }
            Ok(Dacl { protected, aces })
        })();
        // SAFETY: `descriptor` was allocated by Get(Named)SecurityInfo and is freed once, here.
        unsafe { LocalFree(descriptor) };
        result
    }

    thread_local! {
        static BEFORE_FIRST_BYTE: RefCell<Option<io::Result<Dacl>>> = const { RefCell::new(None) };
    }

    /// Records `file`'s DACL as it is now: a writer of an owner-only file calls it, in its
    /// tests only, between opening the file and writing its first byte, so a test can tell a
    /// DACL set before the content from one set after it (both read back the same at the end).
    pub fn record_before_first_byte(file: &File) {
        let dacl = of_file(file);
        BEFORE_FIRST_BYTE.with(|slot| *slot.borrow_mut() = Some(dacl));
    }

    /// The DACL this thread's last [`record_before_first_byte`] read, taken; `None` when
    /// nothing recorded one since the last take.
    pub fn take_before_first_byte() -> Option<io::Result<Dacl>> {
        BEFORE_FIRST_BYTE.with(|slot| slot.borrow_mut().take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_new_file_is_created_once_and_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        let mut file = create_new(&path).unwrap();
        file.write_all(b"first").unwrap();
        drop(file);
        let again = create_new(&path).unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_file_is_0600_from_its_open() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = create_new(&dir.path().join("secret")).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }

    /// Read through the handle create_new returns, before any byte is written, and again by
    /// path once it is closed.
    #[cfg(windows)]
    #[test]
    fn a_new_file_has_the_owner_only_dacl_before_its_first_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        let mut file = create_new(&path).unwrap();
        testing::of_file(&file)
            .unwrap()
            .assert_owner_only_file("a new file, before its first byte");
        file.write_all(b"secret").unwrap();
        drop(file);
        testing::of_path(&path)
            .unwrap()
            .assert_owner_only_file("a new file, closed");
    }

    /// The directory's one ACE is inherited by a file made in it with no DACL of its own, and
    /// restricting a directory that already exists replaces what it inherited.
    #[cfg(windows)]
    #[test]
    fn a_restricted_directory_hands_its_one_ace_to_what_is_made_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        std::fs::create_dir(&private).unwrap();
        assert!(
            !testing::of_path(&private).unwrap().protected,
            "a new directory inherits (the test's premise)"
        );
        restrict_dir(&private).unwrap();
        testing::of_path(&private)
            .unwrap()
            .assert_owner_only_dir("the restricted directory");
        let plain = private.join("plain");
        std::fs::write(&plain, b"x").unwrap();
        let inherited = testing::of_path(&plain).unwrap();
        assert_eq!(inherited.aces.len(), 1, "{inherited:?}");
        assert!(inherited.aces[0].for_this_user, "{inherited:?}");
        restrict_dir(&private).unwrap();
        testing::of_path(&private)
            .unwrap()
            .assert_owner_only_dir("the directory restricted again");
    }

    /// The assertions themselves tell the other DACLs apart: a file that inherits (no
    /// restriction), and a directory's DACL read as a file's and the other way round.
    #[cfg(windows)]
    #[test]
    fn the_dacl_assertions_refuse_an_inherited_dacl_and_the_wrong_inheritance() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        std::fs::write(&plain, b"x").unwrap();
        let inherited = testing::of_path(&plain).unwrap();
        let refused =
            |f: &dyn Fn()| std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err();
        assert!(refused(&|| inherited.assert_owner_only_file("plain")));
        let file = dir.path().join("restricted");
        drop(create_new(&file).unwrap());
        let file_dacl = testing::of_path(&file).unwrap();
        assert!(refused(
            &|| file_dacl.assert_owner_only_dir("a file read as a directory")
        ));
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        restrict_dir(&sub).unwrap();
        let dir_dacl = testing::of_path(&sub).unwrap();
        assert!(refused(
            &|| dir_dacl.assert_owner_only_file("a directory read as a file")
        ));
    }
}
