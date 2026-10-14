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
//! - Every folder of the target store is made owner-only from its root down
//!   ([`create_dir_all_from`], WI-479): 0700 whatever the umask, the owner-only DACL on Windows.
//!   The folders above the root are made as the umask or the parent says, and a folder already
//!   there is left as it is: `crate::store_privacy` refuses one another account can change.
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
//! - Who other than this account can change a file or directory ([`control`], WI-475): its
//!   owner, and whether other accounts may write it, read from its metadata or security
//!   descriptor alone. The target store refuses a folder another account owns or can write
//!   (`crate::store_privacy`), and doctor reports it.

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
/// taken. Unix: mode 0700 at `mkdir`, which the umask can only take bits from, so it is never
/// wider, not for a moment; then 0700 is set again (WI-479), which gives back what a umask such
/// as 0277 takes from the owner and gives no one else anything. A directory that cannot be set
/// so is removed and the error returned. Windows: created with the protected DACL whose one ACE
/// the files and directories made in it inherit, so it never has another, not for a moment.
#[cfg(unix)]
pub fn create_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().mode(0o700).create(path)?;
    #[cfg(test)]
    made::record(path);
    if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
        let _ = std::fs::remove_dir(path);
        return Err(e);
    }
    Ok(())
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

/// `dir`, made with every folder missing from `top` down to it, each owner-only from its
/// creation ([`create_dir`]) whatever the umask (WI-479); a folder missing above `top` is made
/// as `std::fs::create_dir_all` makes it: with the mode the umask gives on Unix, with what it
/// inherits on Windows. A folder already there, at any level, is left as it is. `dir` must be
/// `top` or a path under it (`InvalidInput` otherwise).
///
/// The target store is made so from its root down
/// ([`crate::target::TargetStorePaths::create_dir_all`]): every folder in it is this account's
/// alone, while the folders the root sits in (`~/.config`) are the account's own, made as it
/// makes any other folder and never checked (`crate::store_privacy`).
pub fn create_dir_all_from(top: &Path, dir: &Path) -> io::Result<()> {
    let below = dir.strip_prefix(top).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not under {}", dir.display(), top.display()),
        )
    })?;
    if is_dir(dir) {
        return Ok(());
    }
    if let Some(parent) = top.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut at = top.to_path_buf();
    create_dir_unless_there(&at)?;
    for part in below.components() {
        at.push(part);
        create_dir_unless_there(&at)?;
    }
    Ok(())
}

/// [`create_dir`], a directory already at `path` (another process's, made meanwhile) left as
/// it is.
fn create_dir_unless_there(path: &Path) -> io::Result<()> {
    match create_dir(path) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && is_dir(path) => Ok(()),
        other => other,
    }
}

