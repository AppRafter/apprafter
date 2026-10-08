// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The main window: size, background, the navigation guard, its webview storage, and bringing
//! it to the front.
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

use tauri::{Manager, Url, WebviewUrl, WebviewWindowBuilder};

#[cfg(target_os = "macos")]
use crate::env;

/// The window label every capability and command refers to.
pub const MAIN: &str = "main";

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
        .disable_drag_drop_handler()
        .zoom_hotkeys_enabled(false)
        .on_navigation(move |url| is_app_url(url, debug))
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny);
    // macOS 14 and later; on macOS 13 WKWebView ignores it and keeps the default store.
    #[cfg(target_os = "macos")]
    let window = match data_dir {
        Some(dir) => window.data_store_identifier(env::data_store_identifier(dir)),
        None => window,
    };
    #[cfg(not(target_os = "macos"))]
    let _ = data_dir;
    window.build()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
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
