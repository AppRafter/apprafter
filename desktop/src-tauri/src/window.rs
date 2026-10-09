// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The main window: size, background, per-OS chrome, the navigation guard, its webview storage,
//! and bringing it to the front.
//!
//! # Chrome
//!
//! The page draws a 38px title bar (logo, tabs). On Windows the window has no decorations (the
//! page draws the caption buttons) and keeps its shadow, which on Windows 11 also rounds the
//! corners. On macOS the title bar is an overlay with no title, the traffic lights placed in the
//! page's bar. On Linux the window keeps its native decorations: client-side ones vary across
//! X11 and Wayland.
//!
//! # Showing the window
//!
//! The window is created hidden, so it never flashes white, and shows on `window_ready`: the
//! page sends it once its first screen and its theme are in. Two backstops show it anyway, each
//! only if it is still hidden (a visibility check that fails counts as hidden): one
//! [`REVEAL_AFTER_LOAD`] after the page finished loading — the page is there but has not said
//! so — and one [`REVEAL_FALLBACK`] after the window was built, for a page that never loads.
//! The first is not at the load itself: the first screen waits on IPC answers that usually come
//! after it, and showing the window there would show the default dark page before a light one.
//!
//! The webview may only ever show the app itself. A link, a redirect or an injected
//! `location =` that leaves the app origin is refused, and new windows are never opened
//! (external links go through the opener in a later step). The app origin differs per OS:
//! `tauri://localhost` on macOS/Linux, `http(s)://tauri.localhost` on Windows, and the Vite
//! dev server in debug builds.
//!
//! # Webview storage and the data-directory override
//!
//! On Linux and Windows the webview keeps its storage (local storage, caches) in the app's
//! local data directory, which the override moves with everything else. WKWebView on macOS
//! keeps it in a store of its own, which the app's directories do not reach: with an override,
//! the window gets a store of its own
//! ([`env::data_store_identifier`](crate::env::data_store_identifier)), one per override
//! directory. That needs macOS 14; on macOS 13, the oldest the app runs on, WKWebView has no
//! such store and keeps the default one, so a walk there shares its webview storage with the
//! owner's app — the app's settings, logs and single-instance lock still move.

use std::path::Path;
use std::thread;
use std::time::Duration;

use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, Runtime, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

#[cfg(target_os = "macos")]
use crate::env;

/// The window label every capability and command refers to.
pub const MAIN: &str = "main";

/// How long the hidden window waits for the page's `window_ready` before it shows anyway: a
/// page that failed to start must not leave the app running with no window to quit from.
pub const REVEAL_FALLBACK: Duration = Duration::from_secs(5);

/// How long after the page finished loading the window waits for `window_ready` before it
/// shows anyway: long enough for the first screen's IPC answers, short of the fallback.
pub const REVEAL_AFTER_LOAD: Duration = Duration::from_secs(1);

/// Show the window and give it the focus (`window_ready`, and the backstops).
pub fn reveal<R: Runtime>(window: &WebviewWindow<R>) -> tauri::Result<()> {
    window.show()?;
    window.set_focus()
}

/// Is a window whose visibility check answered `visible` still to be shown? A check that
/// failed counts as hidden: a window shown twice is harmless, one never shown is not.
fn still_hidden(visible: tauri::Result<bool>) -> bool {
    !visible.unwrap_or(false)
}

/// The backstop a page-load event arms: a finished load, [`REVEAL_AFTER_LOAD`] from now.
fn backstop_after(event: PageLoadEvent) -> Option<Duration> {
    match event {
        PageLoadEvent::Finished => Some(REVEAL_AFTER_LOAD),
        PageLoadEvent::Started => None,
    }
}

