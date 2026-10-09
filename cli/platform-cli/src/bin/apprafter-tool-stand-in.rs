// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! A stand-in for the external tools `apprafter doctor` probes, for the integration tests on
//! Windows (`tests/common/stand_in.rs`, GOTCHA-66). Windows runs only real executables and
//! the core's resolver takes only `<tool>.exe` (a `.cmd` is reported as not runnable), so the
//! `/bin/sh` stand-ins the Unix tests use have no Windows form. Linked as `<tool>.exe`, this
//! binary answers as those scripts do, the tool being its own file name:
//!
//! - the tool's version call (`cli_core::tools::Tool::version_args`): `<tool> stand-in` and
//!   exit 0, on stderr for `ssh -V` (ssh writes its version there);
//! - `__probe`: exit 0;
//! - anything else: a usage error and exit 2.
//!
//! Built only with the test feature `tool-stand-in`: never in a release build, never installed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    // The name it was started under; `current_exe` follows a symlink to this binary's own name.
    let exe = std::env::args_os()
        .next()
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok());
    let name = exe
        .as_deref()
        .and_then(Path::file_stem)
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["__probe"] {
        return ExitCode::SUCCESS;
    }
    match cli_core::tools::ALL.iter().find(|t| t.name == name) {
        Some(tool) if args == tool.version_args => {
            if tool.name == "ssh" {
                eprintln!("{name} stand-in");
            } else {
                println!("{name} stand-in");
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{name} stand-in: unexpected arguments: {}", args.join(" "));
            ExitCode::from(2)
        }
    }
}