/// Whether a directory is at `path`, a symlink followed.
fn is_dir(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// What [`create_dir`] made, with the mode `mkdir` gave it before it was set again: the tests'
/// proof that a new folder is never wider than 0700, not for a moment. Per thread.
#[cfg(all(test, unix))]
pub(crate) mod made {
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    thread_local! {
        static MADE: RefCell<Vec<(PathBuf, u32)>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn record(path: &Path) {
        let mode = std::fs::symlink_metadata(path)
            .map(|m| m.permissions().mode() & 0o7777)
            .unwrap_or(u32::MAX);
        MADE.with(|m| m.borrow_mut().push((path.to_path_buf(), mode)));
    }

    /// Everything recorded on this thread since the last call.
    pub(crate) fn take() -> Vec<(PathBuf, u32)> {
        MADE.with(|m| std::mem::take(&mut *m.borrow_mut()))
    }
}

/// One test of the running test binary, run again in a child process under another umask
/// (WI-479). A umask belongs to the whole process, so a test that set its own would change what
/// every other test thread makes meanwhile. Unix; for the tests of this crate and, through the
/// `test-support` feature, the core's and the CLI's.
#[cfg(all(unix, any(test, feature = "test-support")))]
pub mod under_umask {
    use std::path::{Path, PathBuf};

    /// How the child is handed its umask and scratch directory: a second filter after the
    /// test's name, `apprafter-under-umask=<octal umask>:<scratch>`, which names no test
    /// (`--exact`). An argument, not a variable: the child reads no environment for it (the
    /// core's env-read ratchet counts this module, which `test-support` builds).
    const PARAM: &str = "apprafter-under-umask=";

    /// Held for writing while a child starts. From its fork to its exec a child holds a copy
    /// of every descriptor this process has open, a store lock's sentinel included, and so
    /// holds that lock a moment after the test that took it let it go.
    static STARTING: std::sync::RwLock<()> = std::sync::RwLock::new(());

    /// No child of [`run`] starts in this process while the guard lives: for a test that lets a
    /// store lock go and takes it again at once, and must not find it held.
    pub fn no_child_starting() -> std::sync::RwLockReadGuard<'static, ()> {
        STARTING.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs the ignored test `name` (its path in this test binary) in a child of this test
    /// binary whose umask is `mask`, alone (`--exact`, one thread), handing it `scratch`: a
    /// temporary directory the caller made under its own umask, since one such as 0277 leaves
    /// the child unable to make one it can write. Panics, with the child's output, unless
    /// exactly that test ran and passed.
    pub fn run(name: &str, mask: u32, scratch: &Path) {
        use std::os::unix::process::CommandExt;
        let scratch = scratch
            .to_str()
            .expect("a scratch directory named in UTF-8");
        let param = format!("{PARAM}{mask:o}:{scratch}");
        let mut child =
            std::process::Command::new(std::env::current_exe().expect("this test binary"));
        child.args([
            "--exact",
            name,
            &param,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]);
        #[allow(clippy::unnecessary_cast)] // `mode_t` is u32 on Linux, u16 on macOS.
        let m = mask as libc::mode_t;
        // SAFETY: umask(2) is async-signal-safe and cannot fail; the closure touches nothing
        // else of this process.
        unsafe {
            child.pre_exec(move || {
                libc::umask(m);
                Ok(())
            });
        }
        child
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // `spawn` returns once the child has exec'd (or failed to), its copies closed.
        let started = {
            let _starting = STARTING.write().unwrap_or_else(|e| e.into_inner());
            child.spawn().expect("run this test binary again")
        };
        let out = started.wait_with_output().expect("the child's output");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success() && text.contains("test result: ok. 1 passed"),
            "{name} under umask {mask:03o}:\n{text}"
        );
    }

    /// What [`run`] handed its child; panics anywhere else: such a test means nothing outside
    /// its child.
    fn param() -> (u32, PathBuf) {
        let param = std::env::args()
            .find_map(|arg| arg.strip_prefix(PARAM).map(str::to_string))
            .unwrap_or_else(|| panic!("run only by under_umask::run, in a child"));
        let (mask, scratch) = param.split_once(':').expect("<umask>:<scratch>");
        (
            u32::from_str_radix(mask, 8).expect("an octal umask"),
            PathBuf::from(scratch),
        )
    }

    /// In a child [`run`] started: its umask, as the process has it (checked against what
    /// [`run`] gave). Panics anywhere else.
    pub fn mask() -> u32 {
        let (given, _) = param();
        // SAFETY: umask(2) sets and returns; the old value is put back at once, and the child
        // runs one test on one thread.
        let actual = unsafe {
            let old = libc::umask(0o077);
            libc::umask(old);
            old
        };
        #[allow(clippy::unnecessary_cast)] // `mode_t` is u32 on Linux, u16 on macOS.
        let actual = actual as u32;
        assert_eq!(actual, given, "the child's umask");
        given
    }

    /// In a child [`run`] started: the scratch directory it was handed. Panics anywhere else.
    pub fn scratch() -> PathBuf {
        param().1
    }
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

/// The SIDs whose rights on a file are no other account's ([`Ace::trusted`], WI-475): the
/// process token's user and default owner (in buffers that own them), LocalSystem, the
/// Administrators group, OWNER RIGHTS and CREATOR OWNER.
#[cfg(windows)]
struct TrustedSids {
    user: Vec<u64>,
    default_owner: Vec<u64>,
    /// SECURITY_MAX_SID_SIZE (68) bytes each, 8-aligned.
    well_known: [[u64; 9]; 4],
}

#[cfg(windows)]
impl TrustedSids {
    fn current() -> io::Result<Self> {
        use windows_sys::Win32::Security::{
            CreateWellKnownSid, TokenOwner, TokenUser, WinBuiltinAdministratorsSid,
            WinCreatorOwnerRightsSid, WinCreatorOwnerSid, WinLocalSystemSid,
        };
        let mut well_known = [[0u64; 9]; 4];
        for (sid, kind) in well_known.iter_mut().zip([
            WinLocalSystemSid,
            WinBuiltinAdministratorsSid,
            WinCreatorOwnerRightsSid,
            WinCreatorOwnerSid,
        ]) {
            let mut len = std::mem::size_of_val(sid) as u32;
            // SAFETY: `sid` holds `len` writable bytes; no domain SID for these.
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
        }
        Ok(TrustedSids {
            user: token_information(TokenUser)?,
            default_owner: token_information(TokenOwner)?,
            well_known,
        })
    }

    /// Whether `sid` (a valid SID) is one of them.
    fn contains(&self, sid: windows_sys::Win32::Security::PSID) -> bool {
        use windows_sys::Win32::Security::{EqualSid, PSID, TOKEN_OWNER, TOKEN_USER};
        // SAFETY: the buffers hold the TOKEN_USER and TOKEN_OWNER GetTokenInformation wrote.
        let (user, owner) = unsafe {
            (
                (*self.user.as_ptr().cast::<TOKEN_USER>()).User.Sid,
                (*self.default_owner.as_ptr().cast::<TOKEN_OWNER>()).Owner,
            )
        };
        let well_known = self
            .well_known
            .iter()
            .map(|sid| -> PSID { sid.as_ptr().cast_mut().cast() });
        // SAFETY: every SID compared is valid: `sid` from the caller, the others above.
        std::iter::once(user)
            .chain(std::iter::once(owner))
            .chain(well_known)
            .any(|trusted| unsafe { EqualSid(sid, trusted) } != 0)
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

// ---- Who else can change a file or directory (WI-475). ----

/// Who other than this account can change an existing file or directory, as its metadata or
/// security descriptor says ([`control`]): nothing is read from its contents, and nothing is
/// changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Control {
    /// Another account owns it, and so can change it — on Windows, give itself the right to,
    /// whatever its DACL says. Unix: an owner other than this process's effective user. Windows:
    /// an owner [`restrict_dir`] does not accept (this account, the token's default owner,
    /// LocalSystem and the Administrators group are accepted).
    pub foreign_owner: bool,
    /// Other accounts may write it: a directory's entries they can create, replace or remove, a
    /// file's bytes. Unix: the write bit for others — a sticky bit does not undo it, since they
    /// can still create names in it — or for its group, unless that group is this account's
    /// own private group ([`group_is_only`]). Windows: no DACL at all, or an access-allowed ACE
    /// that grants a right to write, to delete, or to change the DACL or the owner, to a SID
    /// other than this account's, the token's default owner, LocalSystem, the Administrators
    /// group, OWNER RIGHTS or CREATOR OWNER — on the object itself or for what is made in it,
    /// which inherits it.
    pub writable_by_others: bool,
    /// Unix: the permission bits (`st_mode & 0o7777`), for display. `None` on Windows.
    pub mode: Option<u32>,
}

impl Control {
    /// Whether this account alone can change it.
    pub fn is_private(&self) -> bool {
        !self.foreign_owner && !self.writable_by_others
    }
}

/// [`Control`] of the existing file or directory at `path`, a symlink followed.
#[cfg(unix)]
pub fn control(path: &Path) -> io::Result<Control> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path)?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    #[allow(unused_mut)] // Mutated only by the tests' seam.
    let mut foreign_owner = meta.uid() != me;
    #[cfg(any(test, feature = "test-support"))]
    {
        foreign_owner |= pretend::is_foreign(path);
    }
    let mode = meta.mode() & 0o7777;
    let writable_by_others =
        mode & 0o002 != 0 || (mode & 0o020 != 0 && !group_is_only(meta.gid(), me));
    Ok(Control {
        foreign_owner,
        writable_by_others,
        mode: Some(mode),
    })
}

/// [`Control`] of the existing file or directory at `path`, a reparse point followed. Opened
/// with `READ_CONTROL` only.
#[cfg(windows)]
pub fn control(path: &Path) -> io::Result<Control> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, READ_CONTROL};
    let file = OpenOptions::new()
        .access_mode(READ_CONTROL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    #[allow(unused_mut)] // Mutated only by the tests' seam.
    let mut foreign_owner = !trusted_owner(&file)?;
    #[cfg(any(test, feature = "test-support"))]
    {
        foreign_owner |= pretend::is_foreign(path);
    }
    Ok(Control {
        foreign_owner,
        writable_by_others: dacl_of_file(&file)?.writable_by_others(),
        mode: None,
    })
}

