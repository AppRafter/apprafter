// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Files and directories only this account can open (WI-472, WI-473): the one place the CLI,
//! the core and AppRafter Desktop make them owner-only, tighten them and check them.
//!
//! - On Unix it is a mode: 0600 for a file, given at `open` (never a chmod after its first
//!   byte), and 0700 for a directory.
//! - On Windows it is a protected DACL with one ACE, full access for the process token's user:
//!   `D:P(A;;FA;;;<user SID>)` on a file ([`restrict_file`]) and `D:P(A;OICI;FA;;;<user SID>)`
//!   on a directory ([`restrict_dir`], [`create_dir`]), whose ACE the files and directories
//!   made in it inherit. A protected DACL takes nothing from its parent, so a store in a folder
//!   other users can read keeps them out too. No SYSTEM or Administrators ACE: they can read the
//!   file either way (ADR 0068 Risks).
//! - A file gets its DACL on its open handle, before anything is written to it, and a directory
//!   at its creation. A same-volume rename (`std::fs::rename`, `MoveFileExW`) keeps it;
//!   `ReplaceFileW`, which keeps the replaced file's DACL, is never used for these files.
//! - A secret file that is already there is tightened when it is read (WI-473, Windows):
//!   [`open_secret`] gives a file whose DACL is not owner-only — written before WI-472, or
//!   widened since — the owner-only DACL on the handle it reads through. A file whose DACL this
//!   account may not set is read as it is, and doctor reports it ([`exposure`]); doctor itself
//!   changes no file ([`without_tightening`]). A Unix mode is given when the file is written
//!   and never changed by a read.
//! - The files the CLI hands to kubectl and helm in the system temp dir ([`temp_file`],
//!   [`write_temp_file`]) are owner-only from their first byte as well, whatever the temp dir
//!   lets others do, and other processes of this account can read them while the CLI holds
//!   them; a directory there ([`temp_dir`]) is owner-only from its creation.
//! - A directory another account owns is never restricted or used ([`restrict_dir`]): its owner
//!   can grant itself access again whatever DACL is set on it.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// A new file at `path`, open for writing and owner-only before anything is written to it;
/// `AlreadyExists` when the name is taken (never replaced). Unix: mode 0600 at `open`.
/// Windows: opened with the right to set its DACL, and given the owner-only DACL
/// ([`restrict_file`]) before it is returned; a file that cannot be restricted is removed and
/// the error returned. While it is held no one else opens it, to read or to write: the open
/// shares only removal by name, which tempfile's cleanup needs when a write fails (it removes
/// the file before it closes the handle).
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
/// Windows: opened with the right to set its DACL, and given the owner-only DACL
/// ([`restrict_file`]) before it is returned; a file that cannot be restricted is removed and
/// the error returned. While it is held no one else opens it, to read or to write: the open
/// shares only removal by name, which tempfile's cleanup needs when a write fails (it removes
/// the file before it closes the handle).
#[cfg(windows)]
pub fn create_new(path: &Path) -> io::Result<File> {
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;
    // No reader and no other writer while it is ours; removal by name, for tempfile's cleanup.
    create_new_sharing(path, FILE_SHARE_DELETE)
}

/// [`create_new`], for a file that another process of this account reads while this one still
/// holds it: kubectl or helm, handed a manifest or a kubeconfig. Unix: [`create_new`] itself.
/// Windows: the open shares reading, and removal by name (tempfile's cleanup when it drops, a
/// second Ctrl-C), and nothing else; the DACL keeps every other account out.
#[cfg(unix)]
pub fn create_new_shared(path: &Path) -> io::Result<File> {
    create_new(path)
}

/// [`create_new`], for a file that another process of this account reads while this one still
/// holds it: kubectl or helm, handed a manifest or a kubeconfig. Unix: [`create_new`] itself.
/// Windows: the open shares reading, and removal by name (tempfile's cleanup when it drops, a
/// second Ctrl-C), and nothing else; the DACL keeps every other account out.
#[cfg(windows)]
pub fn create_new_shared(path: &Path) -> io::Result<File> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};
    create_new_sharing(path, FILE_SHARE_READ | FILE_SHARE_DELETE)
}

/// [`create_new`] on Windows, opened with the share mode `share`.
#[cfg(windows)]
fn create_new_sharing(path: &Path, share: u32) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Foundation::GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::{READ_CONTROL, WRITE_DAC};
    // The right to set its DACL.
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(share)
        .access_mode(GENERIC_WRITE | WRITE_DAC | READ_CONTROL)
        .open(path)?;
    if let Err(e) = restrict_file(&file) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(file)
}

/// A new directory at `path`, owner-only from its creation; `AlreadyExists` when the name is
/// taken. Unix: mode 0700 at `mkdir`. Windows: created with the protected DACL whose one ACE
/// the files and directories made in it inherit, so it never has another, not for a moment.
#[cfg(unix)]
pub fn create_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

