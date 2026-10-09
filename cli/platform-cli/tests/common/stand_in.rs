// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Stand-ins for the external tools `apprafter doctor` probes (GOTCHA-66): a `PATH` that holds
//! nothing else, so no test depends on the tools the machine running it happens to carry (the
//! macOS runners have no `kubectl`) and none ever runs a real one.

use cli_core::tools::Tool;
use tempfile::TempDir;

/// A directory, for `PATH`, holding one stand-in per tool, named `<tool>{EXE_SUFFIX}`. Made
/// under `CARGO_TARGET_TMPDIR`: a hard link cannot cross volumes, and the Windows runners keep
/// `%TEMP%` and the checkout on different drives.
///
/// Unix: a `/bin/sh` script that answers the tool's version call (`Tool::version_args`) as the
/// real tool does — one line, `<tool> stand-in`, and exit 0; on stderr for `ssh -V`, which
/// writes its version there — and any other arguments with a usage error and exit 2. A real
/// tool that exits non-zero on its version call is reported as having no version, so a
/// stand-in that did would test that path instead of a working tool. Only shell builtins: the
/// probed child's `PATH` is this directory alone (GOTCHA-104).
///
/// Windows: a hard link of the `apprafter` binary under test, because Windows runs only real
/// executables. It answers `--version` (git) and `-V` (ssh) with its own version, and the
/// `version` subcommands of the others with a usage error and exit 2.
pub fn tool_stand_ins<'a>(tools: impl IntoIterator<Item = &'a Tool>) -> TempDir {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("stand-in tools dir");
    for tool in tools {
        let path = dir
            .path()
            .join(format!("{}{}", tool.name, std::env::consts::EXE_SUFFIX));
        write_stand_in(&path, tool);
    }
    dir
}

#[cfg(unix)]
fn write_stand_in(path: &std::path::Path, tool: &Tool) {
    use std::os::unix::fs::PermissionsExt;
    let name = tool.name;
    let call = tool.version_args.join(" ");
    let answer = if name == "ssh" {
        format!("echo '{name} stand-in' >&2")
    } else {
        format!("echo '{name} stand-in'")
    };
    std::fs::write(
        path,
        format!(
            "#!/bin/sh\n\
             case \"$*\" in\n\
             __probe) exit 0 ;;\n\
             '{call}') {answer} ;;\n\
             *) echo \"{name} stand-in: unexpected arguments: $*\" >&2; exit 2 ;;\n\
             esac\n"
        ),
    )
    .expect("write stand-in");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod stand-in");
    // A sibling test thread that forked while the file was open for writing holds it open in
    // its child until that child execs, and `execve` refuses with ETXTBSY until then: run the
    // stand-in until it starts, so the command under test never meets that window.
    for _ in 0..200 {
        match std::process::Command::new(path).arg("__probe").status() {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
            _ => break,
        }
    }
}

#[cfg(not(unix))]
fn write_stand_in(path: &std::path::Path, _tool: &Tool) {
    std::fs::hard_link(env!("CARGO_BIN_EXE_apprafter"), path).expect("stand-in tool");
}
