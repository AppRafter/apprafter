// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! The macOS app menu, built here on any OS (the app sets it on macOS only): one Quit, the
//! app's own item rather than the system's `terminate:`, and choosing it runs the quit
//! sequence — afterwards nothing starts.
//!
//! A `harness = false` target (Cargo.toml): on macOS muda builds a menu item on the main
//! thread only, as the app does, and libtest runs every test on a thread of its own. So
//! `main` runs the checks itself, on the process's main thread, on every OS; a failed check
//! panics there and the process exits non-zero.

mod common;

use std::thread;

use apprafter_core::{Outcome, PlanClass};
use apprafter_desktop::menu;
use apprafter_desktop::ops::PlanParts;
use apprafter_desktop_ipc::errors;
use common::{code, invoke, lock_off, rig};
use serde_json::json;
use tauri::menu::{MenuEvent, MenuId, MenuItemKind};

fn main() {
    // Rust names the thread `main` runs on `main`; a libtest thread bears its test's name.
    assert_eq!(
        thread::current().name(),
        Some("main"),
        "the checks must run on the main thread"
    );
    println!("ok: on the main thread");

    let rig = rig(lock_off());
    let handle = rig._app.handle();
    let bar = menu::app_menu(handle).unwrap();
    let mut items = Vec::new();
    for top in bar.items().unwrap() {
        let submenu = top.as_submenu().expect("a menu bar of submenus");
        items.extend(submenu.items().unwrap());
    }
    let quits: Vec<String> = items
        .iter()
        .filter_map(|item| {
            let text = match item {
                MenuItemKind::MenuItem(item) => item.text(),
                MenuItemKind::Predefined(item) => item.text(),
                _ => return None,
            };
            Some(text.unwrap()).filter(|text| text.contains("Quit"))
        })
        .collect();
    assert_eq!(quits, ["Quit AppRafter"], "one Quit, and no predefined one");
    println!("ok: the app menu has one Quit, \"Quit AppRafter\", and no predefined one");
    let quit = items
        .iter()
        .find(|item| item.id() == menu::QUIT_ITEM)
        .expect("the Quit item");
    assert!(quit.as_menuitem().is_some(), "the app's own item");
    println!("ok: the Quit is the app's own item, not the system's terminate:");

    // Choosing it begins the quit before `on_menu_event` returns: from there nothing starts,
    // so nothing below waits. The mock runtime cannot exit (its `request_exit` is
    // `unimplemented!()`), so the quit thread's final exit panics there, on its own thread,
    // after everything checked here; a panic on another thread leaves the exit code alone.
    menu::on_menu_event(
        handle,
        &MenuEvent {
            id: MenuId::new(menu::QUIT_ITEM),
        },
    );
    let plan = rig.shell.ops.register_plan(
        PlanParts::new(PlanClass::Bounded, "Upgrade", "upgrade"),
        Box::new(|_, _| Ok(Outcome::Completed { result: json!(0) })),
    );
    let reply = invoke(
        &rig,
        "op_execute",
        json!({ "opId": plan.op_id, "onEvent": "__CHANNEL__:7" }),
    );
    assert_eq!(code(&reply), Some(errors::CLOSING), "{reply:?}");
    println!("ok: choosing Quit starts the quit: op_execute then answers closing");
}

/// Under libtest (`harness = false` gone from Cargo.toml) `main` never runs, and a target with
/// no test passes: this one fails it instead. A `#[test]` item is compiled only under libtest
/// (rustc `--test`); `cfg(test)` cannot tell, Cargo sets it for every test target.
#[test]
fn the_checks_run_in_main_only_with_harness_false() {
    panic!("tests/app_menu.rs needs `harness = false` in Cargo.toml: its checks run in `main`");
}
