// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The native window's colour scheme, which the page sets through `theme_apply` with the theme
//! setting: Light and Dark are applied as they are, on every OS. System differs.
//!
//! - **macOS and Windows:** `set_theme(None)` leaves the window, and the webview's
//!   `prefers-color-scheme` with it, to the OS, which keeps them current by itself.
//! - **Linux:** tao's `set_theme(None)` sets GTK's `gtk-application-prefer-dark-theme` to
//!   `false` (tao 0.37 `event_loop.rs`: `theme == Some(Theme::Dark)`), which forces light on a
//!   dark desktop. So the app resolves System itself ([`linux_system_theme`]): the desktop's
//!   colour scheme as the XDG desktop portal reports it (`org.freedesktop.appearance`
//!   `color-scheme`: 1 dark, 2 light, 0 no preference), else GTK's own preference as it stood at
//!   start (`~/.config/gtk-3.0/settings.ini`, which KDE Plasma keeps in step), else light. It
//!   applies the result with `set_theme(Some(..))`, at start before the window exists
//!   ([`start`]), on `theme_apply`, and, while the setting is System, on every change the portal
//!   signals and on its answer when a portal starts ([`on_desktop_change`], [`portal`]).
//!
//! The page decides its own `data-theme` from `matchMedia('(prefers-color-scheme: dark)')`.
//! WebKitGTK follows `gtk-application-prefer-dark-theme` live: it listens for the property's
//! change notification and re-evaluates the page's media queries, so the page's listener hears
//! each change (WebKitGTK 2.52, `UIProcess/gtk/SystemSettingsManagerProxyGtk.cpp`: `darkMode()`
//! reads the property and `notify::gtk-application-prefer-dark-theme` calls
//! `settingsDidChange`; `WebKitWebViewBase.cpp` then calls `effectiveAppearanceDidChange`, and
//! `Page::appearanceDidChange` re-evaluates media queries).
//!
//! tao reads the portal on its own too, behind its `dbus` feature: once at window creation,
//! blocking the main thread for up to 5 s, and again on every change, when it forces the
//! portal's reading over whatever the app set — Light or Dark turned with the desktop, and "no
//! preference" became light whatever GTK said. `desktop/Cargo.toml` leaves Tauri's `dbus`
//! feature off, so the theme has one owner: this module.

use std::sync::{Mutex, MutexGuard, PoisonError};

use apprafter_desktop_ipc::Theme;
use tauri::Theme as NativeTheme;

#[cfg(target_os = "linux")]
pub mod portal;

/// The portal's `color-scheme` for a dark appearance.
pub const PORTAL_DARK: u32 = 1;
/// The portal's `color-scheme` for a light appearance; 0 is no preference.
pub const PORTAL_LIGHT: u32 = 2;

/// System, resolved on Linux: the portal's `color-scheme` when it says dark or light;
/// otherwise — no preference (0), a value the portal does not define, or no answer — GTK's
/// preference for a dark theme as it stood at start; without that, light.
pub fn linux_system_theme(portal: Option<u32>, gtk_prefers_dark: Option<bool>) -> NativeTheme {
    match portal {
        Some(PORTAL_DARK) => NativeTheme::Dark,
        Some(PORTAL_LIGHT) => NativeTheme::Light,
        _ if gtk_prefers_dark == Some(true) => NativeTheme::Dark,
        _ => NativeTheme::Light,
    }
}

/// What the native window gets for `setting`: an explicit theme as it is; System as the
/// platform resolves it, `system` — the resolved theme on Linux, `None` (the OS's own) on macOS
/// and Windows.
pub fn native_theme(setting: Theme, system: Option<NativeTheme>) -> Option<NativeTheme> {
    match setting {
        Theme::Light => Some(NativeTheme::Light),
        Theme::Dark => Some(NativeTheme::Dark),
        Theme::System => system,
    }
}

/// The re-apply rule on Linux: a change of the desktop's colour scheme reaches the window only
/// while the setting is System, as what System now resolves to; under Light or Dark, `None`.
pub fn on_desktop_change(
    setting: Theme,
    portal: Option<u32>,
    gtk_prefers_dark: Option<bool>,
) -> Option<NativeTheme> {
    (setting == Theme::System).then(|| linux_system_theme(portal, gtk_prefers_dark))
}

/// How a theme the app decided reaches the window: `AppHandle::set_theme` in the app.
pub type Apply = Box<dyn Fn(NativeTheme) + Send + Sync>;

/// The theme state `theme_apply` and the portal's watch share. One lock: the window gets the
/// themes in the order they were decided, and the last decided is the one that stays.
pub struct Appearance {
    inner: Mutex<Inner>,
}

