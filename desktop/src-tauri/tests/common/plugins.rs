// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Stand-ins for the plugins whose commands reach the owner's desktop session (GOTCHA-156).
//!
//! - The clipboard: its setup opens arboard, which connects to the X11 or Wayland session the
//!   process inherited (on macOS and Windows, the system clipboard needs no session at all), and
//!   its commands read and write what the owner copied.
//! - The opener: an allowed URL starts `xdg-open`, `gio open` (which needs no display, only the
//!   session bus), `/usr/bin/open` or the Windows shell — under wine, `winebrowser` — and the
//!   owner's browser opens.
//!
//! Unsetting `DISPLAY` and `WAYLAND_DISPLAY` stops the first on Linux only, and the second not
//! at all. So the rig never registers either: it registers these under the real plugins' names.
//! A page's `plugin:clipboard-manager|…` and `plugin:opener|…` still pass the very ACL the app
//! has — the capability and the real plugins' permission sets are compiled into the context
//! (`generate_context!`), not taken from the plugin a test registers — and then reach a stand-in
//! that writes down what it was asked and touches nothing outside this process. The rig checks,
//! for every test, that neither real plugin set up (`common::rig_on`); tests/plugin_guard.rs
//! checks that nothing else in `src/` or `tests/` builds one; the real opener's own scope check
//! runs in tests/opener_scope.rs, in a child process that has nothing it could start.

use std::sync::{Arc, Mutex};

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime};

/// The real plugins' names: the stand-ins answer under them.
pub const CLIPBOARD: &str = "clipboard-manager";
pub const OPENER: &str = "opener";

/// What the stand-ins were asked, in order: `<plugin>|<command>` and its text, URL or path.
#[derive(Debug, Default, Clone)]
pub struct Asked(Arc<Mutex<Vec<(String, String)>>>);

impl Asked {
    fn push(&self, plugin: &str, command: &str, what: impl Into<String>) {
        self.0
            .lock()
            .unwrap()
            .push((format!("{plugin}|{command}"), what.into()));
    }

    /// Everything asked so far.
    // Read by tests/ipc_mock.rs only; tests/app_menu.rs compiles this module too.
    #[allow(dead_code)]
    pub fn all(&self) -> Vec<(String, String)> {
        self.0.lock().unwrap().clone()
    }
}

/// The clipboard stand-in: the real plugin's six commands, with the real plugin's arguments
/// (so an empty call fails on its missing text, as there). A write is written down; a read
/// answers an error, never a text.
pub fn clipboard<R: Runtime>(asked: Asked) -> TauriPlugin<R> {
    Builder::new(CLIPBOARD)
        .invoke_handler(tauri::generate_handler![
            clipboard::write_text,
            clipboard::read_text,
            clipboard::read_image,
            clipboard::write_image,
            clipboard::write_html,
            clipboard::clear
        ])
        .setup(move |app, _api| {
            app.manage(clipboard::Log(asked));
            Ok(())
        })
        .build()
}

/// The opener stand-in: the real plugin's three commands, with its arguments. Whatever it is
/// asked to open is written down; nothing is started.
pub fn opener<R: Runtime>(asked: Asked) -> TauriPlugin<R> {
    Builder::new(OPENER)
        .invoke_handler(tauri::generate_handler![
            opener::open_url,
            opener::open_path,
            opener::reveal_item_in_dir
        ])
        .setup(move |app, _api| {
            app.manage(opener::Log(asked));
            Ok(())
        })
        .build()
}

mod clipboard {
    use tauri::State;

    use super::{Asked, CLIPBOARD};

    pub struct Log(pub Asked);

    const NOTHING: &str = "the test clipboard holds nothing";

    #[tauri::command]
    pub fn write_text(log: State<'_, Log>, text: String, label: Option<String>) {
        let _ = label;
        log.0.push(CLIPBOARD, "write_text", text);
    }

    #[tauri::command]
    pub fn read_text(log: State<'_, Log>) -> Result<String, String> {
        log.0.push(CLIPBOARD, "read_text", "");
        Err(NOTHING.into())
    }

    #[tauri::command]
    pub fn read_image(log: State<'_, Log>) -> Result<u32, String> {
        log.0.push(CLIPBOARD, "read_image", "");
        Err(NOTHING.into())
    }

    #[tauri::command]
    pub fn write_image(log: State<'_, Log>, image: serde_json::Value) {
        log.0.push(CLIPBOARD, "write_image", image.to_string());
    }

    #[tauri::command]
    pub fn write_html(log: State<'_, Log>, html: String, alt_text: Option<String>) {
        let _ = alt_text;
        log.0.push(CLIPBOARD, "write_html", html);
    }

    #[tauri::command]
    pub fn clear(log: State<'_, Log>) {
        log.0.push(CLIPBOARD, "clear", "");
    }
}

mod opener {
    use tauri::State;

    use super::{Asked, OPENER};

    pub struct Log(pub Asked);

    #[tauri::command]
    pub fn open_url(log: State<'_, Log>, url: String, with: Option<String>) {
        let _ = with;
        log.0.push(OPENER, "open_url", url);
    }

    #[tauri::command]
    pub fn open_path(log: State<'_, Log>, path: String, with: Option<String>) {
        let _ = with;
        log.0.push(OPENER, "open_path", path);
    }

    #[tauri::command]
    pub fn reveal_item_in_dir(log: State<'_, Log>, paths: Vec<String>) {
        log.0.push(OPENER, "reveal_item_in_dir", paths.join("\n"));
    }
}
