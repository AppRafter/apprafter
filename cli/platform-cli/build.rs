// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Give the binaries of this package an 8 MiB main-thread stack on Windows.
//!
//! Windows reserves 1 MiB for a program's main thread; Linux and macOS give it
//! 8 MiB. clap builds the whole command tree on that stack before it reads a
//! single argument, and an unoptimised build of this tree needs more than
//! 1 MiB: on the first Windows test run every debug `apprafter.exe`, even
//! `--version`, died with "thread 'main' has overflowed its stack"
//! (0xC00000FD) while the same code passed on Linux and macOS. The reservation
//! lives in the executable's header, which only the linker writes, so it is set
//! here — to what the other two platforms already give — rather than betting on
//! how much an optimised build happens to need.

const MAIN_THREAD_STACK_BYTES: u64 = 8 * 1024 * 1024;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:{MAIN_THREAD_STACK_BYTES}"),
        Ok("gnu") => println!("cargo:rustc-link-arg-bins=-Wl,--stack,{MAIN_THREAD_STACK_BYTES}"),
        _ => {}
    }
}
