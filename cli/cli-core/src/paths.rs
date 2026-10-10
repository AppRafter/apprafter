// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The one home-directory lookup the shared core may reach, sanctioned for its two context
//! builders only (`apprafter_core::Context::from_cli_env` and `from_desktop_env`, guarded by
//! `apprafter-core/tests/guards.rs`), and the pure `~/` rendering both clients show.

use std::path::{Path, PathBuf};

/// The account's home directory, as [`dirs::home_dir`] resolves it: `$HOME` on Unix when it is
/// set and non-empty, else the password database; the user profile on Windows.
pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
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
}
