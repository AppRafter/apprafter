// SPDX-License-Identifier: FSL-1.1-MIT
//! Deprecated `platform-cli` shim.
//!
//! The canonical binary is now `apprafter`. This shim exists for one
//! release cycle (M1.5, v0.1.69 → v0.1.83) so anybody invoking the old
//! name gets a clear warning and a working forward instead of a hard
//! failure. Removed at the `v0.2.0-self-managing` tag.

use std::process::{exit, Command};

fn main() {
    eprintln!("Warning: `platform-cli` has been renamed to `apprafter`.");
    eprintln!("Run `apprafter <command>` instead.");
    eprintln!("This shim will be removed in v0.2.0.");
    eprintln!();

    let me = match std::env::current_exe() {
        Ok(p) => p,
        Err(err) => {
            eprintln!("platform-cli shim: cannot locate current_exe: {err}");
            exit(127);
        }
    };
    let parent = match me.parent() {
        Some(p) => p,
        None => {
            eprintln!("platform-cli shim: current_exe has no parent directory");
            exit(127);
        }
    };
    let apprafter_path = parent.join(if cfg!(windows) {
        "apprafter.exe"
    } else {
        "apprafter"
    });

    let status = Command::new(&apprafter_path)
        .args(std::env::args_os().skip(1))
        .status();

    match status {
        Ok(s) => exit(s.code().unwrap_or(1)),
        Err(err) => {
            eprintln!(
                "platform-cli shim: failed to exec {} — {err}",
                apprafter_path.display()
            );
            exit(127);
        }
    }
}
