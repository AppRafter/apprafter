// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! What the webview learns about the app once, at start: OS, versions, who and where.

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

/// Where the target store keeps its secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum SecretBackend {
    File,
    Keyring,
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
    pub secret_backend: SecretBackend,
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
            secret_backend: SecretBackend::File,
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
            r#"{"os":"linux","desktopVersion":"0.1.0","coreVersion":"0.2.80","secretBackend":"file","account":"rem","host":"box","auth":{"available":false,"method":null,"unavailable":"no_backend","biometricsChoice":false,"passwordField":false},"sessionEvents":{"lock":true,"sleep":false},"testBuild":false,"settingsNotice":null}"#
        );
    }

    #[test]
    fn os_and_secret_backend_are_lowercase() {
        assert_eq!(serde_json::to_value(Os::Windows).unwrap(), "windows");
        assert_eq!(serde_json::to_value(Os::Macos).unwrap(), "macos");
        assert_eq!(serde_json::to_value(Os::Linux).unwrap(), "linux");
        assert_eq!(serde_json::to_value(SecretBackend::File).unwrap(), "file");
        assert_eq!(
            serde_json::to_value(SecretBackend::Keyring).unwrap(),
            "keyring"
        );
    }
}
