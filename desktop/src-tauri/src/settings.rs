// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The desktop's preferences in `settings.json` (design spec §4.6), owned by Rust because the
//! lock, the tray and the notifier read them too.
//!
//! Loading never fails the app. A missing file reads as the defaults. Every other way it falls
//! back to the defaults leaves a [`notice`](SettingsStore::notice) for the owner that says
//! what happened to the file:
//!
//! - unreadable as settings — not JSON, not an object, a value this build does not know, a
//!   `version` that is no number: moved aside to `settings.json.corrupt-<ms>`, so the next save
//!   does not overwrite it (when it cannot be moved, it is left in place, and the notice says
//!   the next change overwrites it);
//! - not readable at all (an I/O error, such as a permission): left in place, since the
//!   settings in it may be fine, and the notice names the error and says the next change
//!   overwrites it;
//! - written by a newer AppRafter (a higher `version`): left as it is until the owner's next
//!   explicit [`set`](SettingsStore::set) replaces it, which the notice says.
//!
//! [`set`](SettingsStore::set) writes the whole file through [`cli_core::atomic_replace`] (a
//! reader sees the old file or the new one, never a partial write) with mode 0600, as pretty
//! JSON with a trailing newline, always under [`Settings::CURRENT_VERSION`].

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use apprafter_desktop_ipc::Settings;
use serde::Deserialize;

use crate::errors::DesktopError;
use crate::ops::Clock;

/// The file's name inside the data directory.
pub const FILE_NAME: &str = "settings.json";

pub struct SettingsStore {
    path: PathBuf,
    inner: Mutex<Inner>,
}

struct Inner {
    current: Settings,
    /// What the owner should know about the file as it was loaded; cleared once a change
    /// has replaced that file.
    notice: Option<String>,
}

/// What reading the file found.
enum Read {
    Missing,
    Loaded(Settings),
    Newer(u64),
    /// Read, but not as settings: why.
    Unreadable(String),
    /// Not read: the I/O error.
    Inaccessible(String),
}

impl SettingsStore {
    /// Read `<dir>/settings.json`. `clock` names the file an unreadable one is moved to.
    pub fn load(dir: &Path, clock: &dyn Clock) -> Self {
        let path = dir.join(FILE_NAME);
        let (current, notice) = match read(&path) {
            Read::Missing => (Settings::default(), None),
            Read::Loaded(settings) => (settings, None),
            Read::Newer(version) => (
                Settings::default(),
                Some(format!(
                    "{FILE_NAME} is version {version}, newer than this AppRafter reads \
                     (version {}); defaults are in use and the file is left as it is until \
                     the next change replaces it",
                    Settings::CURRENT_VERSION
                )),
            ),
            Read::Unreadable(reason) => {
                let aside = format!("{FILE_NAME}.corrupt-{}", clock.now_ms());
                let notice = match std::fs::rename(&path, dir.join(&aside)) {
                    Ok(()) => format!(
                        "{FILE_NAME} was unreadable ({reason}); defaults are in use, and the \
                         file was moved to {aside}."
                    ),
                    Err(e) => format!(
                        "{FILE_NAME} was unreadable ({reason}); defaults are in use. Moving it \
                         to {aside} failed ({e}), so it was left in place and the next change \
                         overwrites it."
                    ),
                };
                (Settings::default(), Some(notice))
            }
            // Not moved: nothing says the settings in it are bad.
            Read::Inaccessible(error) => (
                Settings::default(),
                Some(format!(
                    "{FILE_NAME} could not be read ({error}); defaults are in use. The file was \
                     left in place, and the next change overwrites it."
                )),
            ),
        };
        Self {
            path,
            inner: Mutex::new(Inner { current, notice }),
        }
    }

    pub fn get(&self) -> Settings {
        self.lock().current.clone()
    }

    /// Save `settings` and make them current; on an error neither the file nor the current
    /// settings change. The `version` given is ignored: this build writes its own.
    pub fn set(&self, settings: Settings) -> Result<(), DesktopError> {
        let settings = Settings {
            version: Settings::CURRENT_VERSION,
            ..settings
        };
        let mut bytes = serde_json::to_vec_pretty(&settings)
            .map_err(|e| DesktopError::Internal(format!("settings did not serialise: {e}")))?;
        bytes.push(b'\n');
        // Held across the write, so the file and `current` always agree.
        let mut inner = self.lock();
        save(&self.path, &bytes)
            .map_err(|e| DesktopError::SettingsIo(format!("{}: {e}", self.path.display())))?;
        inner.current = settings;
        inner.notice = None;
        Ok(())
    }

