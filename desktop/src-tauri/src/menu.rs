// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The macOS app menu: Tauri's default menu, its Quit item replaced by one of the app's own.
//!
//! Tauri's default Quit is the system's `terminate:`, which ends the app with no exit request
//! the shell could refuse: the operations would get only the short wait of an exit the OS
//! forced ([`Shell::on_exit`](crate::app::Shell::on_exit)). The app's own Quit, on the same
//! Cmd+Q, runs the full quit sequence ([`app::quit`]) instead. The rest of the default stays:
//! the Edit menu above all, without which Cmd+C, Cmd+V and Cmd+A do nothing in a text field.
//!
//! The Dock's Quit, a logout and a shutdown still send `terminate:`; `Shell::on_exit` covers
//! them. Linux and Windows show no menu bar, so the app sets none there; this compiles on every
//! OS all the same, so its tests run everywhere.

use std::sync::Arc;

use tauri::menu::{
    AboutMetadata, Menu, MenuBuilder, MenuEvent, MenuItemBuilder, SubmenuBuilder, HELP_SUBMENU_ID,
    WINDOW_SUBMENU_ID,
};
use tauri::{AppHandle, Manager, Runtime};

use crate::app::{self, Shell};

/// The id of the app menu's Quit item.
pub const QUIT_ITEM: &str = "quit";

/// The macOS app menu (see the module docs): the app's own submenu (About, Services, Hide,
/// Hide Others, Quit), File, Edit, View, Window and Help, as Tauri builds them by default.
pub fn app_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let name = app.package_info().name.clone();
    let about = AboutMetadata {
        name: Some(name.clone()),
        version: Some(app.package_info().version.to_string()),
        ..AboutMetadata::default()
    };
    let quit = MenuItemBuilder::with_id(QUIT_ITEM, format!("Quit {name}"))
        .accelerator("CmdOrCtrl+Q")
        .build(app)?;
    let app_submenu = SubmenuBuilder::new(app, &name)
        .about(Some(about))
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .separator()
        .item(&quit)
        .build()?;
    let file = SubmenuBuilder::new(app, "File").close_window().build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let view = SubmenuBuilder::new(app, "View").fullscreen().build()?;
    let window = SubmenuBuilder::with_id(app, WINDOW_SUBMENU_ID, "Window")
        .minimize()
        .maximize()
        .separator()
        .close_window()
        .build()?;
    let help = SubmenuBuilder::with_id(app, HELP_SUBMENU_ID, "Help").build()?;
    MenuBuilder::new(app)
        .items(&[&app_submenu, &file, &edit, &view, &window, &help])
        .build()
}

/// A menu item was chosen: the Quit item quits ([`app::quit`]); every other item is one of
/// the system's own, which needs nothing from the app.
pub fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: &MenuEvent) {
    if event.id() == QUIT_ITEM {
        match app.try_state::<Arc<Shell>>() {
            Some(shell) => app::quit(app, &shell),
            // Before the shell is installed nothing runs yet: nothing to wait for.
            None => app.exit(0),
        }
    }
}
