// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The desktop's own preferences, `settings.json` (design spec §4.6).

use serde::{Deserialize, Serialize};

/// Every preference the desktop keeps; read from the webview and written back by it.
///
/// A file missing some keys reads with the defaults for them (`#[serde(default)]`); an
/// unknown value for a known key is an error, never a silent default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// The file format; [`Settings::CURRENT_VERSION`] today.
    pub version: u32,
    pub theme: Theme,
    pub lock_enabled: bool,
    pub lock_on_start: bool,
    pub lock_on_sleep: bool,
    /// Windows Hello when it is available, rather than the account password.
    pub hello: bool,
    pub auto_lock: AutoLock,
    pub refresh: Refresh,
    /// Pause refreshing while the window is hidden.
    pub pause_hidden: bool,
    pub os_notify: bool,
    pub tray_badge: bool,
    pub close_to_tray: bool,
}

impl Settings {
    /// The `version` this build writes.
    pub const CURRENT_VERSION: u32 = 1;
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: Self::CURRENT_VERSION,
            theme: Theme::Dark,
            lock_enabled: true,
            lock_on_start: true,
            lock_on_sleep: true,
            hello: true,
            auto_lock: AutoLock::Min10,
            refresh: Refresh::S5,
            pause_hidden: true,
            os_notify: true,
            tray_badge: true,
            close_to_tray: true,
        }
    }
}

/// The colour scheme; `system` follows the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    System,
    Light,
    Dark,
}

/// How long without activity before the app locks itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum AutoLock {
    #[serde(rename = "5")]
    Min5,
    #[serde(rename = "10")]
    Min10,
    #[serde(rename = "30")]
    Min30,
    #[serde(rename = "never")]
    Never,
}

impl AutoLock {
    /// The idle time in minutes; `None` for never.
    pub fn minutes(self) -> Option<u32> {
        match self {
            AutoLock::Min5 => Some(5),
            AutoLock::Min10 => Some(10),
            AutoLock::Min30 => Some(30),
            AutoLock::Never => None,
        }
    }
}

/// How often the open views poll the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum Refresh {
    #[serde(rename = "5")]
    S5,
    #[serde(rename = "15")]
    S15,
    #[serde(rename = "30")]
    S30,
    #[serde(rename = "60")]
    S60,
}

impl Refresh {
    /// The polling interval in seconds.
    pub fn seconds(self) -> u32 {
        match self {
            Refresh::S5 => 5,
            Refresh::S15 => 15,
            Refresh::S30 => 30,
            Refresh::S60 => 60,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_serialise_to_the_spec_wire_shape() {
        assert_eq!(
            serde_json::to_string(&Settings::default()).unwrap(),
            r#"{"version":1,"theme":"dark","lockEnabled":true,"lockOnStart":true,"lockOnSleep":true,"hello":true,"autoLock":"10","refresh":"5","pauseHidden":true,"osNotify":true,"trayBadge":true,"closeToTray":true}"#
        );
    }

    #[test]
    fn missing_keys_take_their_defaults() {
        let partial: Settings =
            serde_json::from_str(r#"{"theme":"light","autoLock":"never"}"#).unwrap();
        assert_eq!(
            partial,
            Settings {
                theme: Theme::Light,
                auto_lock: AutoLock::Never,
                ..Settings::default()
            }
        );
        let empty: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, Settings::default());
    }

    #[test]
    fn settings_round_trip() {
        let s = Settings {
            theme: Theme::System,
            lock_enabled: false,
            auto_lock: AutoLock::Min30,
            refresh: Refresh::S60,
            close_to_tray: false,
            ..Settings::default()
        };
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn an_unknown_value_is_an_error_not_a_default() {
        let err = serde_json::from_str::<Settings>(r#"{"theme":"sepia"}"#).unwrap_err();
        assert!(err.to_string().contains("sepia"), "{err}");
        // The wire carries choices as strings, so a bare number is not one of them.
        assert!(serde_json::from_str::<Settings>(r#"{"autoLock":10}"#).is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"refresh":"10"}"#).is_err());
    }

    #[test]
    fn every_auto_lock_choice_has_its_wire_name_and_duration() {
        for (choice, wire, minutes) in [
            (AutoLock::Min5, r#""5""#, Some(5)),
            (AutoLock::Min10, r#""10""#, Some(10)),
            (AutoLock::Min30, r#""30""#, Some(30)),
            (AutoLock::Never, r#""never""#, None),
        ] {
            assert_eq!(serde_json::to_string(&choice).unwrap(), wire);
            assert_eq!(serde_json::from_str::<AutoLock>(wire).unwrap(), choice);
            assert_eq!(choice.minutes(), minutes, "{choice:?}");
        }
    }

    #[test]
    fn every_refresh_choice_has_its_wire_name_and_interval() {
        for (choice, wire, seconds) in [
            (Refresh::S5, r#""5""#, 5),
            (Refresh::S15, r#""15""#, 15),
            (Refresh::S30, r#""30""#, 30),
            (Refresh::S60, r#""60""#, 60),
        ] {
            assert_eq!(serde_json::to_string(&choice).unwrap(), wire);
            assert_eq!(serde_json::from_str::<Refresh>(wire).unwrap(), choice);
            assert_eq!(choice.seconds(), seconds, "{choice:?}");
        }
    }

    #[cfg(feature = "ts")]
    #[test]
    fn the_typescript_declaration_keeps_the_wire_names() {
        use ts_rs::TS;
        let cfg = ts_rs::Config::new().with_large_int("number");
        let settings = Settings::decl(&cfg);
        for field in [
            "version: number",
            "lockEnabled: boolean",
            "autoLock: AutoLock",
            "closeToTray: boolean",
        ] {
            assert!(settings.contains(field), "{field} missing from {settings}");
        }
        // `#[serde(default)]` is for reading old files; settings_get always sends every key.
        assert!(!settings.contains("?:"), "{settings}");
        let auto_lock = AutoLock::decl(&cfg);
        assert!(
            auto_lock.contains(r#""5" | "10" | "30" | "never""#),
            "{auto_lock}"
        );
        let refresh = Refresh::decl(&cfg);
        assert!(refresh.contains(r#""5" | "15" | "30" | "60""#), "{refresh}");
    }
}