/// A new directory at `path`, owner-only from its creation; `AlreadyExists` when the name is
/// taken. Unix: mode 0700 at `mkdir`. Windows: created with the protected DACL whose one ACE
/// the files and directories made in it inherit, so it never has another, not for a moment.
#[cfg(windows)]
pub fn create_dir(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::{
        InitializeSecurityDescriptor, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
        CONTAINER_INHERIT_ACE, OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        SECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
    };
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    /// `SECURITY_DESCRIPTOR_REVISION` (winnt.h; windows-sys has it only under SystemServices).
    const REVISION: u32 = 1;

    let acl = OwnerOnlyAcl::new(OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE)?;
    // SAFETY: SECURITY_DESCRIPTOR is plain data; InitializeSecurityDescriptor sets every field.
    let mut descriptor: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
    let sd: PSECURITY_DESCRIPTOR = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
    // SAFETY: `sd` points at a SECURITY_DESCRIPTOR that outlives every call below; the ACL it
    // is given lives in `acl`, which outlives the CreateDirectoryW call.
    unsafe {
        if InitializeSecurityDescriptor(sd, REVISION) == 0
            || SetSecurityDescriptorDacl(sd, 1, acl.as_ptr(), 0) == 0
            || SetSecurityDescriptorControl(sd, SE_DACL_PROTECTED, SE_DACL_PROTECTED) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd,
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: a NUL-terminated path, and the attributes built above.
    if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Replaces `file`'s DACL with the owner-only one, protected from inheritance:
/// `D:P(A;;FA;;;<user SID>)`, the analogue of mode 0600. The handle needs `WRITE_DAC`
/// ([`create_new`] opens with it).
#[cfg(windows)]
pub fn restrict_file(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    set_owner_only_dacl(file.as_raw_handle(), 0)
}

/// Why [`restrict_dir`] set no DACL.
#[cfg(windows)]
#[derive(Debug)]
pub enum RestrictDirError {
    /// Another account owns the directory. Its owner can grant itself access again whatever
    /// DACL is set, so none is.
    ForeignOwner,
    Io(io::Error),
}

#[cfg(windows)]
impl From<io::Error> for RestrictDirError {
    fn from(e: io::Error) -> Self {
        RestrictDirError::Io(e)
    }
}

#[cfg(windows)]
impl std::fmt::Display for RestrictDirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestrictDirError::ForeignOwner => f.write_str("another account owns the directory"),
            RestrictDirError::Io(e) => e.fmt(f),
        }
    }
}

#[cfg(windows)]
impl std::error::Error for RestrictDirError {}

/// Replaces the directory `dir`'s DACL with the owner-only one, protected from inheritance,
/// whose ACE the files and directories made in it inherit: `D:P(A;OICI;FA;;;<user SID>)`, the
/// analogue of mode 0700. A reparse point at `dir` is opened itself, never followed. A
/// directory another account owns ([`trusted_owner`]) is left as it is:
/// [`RestrictDirError::ForeignOwner`], whether or not its DACL lets this account set another.
#[cfg(windows)]
pub fn restrict_dir(dir: &Path) -> Result<(), RestrictDirError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Security::{CONTAINER_INHERIT_ACE, OBJECT_INHERIT_ACE};
    use windows_sys::Win32::Storage::FileSystem::{READ_CONTROL, WRITE_DAC};
    let handle = match open_dir(dir, WRITE_DAC | READ_CONTROL) {
        Ok(handle) => handle,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            // Not this account's DACL to set: say whose directory it is, when that can be read.
            if !trusted_owner(&open_dir(dir, READ_CONTROL)?)? {
                return Err(RestrictDirError::ForeignOwner);
            }
            return Err(RestrictDirError::Io(e));
        }
        Err(e) => return Err(RestrictDirError::Io(e)),
    };
    if !trusted_owner(&handle)? {
        return Err(RestrictDirError::ForeignOwner);
    }
    set_owner_only_dacl(
        handle.as_raw_handle(),
        OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
    )?;
    Ok(())
}

/// The directory `dir` itself, opened with `access`: with backup semantics (a directory opens
/// only so), and a reparse point is not followed.
#[cfg(windows)]
fn open_dir(dir: &Path, access: u32) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    OpenOptions::new()
        .access_mode(access)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir)
}

/// Whether the object behind `file` (opened with `READ_CONTROL`) is owned here: by the process
/// token's user, by the owner the token gives what it creates (the Administrators group in an
/// elevated process), or by LocalSystem or the Administrators group, who can open any file on
/// this computer anyway (ADR 0068 Risks). Any other owner can grant itself access to it.
#[cfg(windows)]
fn trusted_owner(file: &File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: an open handle with READ_CONTROL; the out-pointers are valid; `descriptor` is
    // freed below, after the last use of `owner`, which points into it.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    // Tests: another account's directory, without a second account or a system directory.
    #[cfg(any(test, feature = "test-support"))]
    let pretended = testing::pretended_owner();
    #[cfg(any(test, feature = "test-support"))]
    let owner: PSID = match &pretended {
        Some(sid) => sid.as_ptr().cast_mut().cast(),
        None => owner,
    };
    let trusted = owner_is_trusted(owner);
    // SAFETY: allocated by GetSecurityInfo, freed once, here.
    unsafe { LocalFree(descriptor) };
    trusted
}

