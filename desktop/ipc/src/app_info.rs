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
    /// The OS reports its session's locks and sleeps to the app, so `lockOnSleep` can work: the
    /// watch on them listens. `false` while it does not (yet): the settings show the row
    /// disabled, with the reason. Final in the first answer — `app_info` waits a short, bounded
    /// time for the watch to say.
    pub session_events: bool,
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
            session_events: true,
            test_build: false,
            settings_notice: None,
        };
        assert_eq!(
            serde_json::to_string(&info).unwrap(),
            r#"{"os":"linux","desktopVersion":"0.1.0","coreVersion":"0.2.80","secretBackend":"file","account":"rem","host":"box","auth":{"available":false,"method":null,"unavailable":"no_backend","biometricsChoice":false,"passwordField":false},"sessionEvents":true,"testBuild":false,"settingsNotice":null}"#
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
