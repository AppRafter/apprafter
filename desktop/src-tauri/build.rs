// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Tauri's build step, with the app's command list as its ACL manifest, plus an 8 MiB
//! main-thread stack and, on MSVC, an application manifest in every executable on Windows.
//!
//! Every command the shell registers gets an `allow-<command>` permission from
//! [`COMMANDS`] (capabilities/main.json5 grants them); a command missing from the list has no
//! permission and the ACL refuses it, so the list and the registration cannot drift apart
//! unnoticed (tests/ipc_mock.rs invokes every name).
//!
//! Windows reserves 1 MiB for a program's main thread (Linux and macOS 8 MiB); a debug
//! `apprafter.exe` overflowed it inside clap before reading an argument (cli/platform-cli/build.rs).
//! The desktop links the same core, so its binary gets what the other platforms give.
//!
//! tauri-build embeds its application manifest (Common Controls v6) in the app binary only, as
//! a resource linked into bins. A test executable linking Tauri then gets comctl32 v5, which
//! has no `TaskDialogIndirect`, and dies before its first test with
//! `STATUS_ENTRYPOINT_NOT_FOUND` (tests/ipc_mock.rs did). So on MSVC the linker embeds the
//! manifest in every executable of the package instead — the app, tests and examples alike —
//! from windows-app-manifest.xml, and tauri-build embeds none: two would be a duplicate
//! resource.

// At module level: in a function body `include!` takes an expression, not the `pub const`
// items the file holds. `ALLOWED_WHILE_LOCKED` comes along unused.
#[allow(dead_code)]
mod ipc_commands {
    include!("../ipc/src/commands.rs");
}

use ipc_commands::COMMANDS;

const MAIN_THREAD_STACK_BYTES: u64 = 8 * 1024 * 1024;

/// The application manifest every Windows MSVC executable of the package embeds.
const WINDOWS_APP_MANIFEST: &str = "windows-app-manifest.xml";

fn main() {
    let mut attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
            Ok("msvc") => {
                println!("cargo:rustc-link-arg-bins=/STACK:{MAIN_THREAD_STACK_BYTES}");
                let manifest = std::path::Path::new(
                    &std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"),
                )
                .join(WINDOWS_APP_MANIFEST);
                println!("cargo:rerun-if-changed={WINDOWS_APP_MANIFEST}");
                // `rustc-link-arg`, not `-bins`: tests and examples link with it too.
                println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
                println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
                attributes = attributes
                    .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
            }
            Ok("gnu") => {
                println!("cargo:rustc-link-arg-bins=-Wl,--stack,{MAIN_THREAD_STACK_BYTES}")
            }
            _ => {}
        }
    }
    // tauri-build narrows the rerun triggers to its own inputs; the command list is one too.
    println!("cargo:rerun-if-changed=../ipc/src/commands.rs");
    tauri_build::try_build(attributes).expect("tauri-build");
}
