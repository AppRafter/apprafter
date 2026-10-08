// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Writes `desktop/src/ipc/generated/`: the TypeScript declarations of every type that crosses
//! IPC (ts-rs), and the command, error-code and event names (`commands.ts`, `errors.ts`,
//! `events.ts`) from this crate's constants.
//!
//! Running it IS the regeneration: `just desktop-ipc-types`. The output is committed, and
//! `scripts/check-desktop-ipc-types.sh` (CI, `just desktop-check`) fails when a fresh export
//! differs from the git index.
//!
//! Everything is written to a temporary directory first and copied over the committed one at the
//! end. ts-rs merges a second type into an existing file by seeking past its own first line, so
//! it must never see a committed file that carries the SPDX line in front of that one.
#![cfg(feature = "ts")]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use apprafter_core::{Outcome, PlanClass, PlannedChange};
use apprafter_desktop_ipc::{
    errors, AppInfo, AuthInfo, AuthMethod, AuthOutcome, AutoLock, CancelledBy, LockReason,
    LockState, OpEvent, OpId, OpState, OpSummary, Os, OutputStream, PlanView, Refresh,
    SecretBackend, Settings, Subscribed, SubscriptionId, Theme, UiError, UnavailableReason,
    ALLOWED_WHILE_LOCKED, COMMANDS, LOCK_CHANGED,
};
use ts_rs::TS;

const SPDX: &str = "// SPDX-License-Identifier: FSL-1.1-Apache-2.0\n";
/// The prefix every code in [`errors::ALL`] carries; the rest, upper-cased, is its key.
const ERROR_PREFIX: &str = "apprafter::desktop::";
/// desktop/biome.json `lineWidth`: an array that fits stays on one line, as Biome writes it.
const LINE_WIDTH: usize = 100;

/// Exports each type with its dependencies (`export_all`): `UiError.fields` pulls in ts-rs's
/// `JsonValue` (`serde_json/JsonValue.ts`), `OpEvent` the core's `Outcome`.
macro_rules! export_all {
    ($cfg:expr; $($ty:ty),+ $(,)?) => {
        $(
            <$ty as TS>::export_all($cfg)
                .unwrap_or_else(|e| panic!("exporting {}: {e}", stringify!($ty)));
        )+
    };
}

#[test]
fn export_the_typescript_bindings() {
    let tmp = tempfile::tempdir().unwrap();
    // `Config::new()`, never `from_env()`: a `TS_RS_*` variable in someone's shell must not
    // change the committed output. Every u64 here stays below 2^53 (ids, byte counts, epoch
    // milliseconds), so `number` holds it exactly.
    let cfg = ts_rs::Config::new()
        .with_out_dir(tmp.path())
        .with_large_int("number");
    export_all!(&cfg;
        // settings.rs
        Settings, Theme, AutoLock, Refresh,
        // lock.rs
        LockState, LockReason,
        // auth.rs
        AuthInfo, AuthOutcome, AuthMethod, CancelledBy, UnavailableReason,
        // app_info.rs
        AppInfo, Os, SecretBackend,
        // ops.rs
        OpId, OpEvent, OutputStream, PlanView, OpSummary, OpState, Subscribed, SubscriptionId,
        // apprafter-core, as OpEvent and PlanView carry them
        UiError, PlanClass, PlannedChange, Outcome<serde_json::Value>,
    );
    fs::write(tmp.path().join("commands.ts"), commands_ts()).unwrap();
    fs::write(tmp.path().join("errors.ts"), errors_ts()).unwrap();
    fs::write(tmp.path().join("events.ts"), events_ts()).unwrap();

    let files = files_under(tmp.path());
    check(tmp.path(), &files);
    for file in &files {
        let path = tmp.path().join(file);
        let body = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{SPDX}{body}")).unwrap();
    }

    let dest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("src/ipc/generated");
    if dest.exists() {
        fs::remove_dir_all(&dest).unwrap();
    }
    for file in &files {
        let to = dest.join(file);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(tmp.path().join(file), &to).unwrap();
    }
}

