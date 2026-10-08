// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Source guards for the shared core (ADR 0067 §2).
//!
//! 1. `apprafter-core` never prints, exits the process, prompts, or reads the
//!    environment. A scan of its `src/` fails on any of those tokens outside
//!    comments.
//! 2. Env reads in the crates the core builds on, and in the CLI, may only
//!    go down: each slice that moves a family into the core moves its env
//!    reads behind `Context`. The count is exact, so a drop must lower the
//!    baseline in the same commit and can never creep back up.

use std::fs;
use std::path::{Path, PathBuf};

/// Non-comment occurrences of `env::var(`, `env::var_os(`, `env::vars(` and
/// `env::vars_os(` in the non-test part of the scanned crates' `src/`.
/// Re-measure with `cargo test -p apprafter-core --test guards -- --nocapture`.
const ENV_READ_BASELINE: usize = 35;

const FORBIDDEN_IN_CORE: &[&str] = &[
    "println!",
    "eprintln!",
    "print!(",
    "eprint!(",
    "dbg!(",
    "process::exit",
    "env::var(",
    "env::var_os(",
    "env::vars(",
    "env::vars_os(",
    "env::set_var(",
    "env::remove_var(",
    "inquire::",
    "signal_hook",
];

const ENV_READS: &[&str] = &["env::var(", "env::var_os(", "env::vars(", "env::vars_os("];

fn crate_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(name)
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap_or_else(|e| panic!("read {d:?}: {e}")) {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

#[test]
fn the_core_never_prints_exits_prompts_or_reads_env() {
    let mut hits = Vec::new();
    for file in rust_files(&crate_dir("apprafter-core").join("src")) {
        let text = fs::read_to_string(&file).unwrap();
        for (no, line) in text.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            for token in FORBIDDEN_IN_CORE {
                if line.contains(token) {
                    hits.push(format!(
                        "{}:{}: `{token}`: {}",
                        file.display(),
                        no + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "apprafter-core must stay pure (ADR 0067 §2):\n{}",
        hits.join("\n")
    );
}

/// Whether the `#[cfg(test)]` on `lines[at]` gates the file's test module
/// (`mod tests {` / `mod tests;`). One on a single item — a const, a static,
/// a `pub(crate)` seam — has production code after it, so the scan goes on.
fn gates_a_test_module(lines: &[&str], at: usize) -> bool {
    lines[at + 1..]
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("#[") && !l.starts_with("//"))
        .is_some_and(|l| l.starts_with("mod "))
}

/// Count env reads in one file, up to the `#[cfg(test)]` that opens its test
/// module.
fn env_reads_in(file: &Path) -> Vec<String> {
    let text = fs::read_to_string(file).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    for (no, line) in lines.iter().enumerate() {
        if line.trim() == "#[cfg(test)]" && gates_a_test_module(&lines, no) {
            break;
        }
        if is_comment(line) {
            continue;
        }
        for token in ENV_READS {
            let n = line.matches(token).count();
            for _ in 0..n {
                found.push(format!("{}:{}", file.display(), no + 1));
            }
        }
    }
    found
}

#[test]
fn env_reads_outside_the_core_only_go_down() {
    let mut sites = Vec::new();
    for krate in ["cli-core", "cli-state", "cli-providers", "platform-cli"] {
        for file in rust_files(&crate_dir(krate).join("src")) {
            sites.extend(env_reads_in(&file));
        }
    }
    let n = sites.len();
    println!("env reads outside the core: {n}");
    assert!(
        n <= ENV_READ_BASELINE,
        "env reads rose to {n} (baseline {ENV_READ_BASELINE}). Read the value through \
         apprafter_core::Context instead. Sites:\n{}",
        sites.join("\n")
    );
    assert!(
        n == ENV_READ_BASELINE,
        "env reads fell to {n}: lower ENV_READ_BASELINE in this file to {n} in the same commit"
    );
}