    /// Why the settings in use are not the ones in the file, if they are not.
    pub fn notice(&self) -> Option<String> {
        self.lock().notice.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

fn read(path: &Path) -> Read {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Read::Missing,
        Err(e) => return Read::Inaccessible(e.to_string()),
    };
    let value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(e) => return Read::Unreadable(e.to_string()),
    };
    // Not an object: serde would read `[]` as a struct of defaults.
    let Some(object) = value.as_object() else {
        return Read::Unreadable("not a JSON object".into());
    };
    // Before the values: a newer file may hold values this build has never heard of.
    if let Some(version) = object.get("version").and_then(serde_json::Value::as_u64) {
        if version > u64::from(Settings::CURRENT_VERSION) {
            return Read::Newer(version);
        }
    }
    match Settings::deserialize(&value) {
        // There is no older format: what this build reads is the current one.
        Ok(settings) => Read::Loaded(Settings {
            version: Settings::CURRENT_VERSION,
            ..settings
        }),
        Err(e) => Read::Unreadable(e.to_string()),
    }
}

fn save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    cli_core::atomic_replace(path, bytes, ".settings.json.", Some(0o600))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use apprafter_desktop_ipc::{AutoLock, Settings, Theme};

    use super::{SettingsStore, FILE_NAME};
    use crate::errors::DesktopError;
    use crate::ops::test_clock::ManualClock;

    const T0: u64 = 1_700_000_000_000;

    fn load(dir: &Path) -> SettingsStore {
        SettingsStore::load(dir, &ManualClock::at(T0))
    }

    /// Every name in `dir`, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn changed() -> Settings {
        Settings {
            theme: Theme::Light,
            lock_on_start: false,
            auto_lock: AutoLock::Min30,
            ..Settings::default()
        }
    }

    fn corrupt_name() -> String {
        format!("{FILE_NAME}.corrupt-{T0}")
    }

    #[test]
    fn a_missing_file_reads_as_the_defaults_without_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        let store = load(dir.path());
        assert_eq!(store.get(), Settings::default());
        assert_eq!(store.notice(), None);
        assert!(names(dir.path()).is_empty(), "loading writes nothing");
    }

    #[test]
    fn a_valid_file_is_loaded() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(FILE_NAME),
            r#"{"version":1,"theme":"light","lockOnStart":false,"autoLock":"30"}"#,
        )
        .unwrap();
        let store = load(dir.path());
        assert_eq!(store.get(), changed());
        assert_eq!(store.notice(), None);
    }

    #[test]
    fn a_corrupt_file_reads_as_the_defaults_and_is_moved_aside() {
        for (what, content, reason) in [
            ("not JSON", "{\"theme\": ", "EOF while parsing"),
            ("an unknown value", r#"{"theme":"sepia"}"#, "sepia"),
            ("not an object", "[]", "not a JSON object"),
            ("a version that is no number", r#"{"version":"two"}"#, "two"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join(FILE_NAME), content).unwrap();
            let store = load(dir.path());
            assert_eq!(store.get(), Settings::default(), "{what}");
            let notice = store
                .notice()
                .unwrap_or_else(|| panic!("{what}: no notice"));
            assert!(
                notice.starts_with("settings.json was unreadable ("),
                "{what}: {notice}"
            );
            assert!(notice.contains(reason), "{what}: {notice}");
            // It says what happened, and only that: the file is out of the way, not replaced.
            assert!(
                notice.ends_with(
                    "); defaults are in use, and the file was moved to \
                     settings.json.corrupt-1700000000000."
                ),
                "{what}: {notice}"
            );
            // Kept for the owner, byte for byte, and out of the way of the next save.
            assert_eq!(names(dir.path()), vec![corrupt_name()], "{what}");
            assert_eq!(
                fs::read_to_string(dir.path().join(corrupt_name())).unwrap(),
                content,
                "{what}"
            );
            store.set(changed()).unwrap();
            assert_eq!(
                names(dir.path()),
                vec![FILE_NAME.to_string(), corrupt_name()],
                "{what}"
            );
            assert_eq!(
                fs::read_to_string(dir.path().join(corrupt_name())).unwrap(),
                content,
                "{what}: the next save must not touch the kept file"
            );
        }
    }

    #[test]
    fn an_unreadable_file_that_cannot_be_moved_aside_says_the_next_change_overwrites_it() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE_NAME), "garbage").unwrap();
        // A non-empty directory under the name it would be moved to: every OS refuses that.
        let occupied = dir.path().join(corrupt_name());
        fs::create_dir(&occupied).unwrap();
        fs::write(occupied.join("inside"), "x").unwrap();
        let store = load(dir.path());
        assert_eq!(store.get(), Settings::default());
        let notice = store.notice().unwrap();
        assert!(
            notice.starts_with("settings.json was unreadable ("),
            "{notice}"
        );
        assert!(
            notice.contains(
                "); defaults are in use. Moving it to settings.json.corrupt-1700000000000 \
                 failed ("
            ),
            "{notice}"
        );
        assert!(
            notice.ends_with("), so it was left in place and the next change overwrites it."),
            "{notice}"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(FILE_NAME)).unwrap(),
            "garbage",
            "left where it was"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_be_read_is_left_in_place_for_the_next_change_to_overwrite() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        // Settings the owner may want back: an error reading them says nothing about them.
        let valid = r#"{"version":1,"theme":"light"}"#;
        fs::write(&path, valid).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(
            fs::read(&path).is_err(),
            "this test cannot run as root: root reads a mode-000 file, so there is no read \
             error to test"
        );
        let store = load(dir.path());
        assert_eq!(store.get(), Settings::default());
        let notice = store.notice().expect("a notice");
        assert!(
            notice.starts_with("settings.json could not be read ("),
            "{notice}"
        );
        assert!(
            notice.contains("Permission denied"),
            "names the error: {notice}"
        );
        assert!(
            notice.ends_with(
                "); defaults are in use. The file was left in place, and the next change \
                 overwrites it."
            ),
            "{notice}"
        );
        assert_eq!(
            names(dir.path()),
            vec![FILE_NAME.to_string()],
            "not moved aside"
        );
        // As the notice says.
        store.set(changed()).unwrap();
        assert_eq!(store.notice(), None);
        assert_eq!(load(dir.path()).get(), changed());
        assert_eq!(names(dir.path()), vec![FILE_NAME.to_string()]);
    }

    #[test]
    fn a_newer_file_reads_as_the_defaults_and_is_left_alone_until_the_next_change() {
        let dir = tempfile::tempdir().unwrap();
        // Version 2 may have values this build cannot parse: it must not be called corrupt.
        let newer = r#"{"version":2,"theme":"sepia","lockEnabled":false}"#;
        fs::write(dir.path().join(FILE_NAME), newer).unwrap();
        let store = load(dir.path());
        assert_eq!(store.get(), Settings::default());
        assert_eq!(
            store.notice().as_deref(),
            Some(
                "settings.json is version 2, newer than this AppRafter reads (version 1); \
                 defaults are in use and the file is left as it is until the next change \
                 replaces it"
            )
        );
        assert_eq!(names(dir.path()), vec![FILE_NAME.to_string()]);
        assert_eq!(
            fs::read_to_string(dir.path().join(FILE_NAME)).unwrap(),
            newer,
            "neither renamed nor rewritten by loading"
        );
        store.set(changed()).unwrap();
        assert_eq!(load(dir.path()).get(), changed(), "the change replaced it");
    }

    #[test]
    fn set_saves_atomically_into_a_new_directory_and_a_reload_reads_it_back() {
        let root = tempfile::tempdir().unwrap();
        let dir: PathBuf = root.path().join("not").join("yet");
        let store = load(&dir);
        store.set(changed()).unwrap();
        assert_eq!(store.get(), changed());
        assert_eq!(
            names(&dir),
            vec![FILE_NAME.to_string()],
            "no temp file left"
        );
        let text = fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert_eq!(
            text,
            format!("{}\n", serde_json::to_string_pretty(&changed()).unwrap()),
            "pretty JSON with a trailing newline"
        );
        let reloaded = load(&dir);
        assert_eq!(reloaded.get(), changed());
        assert_eq!(reloaded.notice(), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = load(dir.path());
        store.set(changed()).unwrap();
        let mode = fs::metadata(dir.path().join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn set_writes_the_current_version_whatever_it_was_given() {
        let dir = tempfile::tempdir().unwrap();
        let store = load(dir.path());
        store
            .set(Settings {
                version: 9,
                ..changed()
            })
            .unwrap();
        assert_eq!(store.get().version, Settings::CURRENT_VERSION);
        assert_eq!(load(dir.path()).get(), changed());
    }

    #[test]
    fn a_successful_change_clears_the_notice() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE_NAME), "garbage").unwrap();
        let store = load(dir.path());
        assert!(store.notice().is_some());
        store.set(changed()).unwrap();
        assert_eq!(store.notice(), None, "the file it described is gone");
    }

    #[test]
    fn a_failed_save_is_a_settings_error_and_changes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("dir");
        let store = load(&dir);
        // The directory the file goes in turns out to be a file: neither creating it nor
        // writing into it works, on any OS.
        fs::write(&dir, "x").unwrap();
        let err = store.set(changed()).unwrap_err();
        assert!(matches!(err, DesktopError::SettingsIo(_)), "{err:?}");
        assert_eq!(store.get(), Settings::default());
    }
}