/// What the webview relies on, checked before anything is copied.
fn check(root: &Path, files: &BTreeSet<PathBuf>) {
    let mut lower = BTreeSet::new();
    for file in files {
        // macOS and Windows file systems ignore case: `Commands.ts` would overwrite `commands.ts`.
        let name = file.to_string_lossy().to_lowercase();
        assert!(lower.insert(name), "{} collides by case", file.display());
        let stem = file.file_stem().unwrap().to_string_lossy();
        // DOM and Node globals; a generated type of that name shadows them in every importer.
        assert!(
            !["Event", "Stream"].contains(&stem.as_ref()),
            "{}",
            file.display()
        );
        let body = fs::read_to_string(root.join(file)).unwrap();
        assert!(!body.contains("bigint"), "{}:\n{body}", file.display());
        for line in body.lines() {
            assert!(
                !line.starts_with("export type Event ") && !line.starts_with("export type Stream "),
                "{}: {line}",
                file.display()
            );
            // `import type { A } from "./B";`: the target must be one of the generated files.
            if let Some((_, from)) = line
                .strip_prefix("import ")
                .and_then(|l| l.split_once(" from "))
            {
                let target = from.trim_end_matches(';').trim_matches('"');
                let resolved = file.parent().unwrap().join(format!("{target}.ts"));
                let resolved = normalise(&resolved);
                assert!(
                    files.contains(&resolved),
                    "{}: {line} does not resolve ({})",
                    file.display(),
                    resolved.display()
                );
            }
        }
    }
}

/// `serde_json/../OpEvent.ts` → `OpEvent.ts`, without touching the file system.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => assert!(out.pop(), "{} escapes", path.display()),
            other => out.push(other),
        }
    }
    out
}

/// Every file below `root`, relative to it.
fn files_under(root: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                out.insert(path.strip_prefix(root).unwrap().to_path_buf());
            }
        }
    }
    out
}

fn commands_ts() -> String {
    format!(
        "// Generated from desktop/ipc/src/commands.rs by `just desktop-ipc-types`. Do not edit.\n\
         \n\
         /** Every command the shell registers. */\n\
         {}\n\
         /** What a locked app still answers; everything else fails with `DESKTOP_ERROR_CODES.LOCKED`. */\n\
         {}",
        ts_array("COMMANDS", COMMANDS),
        ts_array("ALLOWED_WHILE_LOCKED", ALLOWED_WHILE_LOCKED),
    )
}

fn errors_ts() -> String {
    let mut codes: Vec<(String, &str)> = errors::ALL
        .iter()
        .map(|code| {
            let key = code
                .strip_prefix(ERROR_PREFIX)
                .unwrap_or_else(|| panic!("{code} lacks {ERROR_PREFIX}"))
                .to_ascii_uppercase();
            assert!(
                key.starts_with(|c: char| c.is_ascii_uppercase())
                    && key
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "{code} does not make a TypeScript key"
            );
            (key, *code)
        })
        .collect();
    codes.sort();
    let mut out = String::from(
        "// Generated from desktop/ipc/src/errors.rs by `just desktop-ipc-types`. Do not edit.\n\
         \n\
         /** The codes the desktop itself raises as `UiError.code`; core codes pass through. */\n\
         export const DESKTOP_ERROR_CODES = {\n",
    );
    for (key, code) in codes {
        out.push_str(&format!("  {key}: {},\n", ts_string(code)));
    }
    out.push_str("} as const;\n");
    out
}

fn events_ts() -> String {
    format!(
        "// Generated from desktop/ipc/src/lock.rs by `just desktop-ipc-types`. Do not edit.\n\
         \n\
         /** Emitted on every lock transition, with the new `LockState`. */\n\
         export const LOCK_CHANGED = {} as const;\n",
        ts_string(LOCK_CHANGED),
    )
}

/// `export const NAME = [...] as const;`, sorted (the lists are sets), laid out as Biome would.
fn ts_array(name: &str, items: &[&str]) -> String {
    let mut items: Vec<&str> = items.to_vec();
    items.sort_unstable();
    let quoted: Vec<String> = items.into_iter().map(ts_string).collect();
    let one_line = format!("export const {name} = [{}] as const;", quoted.join(", "));
    if one_line.len() <= LINE_WIDTH {
        return format!("{one_line}\n");
    }
    let mut out = format!("export const {name} = [\n");
    for item in quoted {
        out.push_str(&format!("  {item},\n"));
    }
    out.push_str("] as const;\n");
    out
}

/// A single-quoted TypeScript string (desktop/biome.json `quoteStyle`). The names are plain
/// identifiers; anything that would need escaping is refused rather than escaped.
fn ts_string(value: &str) -> String {
    assert!(
        !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_graphic() && c != '\'' && c != '\\'),
        "{value:?} needs escaping"
    );
    format!("'{value}'")
}