/// Whether the group `gid` holds no account but `uid`: `uid`'s primary group, named as the
/// account is, with no other member listed — a user private group, in which a umask of 002
/// (the default for such groups on several distributions) makes every new folder
/// group-writable to no one else. Read from the user and group databases (`getpwuid_r`,
/// `getgrgid_r`); an account or a group that cannot be read counts as shared.
#[cfg(unix)]
fn group_is_only(gid: u32, uid: u32) -> bool {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(alone) = pretend::pretended_group_alone() {
        return alone;
    }
    match (account(uid), group(gid)) {
        (Some(account), Some(group)) => is_private_group(&account, gid, &group),
        _ => false,
    }
}

/// [`group_is_only`]'s judgement: `account`'s primary group is `gid`, `group` (gid's entry) has
/// its name, and lists no member but it.
#[cfg(unix)]
fn is_private_group(account: &Account, gid: u32, group: &Group) -> bool {
    account.gid == gid
        && group.name == account.name
        && group.members.iter().all(|m| *m == account.name)
}

/// An entry of the user database: the name and the primary group.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Account {
    name: Vec<u8>,
    gid: u32,
}

/// An entry of the group database: the name and the members it lists (not the accounts whose
/// primary group it is).
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Group {
    name: Vec<u8>,
    members: Vec<Vec<u8>>,
}

