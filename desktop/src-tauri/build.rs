// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Tauri's build step, with the app's command list as its ACL manifest, plus an 8 MiB
//! main-thread stack on Windows.
//!
//! Every command the shell registers gets an `allow-<command>` permission from
//! [`COMMANDS`] (capabilities/main.json5 grants them); a command missing from the list has no
//! permission and the ACL refuses it, so the list and the registration cannot drift apart
//! unnoticed (tests/ipc_mock.rs invokes every name).
//!
//! Windows reserves 1 MiB for a program's main thread (Linux and macOS 8 MiB); a debug
//! `apprafter.exe` overflowed it inside clap before reading an argument (cli/platform-cli/build.rs).
//! The desktop links the same core, so its binary gets what the other platforms give.

// At module level: in a function body `include!` takes an expression, not the `pub const`
// items the file holds. `ALLOWED_WHILE_LOCKED` comes along unused.
#[allow(dead_code)]
mod ipc_commands {
    include!("../ipc/src/commands.rs");
}

use ipc_commands::COMMANDS;

const MAIN_THREAD_STACK_BYTES: u64 = 8 * 1024 * 1024;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
            Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:{MAIN_THREAD_STACK_BYTES}"),
            Ok("gnu") => {
                println!("cargo:rustc-link-arg-bins=-Wl,--stack,{MAIN_THREAD_STACK_BYTES}")
            }
            _ => {}
        }
    }
    // tauri-build narrows the rerun triggers to its own inputs; the command list is one too.
    println!("cargo:rerun-if-changed=../ipc/src/commands.rs");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri-build");
}
