// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the webview learns about the app once, at start: OS, versions, who and where.

use apprafter_core::secrets::{LockSlotKind, SecretsMode};
use apprafter_core::UiError;
use serde::Serialize;

use crate::AuthInfo;

/// The OS the app runs on, for per-OS chrome and wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Windows,
    Macos,
    Linux,
}

/// Where the target store keeps its secrets, as the lock screen, the Target screen and Settings ›
/// Lock show it (D.4a §3.14): in files, or in a vault and what opens it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SecretsInfo {
    pub mode: SecretsMode,
    /// What opens the vault, as `store.yaml` lists it; empty in files.
    pub slots: Vec<LockSlotKind>,
    /// The app's unlock opens the store's vault (D.4a T74, T75): the secrets are not in files,
    /// the store has an id and its vault is there — or `store.yaml` cannot be read, which
    /// counts as locked. The lock's settings are pinned meanwhile (O7), and the lock screen
    /// takes the lock password or the recovery key.
    pub protected: bool,
    /// The store's root, home-abbreviated as the app shows paths.
    pub store: String,
    /// Why this store's lock state cannot be used, in the core's words (rule 1): its
    /// `store.yaml` cannot be read (`store_file_unreadable`; the store counts as locked, fail
    /// closed), or it is locked and its vault is gone (`vault_unreadable`, `missing`: O8). Never
    /// shown as "encrypted". `None` otherwise, an interrupted run included.
    pub problem: Option<UiError>,
}

/// Which of the OS session's signals the app hears, so `lockOnSleep` has something to follow:
/// the session locking, the machine going to sleep. Both false: the OS reports neither here
/// (no bus on Linux, as in WSL or a container), and the setting can do nothing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SessionEvents {
    /// The session's locks reach the app.
    pub lock: bool,
    /// Sleeps reach the app.
    pub sleep: bool,
}

/// The `app_info` answer: the About line, the account shown in the title bar, auth support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub os: Os,
    pub desktop_version: String,
    /// `apprafter_core::VERSION`.
    pub core_version: String,
    /// Where the target store keeps its secrets, read from `store.yaml` on every call.
    pub secrets: SecretsInfo,
    /// The OS user name.
    pub account: String,
    /// The host name, `unknown` when the OS does not say.
    pub host: String,
    pub auth: AuthInfo,
    /// What the OS reports of its session to the app, for `lockOnSleep`. Both false while
    /// nothing listens, or not yet: the settings show the row disabled, with the reason. The
    /// first answer waits a short, bounded time for the watch to say; a watch that says later
    /// counts from then on, so a later `app_info` may say more.
    pub session_events: SessionEvents,
    /// A test build (fake authentication); the UI shows a TEST BUILD banner.
    pub test_build: bool,
    /// Why the settings are defaults this run (an unreadable or newer `settings.json`).
    pub settings_notice: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UnavailableReason;

    #[test]
    fn app_info_has_the_wire_shape() {
        let info = AppInfo {
            os: Os::Linux,
            desktop_version: "0.1.0".into(),
            core_version: "0.2.80".into(),
            secrets: SecretsInfo {
                mode: SecretsMode::Files,
                slots: Vec::new(),
                protected: false,
                store: "~/.config/apprafter".into(),
                problem: None,
            },
            account: "rem".into(),
            host: "box".into(),
            auth: AuthInfo {
                available: false,
                method: None,
                unavailable: Some(UnavailableReason::NoBackend),
                biometrics_choice: false,
                password_field: false,
            },
            session_events: SessionEvents {
                lock: true,
                sleep: false,
            },
            test_build: false,
            settings_notice: None,
        };
        assert_eq!(
            serde_json::to_string(&info).unwrap(),
            r#"{"os":"linux","desktopVersion":"0.1.0","coreVersion":"0.2.80","secrets":{"mode":"files","slots":[],"protected":false,"store":"~/.config/apprafter","problem":null},"account":"rem","host":"box","auth":{"available":false,"method":null,"unavailable":"no_backend","biometricsChoice":false,"passwordField":false},"sessionEvents":{"lock":true,"sleep":false},"testBuild":false,"settingsNotice":null}"#
        );
    }

    #[test]
    fn os_is_lowercase_and_secrets_name_the_mode_and_its_slots() {
        assert_eq!(serde_json::to_value(Os::Windows).unwrap(), "windows");
        assert_eq!(serde_json::to_value(Os::Macos).unwrap(), "macos");
        assert_eq!(serde_json::to_value(Os::Linux).unwrap(), "linux");
        assert_eq!(
            serde_json::to_value(SecretsInfo {
                mode: SecretsMode::Locked,
                slots: vec![LockSlotKind::Password, LockSlotKind::RecoveryKey],
                protected: true,
                store: "~/s".into(),
                problem: None,
            })
            .unwrap(),
            serde_json::json!({"mode": "locked", "slots": ["password", "recovery_key"], "protected": true, "store": "~/s", "problem": null})
        );
    }
}