/// The largest buffer [`account`] and [`group`] grow to; a group listing more members than
/// that holds counts as shared.
#[cfg(unix)]
const NSS_BUFFER_MAX: usize = 1 << 20;

/// `uid`'s entry in the user database (`getpwuid_r`), or `None`.
#[cfg(unix)]
fn account(uid: u32) -> Option<Account> {
    let mut buf = vec![0u8; 4096];
    loop {
        // SAFETY: plain C data; getpwuid_r fills every field it returns.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `buf` holds `buf.len()` writable bytes for the strings `entry` points into;
        // `entry` and `found` are valid out-pointers.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &mut entry,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && buf.len() < NSS_BUFFER_MAX {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() || entry.pw_name.is_null() {
            return None;
        }
        // SAFETY: a NUL-terminated string in `buf`, which outlives this use.
        let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) }
            .to_bytes()
            .to_vec();
        return Some(Account {
            name,
            gid: entry.pw_gid,
        });
    }
}

/// `gid`'s entry in the group database (`getgrgid_r`), or `None`.
#[cfg(unix)]
fn group(gid: u32) -> Option<Group> {
    let mut buf = vec![0u8; 4096];
    loop {
        // SAFETY: plain C data; getgrgid_r fills every field it returns.
        let mut entry: libc::group = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::group = std::ptr::null_mut();
        // SAFETY: as in `account`.
        let rc = unsafe {
            libc::getgrgid_r(
                gid,
                &mut entry,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && buf.len() < NSS_BUFFER_MAX {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() || entry.gr_name.is_null() {
            return None;
        }
        // SAFETY: NUL-terminated strings in `buf`; `gr_mem` is a NULL-terminated array of them.
        let (name, members) = unsafe {
            let name = std::ffi::CStr::from_ptr(entry.gr_name).to_bytes().to_vec();
            let mut members = Vec::new();
            let mut at = entry.gr_mem;
            while !at.is_null() && !(*at).is_null() {
                members.push(std::ffi::CStr::from_ptr(*at).to_bytes().to_vec());
                at = at.add(1);
            }
            (name, members)
        };
        return Some(Group { name, members });
    }
}

/// What the tests of [`control`] — this crate's, and through the `test-support` feature the
/// core's and the CLI's — pretend about a file or directory they made in their own temp dir:
/// another account's, or a group that is this account's alone. Never a system directory, and
/// never in a shipped binary (`test-support` is enabled only from `[dev-dependencies]`). Per
/// thread, and only while the guard lives.
#[cfg(any(test, feature = "test-support"))]
pub mod pretend {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    thread_local! {
        static FOREIGN: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
        static GROUP_ALONE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    }

    /// While the guard lives, [`super::control`] on this thread reads `path` (exactly that
    /// path) as owned by another account. Its permissions are read as they are.
    pub fn foreign_owner(path: &Path) -> ForeignOwner {
        FOREIGN.with(|f| f.borrow_mut().push(path.to_path_buf()));
        ForeignOwner(path.to_path_buf())
    }

    /// [`foreign_owner`]'s guard.
    pub struct ForeignOwner(PathBuf);

    impl Drop for ForeignOwner {
        fn drop(&mut self) {
            FOREIGN.with(|f| {
                let mut f = f.borrow_mut();
                if let Some(at) = f.iter().position(|p| *p == self.0) {
                    f.remove(at);
                }
            });
        }
    }

    pub(super) fn is_foreign(path: &Path) -> bool {
        FOREIGN.with(|f| f.borrow().iter().any(|p| p == path))
    }

    /// While the guard lives, [`super::control`] on this thread (Unix) judges a group-writable
    /// file's group as this account's alone (`true`) or as shared (`false`), whatever the user
    /// and group databases say: the machine running the tests may have user private groups
    /// or not.
    pub fn group_alone(alone: bool) -> GroupAlone {
        GROUP_ALONE.with(|g| g.set(Some(alone)));
        GroupAlone
    }

    /// [`group_alone`]'s guard.
    pub struct GroupAlone;

    impl Drop for GroupAlone {
        fn drop(&mut self) {
            GROUP_ALONE.with(|g| g.set(None));
        }
    }

    #[cfg_attr(windows, allow(dead_code))] // Unix's group check reads it.
    pub(super) fn pretended_group_alone() -> Option<bool> {
        GROUP_ALONE.with(|g| g.get())
    }
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
    /// Whether the ACE's SID is one whose rights are no other account's (WI-475): the process
    /// token's user or default owner, LocalSystem, the Administrators group, OWNER RIGHTS (the
    /// owner, which [`Control::foreign_owner`] judges) or CREATOR OWNER (whoever creates an
    /// object that inherits it; on the object itself it matches no one). Only for those types
    /// too.
    pub trusted: bool,
}

/// The rights that let an account change a file or a directory's entries ([`Dacl::writable_by_others`]):
/// write its data or create files in it (`FILE_WRITE_DATA`), append or create folders in it
/// (`FILE_APPEND_DATA`), remove what is in it (`FILE_DELETE_CHILD`), remove or rename it
/// (`DELETE`), change its DACL or its owner (`WRITE_DAC`, `WRITE_OWNER`), and the generic
/// rights that include them. Not its attributes or extended attributes, which change no bytes.
#[cfg(windows)]
const WRITE_RIGHTS: u32 = {
    use windows_sys::Win32::Foundation::{GENERIC_ALL, GENERIC_WRITE};
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_WRITE_DATA, WRITE_DAC, WRITE_OWNER,
    };
    FILE_WRITE_DATA
        | FILE_APPEND_DATA
        | FILE_DELETE_CHILD
        | DELETE
        | WRITE_DAC
        | WRITE_OWNER
        | GENERIC_WRITE
        | GENERIC_ALL
};

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

    /// Whether another account may change the object (WI-475): there is no DACL, or an
    /// access-allowed ACE grants one of [`WRITE_RIGHTS`] to a SID that is not
    /// [`Ace::trusted`]. Its inheritance flags are not read: an ACE that only what is made in
    /// the object inherits lets that account change the folders and files AppRafter makes
    /// there. A deny ACE is not weighed against it.
    pub fn writable_by_others(&self) -> bool {
        self.null
            || self.aces.iter().any(|ace| {
                ace.kind == ACCESS_ALLOWED && !ace.trusted && ace.mask & WRITE_RIGHTS != 0
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
        let trusted = TrustedSids::current()?;
        let mut aces = Vec::new();
        for index in 0..info.AceCount {
            let mut ace = std::ptr::null_mut();
            // SAFETY: `index` is below AceCount.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: every ACE starts with an ACE_HEADER.
            let header = unsafe { *ace.cast::<ACE_HEADER>() };
            let (mask, for_this_user, sid_trusted) =
                if header.AceType == ACCESS_ALLOWED || header.AceType == ACCESS_DENIED {
                    // Access-allowed and access-denied share ACCESS_ALLOWED_ACE's layout.
                    let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
                    // SAFETY: an ACE of that layout, told by its type byte.
                    unsafe {
                        let sid: PSID = std::ptr::addr_of_mut!((*ace).SidStart).cast();
                        (
                            (*ace).Mask,
                            EqualSid(sid, user.sid()) != 0,
                            trusted.contains(sid),
                        )
                    }
                } else {
                    (0, false, false)
                };
            aces.push(Ace {
                kind: header.AceType,
                flags: header.AceFlags,
                mask,
                for_this_user,
                trusted: sid_trusted,
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
            trusted: for_this_user,
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

    // ---- WI-475: who else can change a file or directory ----

    /// A directory and a file this process made in its own temp dir are this account's alone.
    #[test]
    fn what_this_account_made_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        for path in [dir.path(), file.as_path()] {
            let c = control(path).unwrap();
            assert!(c.is_private(), "{path:?}: {c:?}");
        }
        assert_eq!(
            control(&dir.path().join("missing")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    /// Another account's directory (pretended, on a temp dir: never a system one) is foreign,
    /// and only that path, only while the guard lives.
    #[test]
    fn a_folder_another_account_owns_is_foreign() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        {
            let _foreign = pretend::foreign_owner(&other);
            let c = control(&other).unwrap();
            assert!(c.foreign_owner && !c.writable_by_others, "{c:?}");
            assert!(!c.is_private());
            assert!(control(dir.path()).unwrap().is_private());
        }
        assert!(control(&other).unwrap().is_private());
    }

    /// The owner is read from the folder itself, not only through the tests' seam: a folder
    /// another account owns on every system — `/`, root's (Unix), `%SystemRoot%`,
    /// TrustedInstaller's (Windows) — is foreign; one this process made is not. `control` only
    /// reads its metadata or security descriptor (it has no way to change anything), so
    /// pointing it at a system folder changes nothing whatever the code under test does
    /// (GOTCHA-255 is about code that may set a DACL).
    #[test]
    fn the_owner_is_read_from_the_folder() {
        #[cfg(unix)]
        let system = std::path::PathBuf::from("/");
        #[cfg(windows)]
        let system = std::path::PathBuf::from(
            std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()),
        );
        assert!(control(&system).unwrap().foreign_owner, "{system:?}");
        let dir = tempfile::tempdir().unwrap();
        assert!(!control(dir.path()).unwrap().foreign_owner);
    }

    /// Unix: the write bit for others is never private, with the sticky bit too (others can
    /// still create names); read and execute for others are.
    #[cfg(unix)]
    #[test]
    fn a_folder_others_can_write_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        for (mode, writable) in [
            (0o700, false),
            (0o755, false),
            (0o555, false),
            (0o705, false),
            (0o707, true),
            (0o777, true),
            (0o1777, true),
            (0o703, true),
        ] {
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(mode)).unwrap();
            let c = control(&folder).unwrap();
            assert_eq!(c.writable_by_others, writable, "{mode:o}: {c:?}");
            assert_eq!(c.mode, Some(mode), "{mode:o}");
            assert!(!c.foreign_owner, "{mode:o}");
        }
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// Unix: the group's write bit is private only when the group is this account's own.
    #[cfg(unix)]
    #[test]
    fn a_group_writable_folder_is_private_only_in_a_group_of_this_account_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o770)).unwrap();
        {
            let _shared = pretend::group_alone(false);
            assert!(control(&folder).unwrap().writable_by_others);
        }
        {
            let _alone = pretend::group_alone(true);
            assert!(control(&folder).unwrap().is_private());
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o772)).unwrap();
            assert!(
                control(&folder).unwrap().writable_by_others,
                "others' write bit, whatever the group"
            );
        }
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// Unix: a user private group is the account's primary group, named as it is, listing no
    /// one else.
    #[cfg(unix)]
    #[test]
    fn a_private_group_is_the_accounts_own_and_names_no_one_else() {
        let me = Account {
            name: b"op".to_vec(),
            gid: 1000,
        };
        let group = |name: &[u8], members: &[&[u8]]| Group {
            name: name.to_vec(),
            members: members.iter().map(|m| m.to_vec()).collect(),
        };
        assert!(is_private_group(&me, 1000, &group(b"op", &[])));
        assert!(is_private_group(&me, 1000, &group(b"op", &[b"op"])));
        assert!(!is_private_group(
            &me,
            1000,
            &group(b"op", &[b"op", b"eve"])
        ));
        assert!(!is_private_group(&me, 1000, &group(b"users", &[])));
        assert!(!is_private_group(&me, 100, &group(b"op", &[])));
    }

    /// Unix: the user and group databases are read for this process's own account and group.
    #[cfg(unix)]
    #[test]
    fn this_accounts_entries_are_read() {
        // SAFETY: no preconditions.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        let me = account(uid).expect("this account's entry");
        assert!(!me.name.is_empty());
        assert!(group(gid).is_some_and(|g| !g.name.is_empty()));
        assert_eq!(account(u32::MAX - 7), None);
    }

    /// Windows: an access-allowed ACE that lets another SID write, create, delete or change the
    /// DACL makes a folder writable by others — on the folder or for what is made in it — and
    /// so does no DACL at all; reading, a deny ACE and the trusted SIDs do not.
    #[cfg(windows)]
    #[test]
    fn a_dacl_that_lets_another_account_write_is_not_private() {
        let me = testing::this_user_sid().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        for (sddl, writable) in [
            (format!("D:P(A;OICI;FA;;;{me})"), false),
            (format!("D:P(A;OICI;FA;;;{me})(A;;FR;;;WD)"), false),
            (format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;;FA;;;OW)(A;OICIIO;GA;;;CO)"), false),
            (format!("D:P(D;;FA;;;WD)(A;OICI;FA;;;{me})"), false),
            (format!("D:P(A;OICI;FA;;;{me})(A;;0x2;;;WD)"), true),
            (format!("D:P(A;OICI;FA;;;{me})(A;;0x4;;;BU)"), true),
            (format!("D:P(A;OICI;FA;;;{me})(A;;0x40;;;AU)"), true),
            (format!("D:P(A;OICI;FA;;;{me})(A;;SD;;;WD)"), true),
            (format!("D:P(A;OICI;FA;;;{me})(A;;WD;;;WD)"), true),
            (format!("D:P(A;OICI;FA;;;{me})(A;;WO;;;WD)"), true),
            (format!("D:P(A;OICI;FA;;;{me})(A;;GW;;;WD)"), true),
            (format!("D:P(A;OICI;FA;;;{me})(A;OICIIO;GA;;;AU)"), true),
            (format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;S-1-5-21-1-2-3-1001)"), true),
            ("D:NO_ACCESS_CONTROL".to_string(), true),
        ] {
            testing::set_dacl(&folder, &sddl).unwrap();
            let c = control(&folder).unwrap();
            assert_eq!(c.writable_by_others, writable, "{sddl}: {c:?}");
            assert!(!c.foreign_owner, "{sddl}");
        }
        testing::set_dacl(&folder, &format!("D:P(A;OICI;FA;;;{me})")).unwrap();
    }

    /// Windows: an owner `restrict_dir` would refuse makes the folder foreign (the WI-473 seam,
    /// on a temp dir), the DACL read as it is.
    #[cfg(windows)]
    #[test]
    fn a_folder_under_a_foreign_owner_sid_is_foreign() {
        let dir = tempfile::tempdir().unwrap();
        let _foreign = testing::pretend_owner("S-1-5-21-1-2-3-1001").unwrap();
        let c = control(dir.path()).unwrap();
        assert!(c.foreign_owner && !c.writable_by_others, "{c:?}");
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

    // ---- WI-479: the folders AppRafter makes, owner-only whatever the umask. ----

    /// The umasks the folder tests run under: a shared group's (002, NixOS's `users`), none at
    /// all, the common one, and one that takes the owner's own write (0277).
    #[cfg(unix)]
    const UMASKS: [u32; 4] = [0o002, 0o000, 0o022, 0o277];

    /// Unix, in a child per umask: a new folder is 0700 — at its `mkdir` too, never wider for a
    /// moment (the recorder) — and so is every folder `create_dir_all_from` makes from its top
    /// down, while the folders it makes above the top have the umask's mode.
    #[cfg(unix)]
    #[test]
    fn new_folders_are_0700_whatever_the_umask_and_their_parents_follow_it() {
        for mask in UMASKS {
            let scratch = tempfile::tempdir().unwrap();
            under_umask::run(
                "owner_only::tests::new_folders_under_the_childs_umask",
                mask,
                scratch.path(),
            );
        }
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "runs only inside new_folders_are_0700_whatever_the_umask_and_their_parents_follow_it"]
    fn new_folders_under_the_childs_umask() {
        use std::os::unix::fs::PermissionsExt;
        let (mask, scratch) = (under_umask::mask(), under_umask::scratch());
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        let at_mkdir = 0o700 & !mask;

        let one = scratch.join("one");
        let _ = made::take();
        create_dir(&one).unwrap();
        assert_eq!(mode(&one), 0o700, "umask {mask:03o}");
        assert_eq!(made::take(), [(one.clone(), at_mkdir)], "umask {mask:03o}");

        // The top's parent is there: the top and everything under it are 0700.
        let top = scratch.join("store");
        let dir = top.join("targets").join("prod");
        create_dir_all_from(&top, &dir).unwrap();
        let levels = [top.clone(), top.join("targets"), dir.clone()];
        for level in &levels {
            assert_eq!(mode(level), 0o700, "{level:?} under umask {mask:03o}");
        }
        assert_eq!(
            made::take(),
            levels
                .iter()
                .map(|l| (l.clone(), at_mkdir))
                .collect::<Vec<_>>(),
            "umask {mask:03o}"
        );

        // Missing parents: made as the umask says. A umask that takes the owner's own write
        // leaves a parent no folder can be made in, AppRafter's or any other program's.
        if mask & 0o200 == 0 {
            let top = scratch.join("a").join("b").join("store");
            let dir = top.join("state").join("prod");
            create_dir_all_from(&top, &dir).unwrap();
            for parent in [scratch.join("a"), scratch.join("a").join("b")] {
                assert_eq!(
                    mode(&parent),
                    0o777 & !mask,
                    "{parent:?} under umask {mask:03o}"
                );
            }
            for level in [top.clone(), top.join("state"), dir] {
                assert_eq!(mode(&level), 0o700, "{level:?} under umask {mask:03o}");
            }
        }
    }

    /// A folder already there, at any level, is left as it is — never widened, never
    /// tightened: a store folder others can change is `store_privacy`'s to refuse — and only
    /// what is missing is made.
    #[cfg(unix)]
    #[test]
    fn a_folder_already_there_is_left_as_it_is() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        let top = scratch.path().join("store");
        let targets = top.join("targets");
        std::fs::create_dir_all(&targets).unwrap();
        std::fs::set_permissions(&top, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&targets, std::fs::Permissions::from_mode(0o775)).unwrap();
        create_dir_all_from(&top, &targets.join("prod")).unwrap();
        assert_eq!(mode(&top), 0o755);
        assert_eq!(mode(&targets), 0o775);
        assert_eq!(mode(&targets.join("prod")), 0o700);
        create_dir_all_from(&top, &targets).unwrap();
        assert_eq!(mode(&targets), 0o775, "a folder that is `dir` itself");
    }

    /// Windows: every folder `create_dir_all_from` makes from its top down has the owner-only
    /// DACL, the parents it makes above the top inherit theirs, and a folder already there
    /// keeps its own, wide or not.
    #[cfg(windows)]
    #[test]
    fn new_folders_are_owner_only_and_their_parents_inherit() {
        let scratch = tempfile::tempdir().unwrap();
        let top = scratch.path().join("a").join("b").join("store");
        let dir = top.join("targets").join("prod");
        create_dir_all_from(&top, &dir).unwrap();
        for parent in [scratch.path().join("a"), scratch.path().join("a").join("b")] {
            let dacl = testing::of_path(&parent).unwrap();
            assert!(!dacl.protected, "{parent:?} inherits: {dacl:?}");
        }
        for level in [top.clone(), top.join("targets"), dir] {
            testing::of_path(&level)
                .unwrap()
                .assert_owner_only_dir(&level.display().to_string());
        }

        let me = testing::this_user_sid().unwrap();
        let wide = scratch.path().join("wide");
        std::fs::create_dir(&wide).unwrap();
        testing::set_dacl(
            &wide,
            &format!("D:P(A;OICI;FA;;;{me})(A;OICI;0x1301bf;;;BU)"),
        )
        .unwrap();
        let before = testing::of_path(&wide).unwrap();
        create_dir_all_from(&wide, &wide.join("targets")).unwrap();
        assert_eq!(testing::of_path(&wide).unwrap(), before, "left as it was");
        testing::of_path(&wide.join("targets"))
            .unwrap()
            .assert_owner_only_dir("a folder made in a wide one");
    }

    /// `dir` must be under `top`; a file in the way is the error, not a folder.
    #[test]
    fn create_dir_all_from_refuses_what_it_cannot_make() {
        let scratch = tempfile::tempdir().unwrap();
        let top = scratch.path().join("store");
        let elsewhere = scratch.path().join("elsewhere");
        let e = create_dir_all_from(&top, &elsewhere).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(!top.exists() && !elsewhere.exists());
        std::fs::create_dir(&top).unwrap();
        std::fs::write(top.join("targets"), b"x").unwrap();
        assert!(create_dir_all_from(&top, &top.join("targets").join("prod")).is_err());
        assert_eq!(std::fs::read(top.join("targets")).unwrap(), b"x");
    }
}