/// After `after`, show the main window if it is still hidden, saying why (`what` completes
/// "the page did not report ready ..."). Without a thread to wait on, it shows the window now.
fn reveal_later<R: Runtime>(app: &AppHandle<R>, after: Duration, what: &'static str) {
    let handle = app.clone();
    let spawned = thread::Builder::new()
        .name("reveal-backstop".into())
        .spawn(move || {
            thread::sleep(after);
            if let Some(window) = handle.get_webview_window(MAIN) {
                show_if_hidden(&window, what);
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("no thread for a reveal backstop ({e}); showing the window at once");
        if let Some(window) = app.get_webview_window(MAIN) {
            show_if_hidden(&window, "before a backstop could wait");
        }
    }
}

fn show_if_hidden<R: Runtime>(window: &WebviewWindow<R>, what: &str) {
    if still_hidden(window.is_visible()) {
        tracing::warn!("the page did not report ready {what}");
        if let Err(e) = reveal(window) {
            tracing::error!("the window could not be shown: {e}");
        }
    }
}

/// The dev server `tauri dev` loads (tauri.conf.json5 `build.devUrl`).
const DEV_ORIGIN: &str = "http://localhost:1420";

/// Is `url` the app's own content?
pub fn is_app_url(url: &Url, debug: bool) -> bool {
    match url.scheme() {
        "tauri" => url.host_str() == Some("localhost"),
        "http" | "https" if url.host_str() == Some("tauri.localhost") => true,
        "http" if debug => url.origin().ascii_serialization() == DEV_ORIGIN,
        _ => false,
    }
}

/// What a second launch does (the single-instance plugin): bring the main window to the front,
/// and nothing else. The second process's arguments and working directory are never read, so
/// nothing outside the app can make it act (ADR 0036). Without a main window (a quit is
/// waiting for its operations) it does nothing.
pub fn show_and_focus(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Build the main window. `data_dir` is the data-directory override, prepared
/// ([`env::prepare_data_dir`](crate::env::prepare_data_dir)); on macOS it picks the webview's
/// store (see the module docs), elsewhere the app's directories already carry it.
pub fn build_main(app: &tauri::AppHandle, data_dir: Option<&Path>) -> tauri::Result<()> {
    let debug = cfg!(debug_assertions);
    let window = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::App("index.html".into()))
        .title("AppRafter")
        .inner_size(1280.0, 800.0)
        .min_inner_size(1024.0, 640.0)
        .background_color(tauri::window::Color(0x0a, 0x0e, 0x1a, 0xff))
        .visible(false)
        .disable_drag_drop_handler()
        .zoom_hotkeys_enabled(false)
        .on_navigation(move |url| is_app_url(url, debug))
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
        .on_page_load(|window, payload| {
            if let Some(after) = backstop_after(payload.event()) {
                reveal_later(window.app_handle(), after, "within a second of loading");
            }
        });
    #[cfg(target_os = "windows")]
    let window = window.decorations(false).shadow(true);
    // The lights centred in the page's 38px bar (to be tuned on a Mac, D.2f manual list).
    #[cfg(target_os = "macos")]
    let window = window
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .traffic_light_position(tauri::LogicalPosition::new(14.0, 19.0));
    // macOS 14 and later; on macOS 13 WKWebView ignores it and keeps the default store.
    #[cfg(target_os = "macos")]
    let window = match data_dir {
        Some(dir) => window.data_store_identifier(env::data_store_identifier(dir)),
        None => window,
    };
    #[cfg(not(target_os = "macos"))]
    let _ = data_dir;
    window.build()?;
    reveal_later(app, REVEAL_FALLBACK, "within 5s of the window opening");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn a_failed_visibility_check_counts_as_hidden_so_the_window_shows() {
        assert!(still_hidden(Err(tauri::Error::WindowNotFound)));
        assert!(still_hidden(Ok(false)));
        assert!(!still_hidden(Ok(true)));
    }

    #[test]
    fn a_finished_load_arms_the_backstop_before_the_fallback() {
        assert_eq!(
            backstop_after(PageLoadEvent::Finished),
            Some(REVEAL_AFTER_LOAD)
        );
        assert_eq!(backstop_after(PageLoadEvent::Started), None);
        assert!(REVEAL_AFTER_LOAD < REVEAL_FALLBACK);
    }

    #[test]
    fn the_app_origin_is_allowed_on_every_os() {
        assert!(is_app_url(&u("tauri://localhost/index.html"), false));
        assert!(is_app_url(&u("http://tauri.localhost/"), false));
        assert!(is_app_url(&u("https://tauri.localhost/x"), false));
    }

    #[test]
    fn the_dev_server_is_allowed_only_in_debug_builds() {
        assert!(is_app_url(&u("http://localhost:1420/"), true));
        assert!(!is_app_url(&u("http://localhost:1420/"), false));
        assert!(!is_app_url(&u("http://localhost:1421/"), true));
    }

    #[test]
    fn everything_else_is_refused() {
        for s in [
            "https://apprafter.dev/",
            "http://localhost/",
            "file:///etc/passwd",
            "tauri://evil.example/",
            "https://tauri.localhost.evil.example/",
            "javascript:alert(1)",
        ] {
            assert!(!is_app_url(&u(s), true), "{s} must be refused");
        }
    }
}