/// [`trusted_owner`]'s judgement of the SID `owner`.
#[cfg(windows)]
fn owner_is_trusted(owner: windows_sys::Win32::Security::PSID) -> io::Result<bool> {
    use windows_sys::Win32::Security::{
        CreateWellKnownSid, EqualSid, TokenOwner, TokenUser, WinBuiltinAdministratorsSid,
        WinLocalSystemSid, TOKEN_OWNER, TOKEN_USER,
    };
    if owner.is_null() {
        return Ok(false);
    }
    let user = token_information(TokenUser)?;
    // SAFETY: the buffer holds the TOKEN_USER that GetTokenInformation wrote.
    let user_sid = unsafe { (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let default_owner = token_information(TokenOwner)?;
    // SAFETY: the buffer holds the TOKEN_OWNER that GetTokenInformation wrote.
    let default_owner_sid = unsafe { (*default_owner.as_ptr().cast::<TOKEN_OWNER>()).Owner };
    // SAFETY: valid SIDs: `owner` from the caller, the others in the buffers above.
    if unsafe { EqualSid(owner, user_sid) != 0 || EqualSid(owner, default_owner_sid) != 0 } {
        return Ok(true);
    }
    for kind in [WinBuiltinAdministratorsSid, WinLocalSystemSid] {
        // SECURITY_MAX_SID_SIZE (68) bytes, 8-aligned.
        let mut sid = [0u64; 9];
        let mut len = std::mem::size_of_val(&sid) as u32;
        // SAFETY: `sid` holds `len` writable bytes; no domain SID for these two.
        if unsafe {
            CreateWellKnownSid(
                kind,
                std::ptr::null_mut(),
                sid.as_mut_ptr().cast(),
                &mut len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both are valid SIDs.
        if unsafe { EqualSid(owner, sid.as_mut_ptr().cast()) } != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Sets the protected one-ACE DACL on the object behind `handle`: `FILE_ALL_ACCESS` for the
/// process token's user, with `ace_flags` (0 for a file, the inherit flags for a directory).
#[cfg(windows)]
fn set_owner_only_dacl(handle: std::os::windows::io::RawHandle, ace_flags: u32) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{SetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    };

    let acl = OwnerOnlyAcl::new(ace_flags)?;
    // SAFETY: `handle` is an open file or directory handle with WRITE_DAC; owner and group are
    // left as they are (null), and the DACL is the one in `acl`.
    let status = unsafe {
        SetSecurityInfo(
            handle as HANDLE,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl.as_ptr(),
            std::ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// The owner-only ACL, in a buffer that owns it: one access-allowed ACE, `FILE_ALL_ACCESS` for
/// the process token's user, with the ACE flags given (0 for a file, the inherit flags for a
/// directory).
#[cfg(windows)]
struct OwnerOnlyAcl {
    buf: Vec<u64>,
}

#[cfg(windows)]
impl OwnerOnlyAcl {
    fn new(ace_flags: u32) -> io::Result<Self> {
        use windows_sys::Win32::Security::{
            AddAccessAllowedAceEx, GetLengthSid, InitializeAcl, ACCESS_ALLOWED_ACE, ACL,
            ACL_REVISION,
        };
        use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

        let user = CurrentUser::current()?;
        // SAFETY: the SID lives in `user`'s buffer for as long as `user` does.
        let sid_len = unsafe { GetLengthSid(user.sid()) } as usize;
        let acl_len = std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>()
            - std::mem::size_of::<u32>()
            + sid_len;
        // u64s keep the ACL DWORD-aligned.
        let mut buf = vec![0u64; acl_len.div_ceil(8)];
        let acl = buf.as_mut_ptr().cast::<ACL>();
        // SAFETY: `acl` points at `acl_len` writable, aligned bytes that outlive the calls below.
        if unsafe { InitializeAcl(acl, acl_len as u32, ACL_REVISION) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as above; the SID is valid (`user`), and copied into the ACE.
        if unsafe {
            AddAccessAllowedAceEx(acl, ACL_REVISION, ace_flags, FILE_ALL_ACCESS, user.sid())
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnerOnlyAcl { buf })
    }

    fn as_ptr(&self) -> *const windows_sys::Win32::Security::ACL {
        self.buf.as_ptr().cast()
    }
}

/// The process token's information of `class`, in a buffer that owns it (and whatever SIDs it
/// points to), 8-aligned.
#[cfg(windows)]
fn token_information(
    class: windows_sys::Win32::Security::TOKEN_INFORMATION_CLASS,
) -> io::Result<Vec<u64>> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the current-process pseudo handle; `token` receives a new handle, closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let read = || {
        let mut len = 0u32;
        // SAFETY: a size query: no buffer; `len` receives the size needed.
        unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` holds at least `len` writable bytes, 8-aligned.
        let ok =
            unsafe { GetTokenInformation(token, class, buf.as_mut_ptr().cast(), len, &mut len) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buf)
    };
    let information = read();
    // SAFETY: `token` was opened above and is closed once, here.
    unsafe { CloseHandle(token) };
    information
}

/// The process token's `TOKEN_USER`, in a buffer that owns the SID it points to.
#[cfg(windows)]
struct CurrentUser {
    buf: Vec<u64>,
}

#[cfg(windows)]
impl CurrentUser {
    fn current() -> io::Result<Self> {
        token_information(windows_sys::Win32::Security::TokenUser).map(|buf| CurrentUser { buf })
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

// ---- The system temp dir: what the CLI hands to kubectl and helm (WI-473). ----

/// A new file in the system temp dir, `<prefix><random><suffix>`, made by [`create_new_shared`]:
/// owner-only before its first byte, whatever the temp dir (a `TMP` other accounts share) lets
/// others do, and readable meanwhile by another process of this account. Removed when dropped.
pub fn temp_file(prefix: &str, suffix: &str) -> io::Result<tempfile::NamedTempFile> {
    tempfile::Builder::new()
        .prefix(prefix)
        .suffix(suffix)
        .make(create_new_shared)
}

/// A [`temp_file`] holding `bytes`, written and flushed: a manifest or values file the CLI
/// passes to kubectl or helm by path. Removed when dropped.
pub fn write_temp_file(
    prefix: &str,
    suffix: &str,
    bytes: &[u8],
) -> io::Result<tempfile::NamedTempFile> {
    let mut file = temp_file(prefix, suffix)?;
    #[cfg(all(test, windows))]
    testing::record_before_first_byte(file.as_file());
    file.write_all(bytes)?;
    file.flush()?;
    Ok(file)
}

/// A new directory in the system temp dir, `<prefix><random>`, owner-only from its creation
/// ([`create_dir`]): where a backup stages what it exports and a restore unpacks what it
/// decrypts. Removed, with everything in it, when dropped.
pub fn temp_dir(prefix: &str) -> io::Result<TempDir> {
    let made = tempfile::Builder::new().prefix(prefix).make(create_dir)?;
    // tempfile would remove it as a file; a `TempDir` removes it as a directory.
    let path = made.into_temp_path().keep().map_err(|e| e.error)?;
    Ok(TempDir { path })
}

/// A directory [`temp_dir`] made; it is removed, with everything in it, when this drops.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---- Reading a secret file, and tightening it on the way (WI-473). ----

#[cfg(windows)]
thread_local! {
    /// How many [`without_tightening`] calls this thread is inside.
    static READ_ONLY: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Runs `f` with every [`open_secret`] on this thread reading only: no DACL is changed. Doctor
/// runs inside it, so it reports a file it finds wide and changes none.
#[cfg(windows)]
pub fn without_tightening<R>(f: impl FnOnce() -> R) -> R {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            READ_ONLY.with(|n| n.set(n.get() - 1));
        }
    }
    READ_ONLY.with(|n| n.set(n.get() + 1));
    let _leave = Leave;
    f()
}

/// Runs `f`. On Unix no read changes a file's mode.
#[cfg(not(windows))]
pub fn without_tightening<R>(f: impl FnOnce() -> R) -> R {
    f()
}

/// `path`, an existing secret file, open for reading. Unix: `File::open`; the mode was given
/// when the file was written. Windows: opened with the rights to read and set its DACL too and,
/// when that DACL is not owner-only ([`Dacl::is_owner_only`]), given the owner-only one on this
/// handle before anything is read. A file whose DACL this account may not set (another
/// account's), or that fails to take it, is read as it is: doctor reports it ([`exposure`]).
/// Nothing is changed inside [`without_tightening`].
#[cfg(not(windows))]
pub fn open_secret(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// `path`, an existing secret file, open for reading. Unix: `File::open`; the mode was given
/// when the file was written. Windows: opened with the rights to read and set its DACL too and,
/// when that DACL is not owner-only ([`Dacl::is_owner_only`]), given the owner-only one on this
/// handle before anything is read. A file whose DACL this account may not set (another
/// account's), or that fails to take it, is read as it is: doctor reports it ([`exposure`]).
/// Nothing is changed inside [`without_tightening`].
#[cfg(windows)]
pub fn open_secret(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Foundation::GENERIC_READ;
    use windows_sys::Win32::Storage::FileSystem::{READ_CONTROL, WRITE_DAC};
    if READ_ONLY.with(|n| n.get()) > 0 {
        return File::open(path);
    }
    let file = match OpenOptions::new()
        .access_mode(GENERIC_READ | READ_CONTROL | WRITE_DAC)
        .open(path)
    {
        Ok(file) => file,
        // Not this account's DACL to set: read it as it is.
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return File::open(path),
        Err(e) => return Err(e),
    };
    // A DACL that cannot be read or set leaves the file as it was; the read goes on.
    if dacl_of_file(&file).is_ok_and(|dacl| !dacl.is_owner_only()) {
        let _ = restrict_file(&file);
    }
    Ok(file)
}

/// The bytes of `path`, an existing secret file, read through [`open_secret`].
pub fn read_secret(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    open_secret(path)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Who may open an existing file, as its DACL says (Windows): read from the security
/// descriptor alone, never the file's bytes, and nothing is changed.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// Only this account: [`Dacl::is_owner_only`].
    OwnerOnly,
    /// Another account too, or whatever its folder grants, or everyone (no DACL). With
    /// `restrictable`, this account may set its DACL, and the next [`open_secret`] makes it
    /// owner-only; without, only its owner or an administrator can.
    Wide { restrictable: bool },
}

/// [`Exposure`] of the existing file at `path`.
#[cfg(windows)]
pub fn exposure(path: &Path) -> io::Result<Exposure> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{READ_CONTROL, WRITE_DAC};
    let file = OpenOptions::new().access_mode(READ_CONTROL).open(path)?;
    if dacl_of_file(&file)?.is_owner_only() {
        return Ok(Exposure::OwnerOnly);
    }
    // Asked for, never used: opening with the right changes nothing.
    let restrictable = match OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .open(path)
    {
        Ok(_) => true,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => false,
        Err(e) => return Err(e),
    };
    Ok(Exposure::Wide { restrictable })
}

// ---- Reading a DACL. ----

/// `ACCESS_ALLOWED_ACE_TYPE` and `ACCESS_DENIED_ACE_TYPE` (winnt.h; windows-sys has them only
/// under SystemServices).
#[cfg(windows)]
const ACCESS_ALLOWED: u8 = 0;
#[cfg(windows)]
const ACCESS_DENIED: u8 = 1;

/// A DACL as the OS reports it.
#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dacl {
    /// `SE_DACL_PROTECTED`: it takes no ACE from its parent.
    pub protected: bool,
    /// No DACL at all, which lets everyone do everything; `aces` is then empty.
    pub null: bool,
    pub aces: Vec<Ace>,
}

/// One ACE of a [`Dacl`].
#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    /// The ACE type (`ACCESS_ALLOWED_ACE_TYPE` is 0, `ACCESS_DENIED_ACE_TYPE` 1).
    pub kind: u8,
    /// The inheritance flags (`OBJECT_INHERIT_ACE`, `CONTAINER_INHERIT_ACE`,
    /// `INHERITED_ACE`, …).
    pub flags: u8,
    /// The access mask; only meaningful for the access-allowed and -denied types.
    pub mask: u32,
    /// Whether the ACE's SID is the process token's user; only for those types too.
    pub for_this_user: bool,
}

#[cfg(windows)]
impl Dacl {
    /// Whether only this account may open the object: a DACL is there, it is protected (it
    /// takes nothing from its folder), and every ACE that allows anything allows it to the
    /// process token's user. Deny ACEs are fine; any other ACE type is not.
    pub fn is_owner_only(&self) -> bool {
        !self.null
            && self.protected
            && self.aces.iter().all(|ace| match ace.kind {
                ACCESS_ALLOWED => ace.for_this_user,
                ACCESS_DENIED => true,
                _ => false,
            })
    }
}

/// The DACL of the file or directory at `path`.
#[cfg(windows)]
pub fn dacl_of_path(path: &Path) -> io::Result<Dacl> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: a NUL-terminated path; the out-pointers are valid; `descriptor` is freed by
    // `read_dacl`.
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
    read_dacl(descriptor, dacl)
}

/// The DACL of the open `file`, read through its handle (which needs `READ_CONTROL`; every
/// handle opened for reading or writing has it).
#[cfg(windows)]
pub fn dacl_of_file(file: &File) -> io::Result<Dacl> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
    let mut dacl = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: an open handle; the out-pointers are valid; `descriptor` is freed by `read_dacl`.
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
    read_dacl(descriptor, dacl)
}

/// The [`Dacl`] in `descriptor` (whose DACL is `dacl`); frees `descriptor`.
#[cfg(windows)]
fn read_dacl(
    descriptor: windows_sys::Win32::Security::PSECURITY_DESCRIPTOR,
    dacl: *mut windows_sys::Win32::Security::ACL,
) -> io::Result<Dacl> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::{
        AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL_SIZE_INFORMATION, PSID, SE_DACL_PROTECTED,
    };
    let result = (|| {
        let (mut control, mut revision) = (0u16, 0u32);
        // SAFETY: `descriptor` came from Get(Named)SecurityInfo.
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let protected = control & SE_DACL_PROTECTED != 0;
        if dacl.is_null() {
            // A NULL DACL grants everyone everything; no ACE to report.
            return Ok(Dacl {
                protected,
                null: true,
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
        let user = CurrentUser::current()?;
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
                if header.AceType == ACCESS_ALLOWED || header.AceType == ACCESS_DENIED {
                    // Access-allowed and access-denied share ACCESS_ALLOWED_ACE's layout.
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
        Ok(Dacl {
            protected,
            null: false,
            aces,
        })
    })();
    // SAFETY: `descriptor` was allocated by Get(Named)SecurityInfo and is freed once, here.
    unsafe { LocalFree(descriptor) };
    result
}

/// Reading a DACL back and widening one, for the tests of every owner-only file and directory
/// (this crate's, and through the `test-support` feature the core's, the state's and the
/// CLI's). Never in a shipped binary: `test-support` is enabled only from `[dev-dependencies]`.
#[cfg(all(windows, any(test, feature = "test-support")))]
pub mod testing {
    use std::cell::RefCell;
    use std::fs::File;
    use std::io;
    use std::path::Path;

    use windows_sys::Win32::Security::{CONTAINER_INHERIT_ACE, OBJECT_INHERIT_ACE};
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    pub use super::{Ace, Dacl};

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
                ace.kind,
                super::ACCESS_ALLOWED,
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
        super::dacl_of_path(path)
    }

    /// The DACL of the open `file`, read through its handle.
    pub fn of_file(file: &File) -> io::Result<Dacl> {
        super::dacl_of_file(file)
    }

    /// Gives the file or directory at `path` the DACL in `sddl` (`D:P(…)…`), protected when it
    /// says `P`: a test's wide or locked-down file. `OW` is OWNER RIGHTS, which replaces what an
    /// owner may do implicitly — `(A;;FR;;;OW)` leaves this account, the owner, unable to set
    /// the DACL again — and `WD` everyone.
    pub fn set_dacl(path: &Path, sddl: &str) -> io::Result<()> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS};
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW,
            SDDL_REVISION_1, SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::{
            GetSecurityDescriptorDacl, DACL_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            UNPROTECTED_DACL_SECURITY_INFORMATION,
        };
        let wide = |s: &std::ffi::OsStr| -> Vec<u16> {
            s.encode_wide().chain(std::iter::once(0)).collect()
        };
        let text = wide(std::ffi::OsStr::new(sddl));
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a NUL-terminated SDDL string; `descriptor` is freed below.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let (mut present, mut defaulted) = (0, 0);
            let mut dacl = std::ptr::null_mut();
            // SAFETY: `descriptor` is the one converted above.
            if unsafe {
                GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let protection = if sddl.starts_with("D:P") {
                PROTECTED_DACL_SECURITY_INFORMATION
            } else {
                UNPROTECTED_DACL_SECURITY_INFORMATION
            };
            let name = wide(path.as_os_str());
            // SAFETY: a NUL-terminated path; the DACL points into `descriptor`.
            let status = unsafe {
                SetNamedSecurityInfoW(
                    name.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | protection,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    dacl,
                    std::ptr::null(),
                )
            };
            if status != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(status as i32));
            }
            Ok(())
        })();
        // SAFETY: allocated by the conversion above, freed once, here.
        unsafe { LocalFree(descriptor) };
        result
    }

    /// Gives `path` a DACL other accounts can read through: full access for its owner (through
    /// OWNER RIGHTS, not this account's SID) and read for everyone, protected. Not owner-only,
    /// and this account may set it again: a file in a store other accounts can read.
    pub fn widen(path: &Path) -> io::Result<()> {
        set_dacl(path, "D:P(A;;FA;;;OW)(A;;FR;;;WD)")
    }

    /// Gives `path` a DACL everyone can read through and that its owner, this account, can no
    /// longer change: read for OWNER RIGHTS and for everyone, protected. The file can still be
    /// removed through its folder's DACL.
    pub fn widen_for_good(path: &Path) -> io::Result<()> {
        set_dacl(path, "D:P(A;;FR;;;OW)(A;;FR;;;WD)")
    }

    thread_local! {
        static PRETENDED_OWNER: RefCell<Option<Vec<u64>>> = const { RefCell::new(None) };
    }

    /// While the guard lives, [`super::restrict_dir`] on this thread reads every directory's
    /// owner as `sid` (`S-1-…`): another account's directory, made in a test's own temp dir.
    /// Never a system directory: code under test that is wrong must not change one, and an
    /// elevated token (a CI runner's) may open one with WRITE_DAC.
    pub fn pretend_owner(sid: &str) -> io::Result<PretendedOwner> {
        let bytes = sid_bytes(sid)?;
        PRETENDED_OWNER.with(|slot| *slot.borrow_mut() = Some(bytes));
        Ok(PretendedOwner)
    }

    /// [`pretend_owner`]'s guard: the real owners are read again once it drops.
    pub struct PretendedOwner;

    impl Drop for PretendedOwner {
        fn drop(&mut self) {
            PRETENDED_OWNER.with(|slot| *slot.borrow_mut() = None);
        }
    }

    /// The SID [`pretend_owner`] set on this thread, 8-aligned, if any.
    pub(super) fn pretended_owner() -> Option<Vec<u64>> {
        PRETENDED_OWNER.with(|slot| slot.borrow().clone())
    }

    /// The SID `sid` (`S-1-…`) in an 8-aligned buffer of its own.
    fn sid_bytes(sid: &str) -> io::Result<Vec<u64>> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
        use windows_sys::Win32::Security::GetLengthSid;
        let text: Vec<u16> = std::ffi::OsStr::new(sid)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut psid = std::ptr::null_mut();
        // SAFETY: a NUL-terminated SID string; `psid` is freed below.
        if unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut psid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a valid SID from the conversion.
        let len = unsafe { GetLengthSid(psid) } as usize;
        let mut buf = vec![0u64; len.div_ceil(8)];
        // SAFETY: `len` bytes are readable at `psid` and writable in `buf`.
        unsafe { std::ptr::copy_nonoverlapping(psid.cast::<u8>(), buf.as_mut_ptr().cast(), len) };
        // SAFETY: allocated by the conversion above, freed once, here.
        unsafe { LocalFree(psid) };
        Ok(buf)
    }

    /// Whether the owner of the file or directory at `path` is one [`super::restrict_dir`]
    /// accepts (this account, the token's default owner, LocalSystem, Administrators). Reads
    /// the owner and changes nothing.
    pub fn trusted_owner(path: &Path) -> io::Result<bool> {
        use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;
        super::trusted_owner(&super::open_dir(path, READ_CONTROL)?)
    }

    /// [`super::owner_is_trusted`] of the SID `sid` (`S-1-…`).
    pub fn sid_is_trusted(sid: &str) -> io::Result<bool> {
        let mut bytes = sid_bytes(sid)?;
        super::owner_is_trusted(bytes.as_mut_ptr().cast())
    }

    /// The process token's user, as `S-1-…`.
    pub fn this_user_sid() -> io::Result<String> {
        use windows_sys::Win32::Foundation::LocalFree;
        use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
        let user = super::CurrentUser::current()?;
        let mut text = std::ptr::null_mut();
        // SAFETY: a valid SID; `text` is freed below.
        if unsafe { ConvertSidToStringSidW(user.sid(), &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a NUL-terminated wide string from the conversion.
        let len = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
        // SAFETY: `len` u16s are readable at `text`.
        let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
        // SAFETY: allocated by the conversion above, freed once, here.
        unsafe { LocalFree(text.cast()) };
        Ok(sid)
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

    /// WI-474: while it is held, a new file is no one else's to open, to read or to write; and
    /// tempfile's cleanup, which removes it by name before it closes the handle (what a failed
    /// write drops), leaves nothing behind.
    #[cfg(windows)]
    #[test]
    fn a_new_file_is_opened_by_no_one_else_while_held_and_removed_when_dropped() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut file = tempfile::Builder::new()
            .prefix("held-")
            .make_in(dir.path(), create_new)
            .unwrap();
        file.write_all(b"secret").unwrap();
        let every = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
        let reader = OpenOptions::new()
            .read(true)
            .share_mode(every)
            .open(file.path());
        let writer = OpenOptions::new()
            .write(true)
            .share_mode(every)
            .open(file.path());
        for (who, opened) in [("a reader", reader), ("a writer", writer)] {
            assert_eq!(
                opened.expect_err(who).raw_os_error(),
                Some(ERROR_SHARING_VIOLATION as i32),
                "{who}"
            );
        }
        let path = file.path().to_path_buf();
        drop(file);
        assert!(!path.exists(), "removed when dropped");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    // ---- WI-473 ----

    /// The temp files the CLI hands to kubectl: in the system temp dir, mode 0600 from the open.
    #[cfg(unix)]
    #[test]
    fn a_temp_file_is_0600_in_the_system_temp_dir() {
        use std::os::unix::fs::PermissionsExt;
        let file = write_temp_file("apprafter-test-", ".json", b"{}").unwrap();
        assert!(file.path().starts_with(std::env::temp_dir()), "{file:?}");
        let name = file
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("apprafter-test-") && name.ends_with(".json"),
            "{name}"
        );
        let mode = std::fs::metadata(file.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        assert_eq!(std::fs::read(file.path()).unwrap(), b"{}");
        let path = file.path().to_path_buf();
        drop(file);
        assert!(!path.exists(), "removed when dropped");
    }

    /// The temp files the CLI hands to kubectl: owner-only before the first byte (the recorder
    /// at the write site), and after.
    #[cfg(windows)]
    #[test]
    fn a_temp_file_has_the_owner_only_dacl_before_its_first_byte() {
        let _ = testing::take_before_first_byte();
        let file = write_temp_file("apprafter-test-", ".json", b"{}").unwrap();
        assert!(file.path().starts_with(std::env::temp_dir()), "{file:?}");
        testing::take_before_first_byte()
            .expect("the write site records the DACL")
            .unwrap()
            .assert_owner_only_file("a temp file, before its first byte");
        testing::of_path(file.path())
            .unwrap()
            .assert_owner_only_file("a temp file, written");
    }

    /// kubectl reads the file while the CLI holds it: Go opens with read access, sharing reading
    /// and writing (not deletion). Such a reader gets the bytes; once it is gone, dropping the
    /// file removes it, which needs the CLI's handle to share deletion.
    #[cfg(windows)]
    #[test]
    fn a_temp_file_is_read_by_kubectl_while_held_and_removed_when_dropped() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        let file = write_temp_file("apprafter-test-", ".yaml", b"apiVersion: v1\n").unwrap();
        let mut go_reader = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(file.path())
            .expect("a reader that shares as Go's os.Open does opens the held file");
        let mut seen = Vec::new();
        go_reader.read_to_end(&mut seen).unwrap();
        assert_eq!(seen, b"apiVersion: v1\n");
        drop(go_reader);
        assert_eq!(
            std::fs::read(file.path()).unwrap(),
            b"apiVersion: v1\n",
            "std's reader too"
        );
        let path = file.path().to_path_buf();
        drop(file);
        assert!(!path.exists(), "removed when dropped");
    }

    #[cfg(unix)]
    #[test]
    fn a_temp_dir_is_0700_and_removed_with_what_is_in_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("apprafter-test-").unwrap();
        assert!(dir.path().starts_with(std::env::temp_dir()), "{dir:?}");
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
        std::fs::write(dir.path().join("data"), b"x").unwrap();
        let path = dir.path().to_path_buf();
        drop(dir);
        assert!(!path.exists(), "removed when dropped");
    }

    /// Owner-only from its creation, and what is unpacked into it inherits the one ACE.
    #[cfg(windows)]
    #[test]
    fn a_temp_dir_is_owner_only_from_its_creation_and_removed_with_what_is_in_it() {
        let dir = temp_dir("apprafter-test-").unwrap();
        assert!(dir.path().starts_with(std::env::temp_dir()), "{dir:?}");
        testing::of_path(dir.path())
            .unwrap()
            .assert_owner_only_dir("a temp dir");
        let data = dir.path().join("data");
        std::fs::write(&data, b"x").unwrap();
        let inherited = testing::of_path(&data).unwrap();
        assert_eq!(inherited.aces.len(), 1, "{inherited:?}");
        assert!(inherited.aces[0].for_this_user, "{inherited:?}");
        let path = dir.path().to_path_buf();
        drop(dir);
        assert!(!path.exists(), "removed when dropped");
    }

    #[test]
    fn a_secret_reads_back_its_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        std::fs::write(&path, b"token").unwrap();
        assert_eq!(read_secret(&path).unwrap(), b"token");
        assert_eq!(
            read_secret(&dir.path().join("missing")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    /// A file other accounts can read — one that inherits from its folder, and one with an ACE
    /// for everyone — is owner-only once read, with its bytes intact.
    #[cfg(windows)]
    #[test]
    fn reading_a_wide_secret_makes_it_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let inherited = dir.path().join("inherited");
        std::fs::write(&inherited, b"one").unwrap();
        let widened = dir.path().join("widened");
        std::fs::write(&widened, b"two").unwrap();
        testing::widen(&widened).unwrap();
        for (path, bytes) in [(&inherited, b"one"), (&widened, b"two")] {
            assert!(
                !testing::of_path(path).unwrap().is_owner_only(),
                "the test's premise: {path:?}"
            );
            assert_eq!(
                exposure(path).unwrap(),
                Exposure::Wide { restrictable: true }
            );
            assert_eq!(read_secret(path).unwrap(), bytes);
            testing::of_path(path)
                .unwrap()
                .assert_owner_only_file("a wide secret, read");
            assert_eq!(exposure(path).unwrap(), Exposure::OwnerOnly);
        }
    }

    /// Doctor's reads: the bytes, and the DACL left as it was.
    #[cfg(windows)]
    #[test]
    fn a_read_without_tightening_changes_no_dacl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("widened");
        std::fs::write(&path, b"token").unwrap();
        testing::widen(&path).unwrap();
        let before = testing::of_path(&path).unwrap();
        let read = without_tightening(|| read_secret(&path)).unwrap();
        assert_eq!(read, b"token");
        assert_eq!(testing::of_path(&path).unwrap(), before);
        // Outside it again, the read tightens.
        read_secret(&path).unwrap();
        testing::of_path(&path)
            .unwrap()
            .assert_owner_only_file("read outside without_tightening");
    }

    /// A file whose DACL this account may not set is read as it is, and reported so.
    #[cfg(windows)]
    #[test]
    fn a_secret_this_account_may_not_restrict_is_read_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked-wide");
        std::fs::write(&path, b"token").unwrap();
        testing::widen_for_good(&path).unwrap();
        let before = testing::of_path(&path).unwrap();
        assert_eq!(read_secret(&path).unwrap(), b"token");
        assert_eq!(testing::of_path(&path).unwrap(), before);
        assert_eq!(
            exposure(&path).unwrap(),
            Exposure::Wide {
                restrictable: false
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_new_file_is_owner_only_and_a_missing_one_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret");
        drop(create_new(&path).unwrap());
        assert_eq!(exposure(&path).unwrap(), Exposure::OwnerOnly);
        assert_eq!(
            exposure(&dir.path().join("missing")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    /// What counts as owner-only: protected, and every allowing ACE this account's.
    #[cfg(windows)]
    #[test]
    fn owner_only_is_protected_with_every_allowing_ace_for_this_user() {
        let mine = |kind, for_this_user| Ace {
            kind,
            flags: 0,
            mask: 0x1f01ff,
            for_this_user,
        };
        let dacl = |protected, null, aces| Dacl {
            protected,
            null,
            aces,
        };
        assert!(dacl(true, false, vec![mine(ACCESS_ALLOWED, true)]).is_owner_only());
        assert!(dacl(
            true,
            false,
            vec![mine(ACCESS_DENIED, false), mine(ACCESS_ALLOWED, true)]
        )
        .is_owner_only());
        assert!(!dacl(false, false, vec![mine(ACCESS_ALLOWED, true)]).is_owner_only());
        assert!(!dacl(
            true,
            false,
            vec![mine(ACCESS_ALLOWED, true), mine(ACCESS_ALLOWED, false)]
        )
        .is_owner_only());
        assert!(!dacl(true, true, vec![]).is_owner_only());
        assert!(!dacl(true, false, vec![mine(9, true)]).is_owner_only());
    }

    /// The owners a directory is used under: this account, LocalSystem and the Administrators
    /// group; never another account or a service's.
    #[cfg(windows)]
    #[test]
    fn a_directory_is_trusted_only_under_this_account_system_or_administrators() {
        let me = testing::this_user_sid().unwrap();
        assert!(testing::sid_is_trusted(&me).unwrap(), "{me}");
        assert!(testing::sid_is_trusted("S-1-5-18").unwrap(), "LocalSystem");
        assert!(
            testing::sid_is_trusted("S-1-5-32-544").unwrap(),
            "Administrators"
        );
        for other in [
            "S-1-5-32-545",
            "S-1-1-0",
            "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
            "S-1-5-21-1-2-3-1001",
        ] {
            assert!(!testing::sid_is_trusted(other).unwrap(), "{other}");
        }
    }

    /// The owner of a real directory another account owns (`%SystemRoot%`, TrustedInstaller's)
    /// is read as such; one this process made, as its own. Only read: nothing is ever set on a
    /// system directory, whatever the code under test does.
    #[cfg(windows)]
    #[test]
    fn the_owner_is_read_from_the_directory() {
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        assert!(!testing::trusted_owner(Path::new(&system_root)).unwrap());
        let dir = tempfile::tempdir().unwrap();
        assert!(testing::trusted_owner(dir.path()).unwrap());
    }

    /// A directory another account owns is refused as such and left as it was, whether or not
    /// this account may set its DACL; under its own owner again, it is restricted.
    #[cfg(windows)]
    #[test]
    fn a_directory_another_account_owns_is_never_restricted() {
        let dir = tempfile::tempdir().unwrap();
        let before = testing::of_path(dir.path()).unwrap();
        {
            let _foreign = testing::pretend_owner("S-1-5-21-1-2-3-1001").unwrap();
            assert!(matches!(
                restrict_dir(dir.path()),
                Err(RestrictDirError::ForeignOwner)
            ));
        }
        assert_eq!(testing::of_path(dir.path()).unwrap(), before);
        restrict_dir(dir.path()).unwrap();
        testing::of_path(dir.path())
            .unwrap()
            .assert_owner_only_dir("the directory under its own owner");
    }
}
