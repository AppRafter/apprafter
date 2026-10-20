// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The lookups the shared core may reach, each sanctioned for named context builders only
//! (`apprafter-core/tests/guards.rs`): the home directory, for `Context::from_cli_env` and
//! `from_desktop_env`; the local data directory, for the CLI's Windows runtime dir (D.4a T44);
//! and macOS's per-user temporary directory, for `from_cli_env` (D.4a T49). Also the pure `~/`
//! rendering both clients show.

use std::path::{Path, PathBuf};

/// The account's home directory, as [`dirs::home_dir`] resolves it: `$HOME` on Unix when it is
/// set and non-empty, else the password database; the user profile on Windows.
pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// The account's local, never-roamed data directory, as [`dirs::data_local_dir`] resolves it:
/// `%LOCALAPPDATA%` (the Known Folder) on Windows. The CLI's runtime dir sits under it there
/// when no store variable is set (D.4a T44).
pub fn local_data_dir() -> Option<PathBuf> {
    dirs::data_local_dir()
}

/// `path` with a leading `home` collapsed to `~/`; unchanged when it is not under `home` or
/// there is no home.
pub fn abbreviate_home(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if let Ok(rest) = path.strip_prefix(home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

/// macOS's per-user temporary directory (`confstr(_CS_DARWIN_USER_TEMP_DIR)`: per account,
/// 0700, cleaned of files unused for three days), which no environment variable moves: the
/// last base of a terminal's holder when neither `XDG_RUNTIME_DIR` nor `TMPDIR` is set (D.4a
/// T49). `None` when the lookup fails. The one `confstr` call in the workspace, counted by the
/// core's env-read ratchet (W5).
#[cfg(target_os = "macos")]
pub fn darwin_user_temp_dir() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    // PATH_MAX: the lookup answers a path, NUL included.
    let mut buf = [0u8; 1024];
    // SAFETY: confstr writes at most `buf.len()` bytes, NUL included, into `buf`, and returns
    // the length it needed (0 on failure).
    let needed = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if needed == 0 || needed > buf.len() {
        return None;
    }
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..needed - 1]));
    path.is_absolute().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbreviate_home_collapses_the_home_prefix() {
        let home = Path::new("/home/op");
        assert_eq!(
            abbreviate_home(Path::new("/home/op/.ssh/id.pub"), Some(home)),
            "~/.ssh/id.pub"
        );
        assert_eq!(abbreviate_home(Path::new("/etc/x"), Some(home)), "/etc/x");
        assert_eq!(abbreviate_home(Path::new("/home/op/x"), None), "/home/op/x");
    }

    /// D.4a T49: macOS's per-user temporary directory exists for every account (`confstr` never
    /// fails for `_CS_DARWIN_USER_TEMP_DIR` on a logged-in account) and is absolute.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_darwin_user_temp_dir_is_an_absolute_directory() {
        let dir = darwin_user_temp_dir().expect("confstr(_CS_DARWIN_USER_TEMP_DIR)");
        assert!(dir.is_absolute() && dir.is_dir(), "{dir:?}");
    }
}