struct Inner {
    /// The setting the window follows: the stored one at start, then `theme_apply`'s.
    setting: Theme,
    /// Linux: the portal's latest `color-scheme`; `None` before it answered, or without it.
    #[cfg(target_os = "linux")]
    portal: Option<u32>,
    /// Linux: GTK's preference for a dark theme at start; `None` until [`Appearance::start`].
    #[cfg(target_os = "linux")]
    gtk_prefers_dark: Option<bool>,
    /// Linux: how a change of the desktop reaches the window; `None` until
    /// [`Appearance::start`].
    #[cfg(target_os = "linux")]
    apply: Option<Apply>,
}

impl Inner {
    /// What System resolves to on Linux now.
    #[cfg(target_os = "linux")]
    fn system(&self) -> Option<NativeTheme> {
        Some(linux_system_theme(self.portal, self.gtk_prefers_dark))
    }

    /// What System resolves to on macOS and Windows: the OS's own.
    #[cfg(not(target_os = "linux"))]
    fn system(&self) -> Option<NativeTheme> {
        None
    }
}

impl Appearance {
    /// The state for a window that follows `setting` (the stored one).
    pub fn new(setting: Theme) -> Self {
        Appearance {
            inner: Mutex::new(Inner {
                setting,
                #[cfg(target_os = "linux")]
                portal: None,
                #[cfg(target_os = "linux")]
                gtk_prefers_dark: None,
                #[cfg(target_os = "linux")]
                apply: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The setting the window follows.
    pub fn setting(&self) -> Theme {
        self.lock().setting
    }

    /// `theme_apply`: follow `setting` from now on, and hand `apply` the window's theme for it
    /// ([`native_theme`]), under the lock — so a change of the desktop decided meanwhile lands
    /// before it or after it, never in between.
    pub fn apply<T>(&self, setting: Theme, apply: impl FnOnce(Option<NativeTheme>) -> T) -> T {
        let mut inner = self.lock();
        inner.setting = setting;
        apply(native_theme(setting, inner.system()))
    }
}

#[cfg(target_os = "linux")]
impl Appearance {
    /// At start, before the window exists: GTK's own preference for a dark theme (read before
    /// anything has set it), and how later changes reach the window. The setting followed is
    /// applied at once, from the portal's answer if it came already, else from GTK's preference.
    pub fn start(&self, gtk_prefers_dark: Option<bool>, apply: Apply) {
        let mut inner = self.lock();
        inner.gtk_prefers_dark = gtk_prefers_dark;
        if let Some(theme) = native_theme(inner.setting, inner.system()) {
            apply(theme);
        }
        inner.apply = Some(apply);
    }

    /// The portal answered, or signalled a change: its `color-scheme`, or `None` when it cannot
    /// be read. Kept; under System also applied ([`on_desktop_change`]) once
    /// [`Appearance::start`] has run.
    pub fn desktop_said(&self, portal: Option<u32>) {
        let mut inner = self.lock();
        inner.portal = portal;
        let theme = on_desktop_change(inner.setting, inner.portal, inner.gtk_prefers_dark);
        if let (Some(theme), Some(apply)) = (theme, &inner.apply) {
            apply(theme);
        }
    }
}

/// Linux, on the main thread once the app is built and before its window: GTK's preference for
/// a dark theme as it stands (nothing has set it yet: tao's own portal reading is off, see the
/// module docs), the window's theme from the stored setting, and the portal's watch.
#[cfg(target_os = "linux")]
pub fn start<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    appearance: &std::sync::Arc<Appearance>,
) {
    use gtk::prelude::GtkSettingsExt;

    let gtk_prefers_dark =
        gtk::Settings::default().map(|settings| settings.is_gtk_application_prefer_dark_theme());
    let handle = app.clone();
    appearance.start(
        gtk_prefers_dark,
        Box::new(move |theme| handle.set_theme(Some(theme))),
    );
    tracing::info!(
        setting = ?appearance.setting(),
        gtk_prefers_dark = ?gtk_prefers_dark,
        "the window's theme follows the setting"
    );
    let watched = appearance.clone();
    let watch = portal::watch(portal::Bus::Session, portal::CALL_TIMEOUT, move |scheme| {
        watched.desktop_said(scheme)
    });
    if let Err(e) = watch {
        tracing::warn!(
            "no thread to follow the desktop's colour scheme ({e}): the System theme stays as \
             it was at start"
        );
    }
}

#[cfg(test)]
mod tests {
    use apprafter_desktop_ipc::Theme;
    use tauri::Theme as Native;

    use super::*;

    #[test]
    fn the_portal_says_dark_or_light_and_otherwise_gtk_s_preference_at_start_decides() {
        #[rustfmt::skip]
        let cases = [
            // The portal's color-scheme, GTK's preference for dark at start: the theme.
            (Some(1), None, Native::Dark),
            (Some(1), Some(false), Native::Dark),
            (Some(2), Some(true), Native::Light),
            (Some(2), None, Native::Light),
            // 0, no preference: GTK's, else light.
            (Some(0), Some(true), Native::Dark),
            (Some(0), Some(false), Native::Light),
            (Some(0), None, Native::Light),
            // A value the portal does not define reads as no preference.
            (Some(3), Some(true), Native::Dark),
            (Some(u32::MAX), None, Native::Light),
            // No portal, or no answer from it.
            (None, Some(true), Native::Dark),
            (None, Some(false), Native::Light),
            (None, None, Native::Light),
        ];
        for (portal, gtk, expected) in cases {
            assert_eq!(
                linux_system_theme(portal, gtk),
                expected,
                "portal {portal:?}, GTK prefers dark {gtk:?}"
            );
        }
    }

    #[test]
    fn an_explicit_theme_is_applied_as_it_is_and_system_as_the_platform_resolves_it() {
        for system in [None, Some(Native::Dark), Some(Native::Light)] {
            assert_eq!(native_theme(Theme::Light, system), Some(Native::Light));
            assert_eq!(native_theme(Theme::Dark, system), Some(Native::Dark));
            assert_eq!(native_theme(Theme::System, system), system);
        }
    }

    #[test]
    fn a_change_of_the_desktop_re_applies_only_under_system() {
        assert_eq!(
            on_desktop_change(Theme::System, Some(1), Some(false)),
            Some(Native::Dark)
        );
        assert_eq!(
            on_desktop_change(Theme::System, Some(2), Some(true)),
            Some(Native::Light)
        );
        assert_eq!(
            on_desktop_change(Theme::System, Some(0), Some(true)),
            Some(Native::Dark)
        );
        assert_eq!(
            on_desktop_change(Theme::System, None, None),
            Some(Native::Light)
        );
        for setting in [Theme::Light, Theme::Dark] {
            for portal in [Some(0), Some(1), Some(2), None] {
                for gtk in [Some(true), Some(false), None] {
                    assert_eq!(
                        on_desktop_change(setting, portal, gtk),
                        None,
                        "{setting:?} {portal:?} {gtk:?}"
                    );
                }
            }
        }
    }

    /// The dependencies `Cargo.lock` gives every package named `name`; it fails when there is
    /// none, so a renamed package cannot pass for one without the dependency.
    fn lock_dependencies<'a>(lock: &'a str, name: &str) -> Vec<&'a str> {
        let entry = format!("name = \"{name}\"");
        let packages: Vec<&str> = lock
            .split("[[package]]")
            .filter(|package| package.lines().any(|line| line == entry))
            .collect();
        assert!(!packages.is_empty(), "no {name} in Cargo.lock");
        packages
            .iter()
            .flat_map(|package| {
                package
                    .lines()
                    .skip_while(|line| *line != "dependencies = [")
                    .skip(1)
                    .take_while(|line| *line != "]")
                    .map(|line| line.trim().trim_end_matches(',').trim_matches('"'))
            })
            .collect()
    }

    /// Tauri's `dbus` feature stays off (desktop/Cargo.toml). It turns on tao's own reading of
    /// the portal (GOTCHA-107): on the main thread, blocking it for up to 5 s, at window
    /// creation, and on every change of the desktop, forced over the theme this module decided
    /// — Light or Dark turned with the desktop, and "no preference" light whatever GTK says.
    /// Cargo unifies features, so one crate in the graph that takes `tauri`,
    /// `tauri-runtime-wry` or `tao` with their default features turns it back on, and nothing
    /// else shows it. tao's feature adds one dependency, `dbus`, which the lock lists among
    /// tao's only while some target or feature of the workspace has the feature on. (It does
    /// not carry tao's taskbar badge, which loads libunity at run time: the D.5 tray badge on
    /// Linux needs its own `com.canonical.Unity.LauncherEntry`, WI-433.)
    #[test]
    fn tao_has_no_portal_reading_of_its_own() {
        let tao = lock_dependencies(include_str!("../../Cargo.lock"), "tao");
        assert!(tao.contains(&"gtk"), "tao's dependencies, as read: {tao:?}");
        assert!(
            !tao.iter()
                .any(|dependency| *dependency == "dbus" || dependency.starts_with("dbus ")),
            "tao's `dbus` feature is on: a dependency takes tauri, tauri-runtime-wry or tao with \
             default features ({tao:?})"
        );
    }

    /// `theme_apply`: the explicit theme, or System as this OS resolves it — on Linux from the
    /// portal and GTK, of which nothing is known yet here (light, honestly); elsewhere the OS's
    /// own (`None`).
    #[test]
    fn theme_apply_gives_the_window_the_explicit_theme_or_system_as_this_os_resolves_it() {
        let appearance = Appearance::new(Theme::Dark);
        assert_eq!(appearance.setting(), Theme::Dark);
        let given = |setting| appearance.apply(setting, |native| native);
        assert_eq!(given(Theme::Light), Some(Native::Light));
        assert_eq!(appearance.setting(), Theme::Light);
        assert_eq!(given(Theme::Dark), Some(Native::Dark));
        let system = cfg!(target_os = "linux").then_some(Native::Light);
        assert_eq!(given(Theme::System), system);
        assert_eq!(appearance.setting(), Theme::System);
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use std::sync::{Arc, Mutex};

        use super::*;

        /// What the window was given, in order, by the start, the portal's watch and
        /// `theme_apply` alike.
        type Given = Arc<Mutex<Vec<Native>>>;

        fn recorder() -> (Given, Apply) {
            let given = Given::default();
            let apply: Apply = {
                let given = given.clone();
                Box::new(move |theme| given.lock().unwrap().push(theme))
            };
            (given, apply)
        }

        fn seen(given: &Given) -> Vec<Native> {
            given.lock().unwrap().clone()
        }

        /// `theme_apply`, recorded where the start and the watch record theirs.
        fn theme_apply(appearance: &Appearance, given: &Given, setting: Theme) {
            appearance.apply(setting, |native| {
                given
                    .lock()
                    .unwrap()
                    .push(native.expect("Linux always resolves"))
            });
        }

        #[test]
        fn at_start_the_stored_theme_is_applied_from_gtk_s_preference_until_the_portal_answers() {
            let appearance = Appearance::new(Theme::System);
            let (given, apply) = recorder();
            appearance.start(Some(true), apply);
            assert_eq!(seen(&given), [Native::Dark]);
            appearance.desktop_said(Some(2));
            assert_eq!(seen(&given), [Native::Dark, Native::Light]);
        }

        #[test]
        fn a_portal_answer_before_the_start_is_kept_and_wins_over_gtk() {
            let appearance = Appearance::new(Theme::System);
            appearance.desktop_said(Some(1));
            let (given, apply) = recorder();
            appearance.start(Some(false), apply);
            assert_eq!(seen(&given), [Native::Dark]);
        }

        #[test]
        fn under_system_every_change_of_the_desktop_reaches_the_window() {
            let appearance = Appearance::new(Theme::System);
            let (given, apply) = recorder();
            appearance.start(Some(true), apply);
            for (portal, theme) in [
                (Some(1), Native::Dark),
                (Some(2), Native::Light),
                // No preference, then the portal gone: GTK's preference at start, dark.
                (Some(0), Native::Dark),
                (None, Native::Dark),
            ] {
                appearance.desktop_said(portal);
                assert_eq!(seen(&given).last(), Some(&theme), "{portal:?}");
            }
            assert_eq!(seen(&given).len(), 5);
        }

        #[test]
        fn under_an_explicit_theme_a_change_of_the_desktop_changes_nothing() {
            for setting in [Theme::Light, Theme::Dark] {
                let appearance = Appearance::new(setting);
                let (given, apply) = recorder();
                appearance.start(Some(true), apply);
                let forced = if setting == Theme::Light {
                    Native::Light
                } else {
                    Native::Dark
                };
                assert_eq!(seen(&given), [forced]);
                for portal in [Some(1), Some(2), Some(0), None] {
                    appearance.desktop_said(portal);
                }
                assert_eq!(seen(&given), [forced], "{setting:?}");
            }
        }

        /// The page switches to System and back: System takes the portal's latest answer, and
        /// the desktop is followed only while it stays System.
        #[test]
        fn theme_apply_switches_what_the_desktop_s_changes_do() {
            let appearance = Appearance::new(Theme::Light);
            let (given, apply) = recorder();
            appearance.start(None, apply);
            appearance.desktop_said(Some(1));
            assert_eq!(seen(&given), [Native::Light]);
            theme_apply(&appearance, &given, Theme::System);
            assert_eq!(seen(&given), [Native::Light, Native::Dark]);
            appearance.desktop_said(Some(2));
            assert_eq!(seen(&given).last(), Some(&Native::Light));
            theme_apply(&appearance, &given, Theme::Dark);
            appearance.desktop_said(Some(2));
            appearance.desktop_said(Some(1));
            assert_eq!(
                seen(&given),
                [Native::Light, Native::Dark, Native::Light, Native::Dark]
            );
        }

        /// Before the start nothing reaches the window: there is no window, nor a way to it.
        #[test]
        fn before_the_start_a_change_of_the_desktop_is_only_kept() {
            let appearance = Appearance::new(Theme::System);
            appearance.desktop_said(Some(2));
            appearance.desktop_said(Some(1));
            let (given, apply) = recorder();
            appearance.start(Some(false), apply);
            assert_eq!(seen(&given), [Native::Dark]);
        }
    }
}
