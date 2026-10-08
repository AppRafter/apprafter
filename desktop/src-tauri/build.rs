// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Tauri's build step, plus an 8 MiB main-thread stack on Windows.
//!
//! Windows reserves 1 MiB for a program's main thread (Linux and macOS 8 MiB); a debug
//! `apprafter.exe` overflowed it inside clap before reading an argument (cli/platform-cli/build.rs).
//! The desktop links the same core, so its binary gets what the other platforms give.

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
    tauri_build::build();
}
