// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Writes `desktop/src/ipc/generated/`: the TypeScript declarations of every type that crosses
//! IPC (ts-rs), the command, error-code and event names (`commands.ts`, `errors.ts`,
//! `events.ts`) from this crate's constants, the core's error codes and target constants
//! (`core-errors.ts`, `target.ts`), and `fixtures/target-names.json`, the core's own answers to
//! the target-name rule.
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

use apprafter_core::doctor::{
    Check, CheckFix, CheckGroup, CheckId, CheckStatus, DoctorReport, GroupId, RenewWhy,
};
use apprafter_core::error::codes;
use apprafter_core::kube::{KubeErrorKind, KubeVersion};
use apprafter_core::machine::{
    DeprecationView, MachineCatalogue, MachineOfferView, RegionLatency, RegionView,
};
use apprafter_core::provider::{SkipReason, TokenCheck, Verification, SUPPORTED_PROVIDERS};
use apprafter_core::session::{CliDefaultTarget, Identity, WhoamiReport, WhoamiTarget};
use apprafter_core::ssh::{SshKeyCandidate, SshKeyInfo};
use apprafter_core::target::{
    validate_name, CliDefaultPointer, MachineSet, ProvisionedServer, ProvisionedState,
    PublicAddress, SkuCheck, TargetAdded, TargetListReport, TargetRemoved, TargetRenamed,
    TargetRenewed, TargetReport, TargetSummary, TargetUsed, TokenPresence, UnreadableTarget,
};
use apprafter_core::tools::{
    HintOs, InstallHint, ToolId, ToolProblem, ToolStatus, ToolchainReport,
};
use apprafter_core::{
    ActivePointerChange, ChangeAction, CoreError, Outcome, PathSource, PlanClass, PlannedChange,
};
use apprafter_desktop_ipc::{
    errors, AppInfo, AuthInfo, AuthMethod, AuthOutcome, AutoLock, CancelledBy, CatalogueSourceArg,
    DraftId, LockReason, LockState, OpEvent, OpId, OpState, OpSummary, Os, OutputStream, PlanView,
    Quitting, Refresh, SecretBackend, SessionEvents, Settings, Subscribed, SubscriptionId,
    TargetAddArgs, Theme, TokenVerified, UiError, UnavailableReason, ALLOWED_WHILE_LOCKED,
    COMMANDS, LOCK_CHANGED, QUITTING,
};
use cli_core::Tier;
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
        AppInfo, Os, SecretBackend, SessionEvents,
        // ops.rs
        OpId, OpEvent, OutputStream, PlanView, OpSummary, OpState, Subscribed, SubscriptionId,
        // quit.rs
        Quitting,
        // targets.rs
        DraftId, TokenVerified, TargetAddArgs, CatalogueSourceArg,
        // apprafter-core, as OpEvent and PlanView carry them
        UiError, PlanClass, PlannedChange, ChangeAction, Outcome<serde_json::Value>,
        // apprafter-core, D.3 (overview §3.5–§3.9)
        ActivePointerChange, PathSource, TargetSummary, UnreadableTarget, CliDefaultPointer,
        TargetListReport, ProvisionedServer, ProvisionedState, TokenPresence, TargetReport,
        PublicAddress, SkuCheck, TargetAdded, TargetRenewed, TargetUsed, TargetRenamed, TargetRemoved,
        MachineSet, TokenCheck, Verification, SkipReason, RegionView, DeprecationView,
        MachineOfferView, MachineCatalogue, RegionLatency, SshKeyInfo, SshKeyCandidate, Identity,
        WhoamiTarget, CliDefaultTarget, WhoamiReport, ToolId, HintOs, InstallHint, ToolProblem,
        ToolStatus, ToolchainReport, GroupId, CheckStatus, CheckId, RenewWhy, CheckFix, Check,
        CheckGroup, DoctorReport, KubeErrorKind, KubeVersion,
    );
    fs::write(tmp.path().join("commands.ts"), commands_ts()).unwrap();
    fs::write(tmp.path().join("errors.ts"), errors_ts()).unwrap();
    fs::write(tmp.path().join("events.ts"), events_ts()).unwrap();
    fs::write(tmp.path().join("core-errors.ts"), core_errors_ts()).unwrap();
    fs::write(tmp.path().join("target.ts"), target_ts()).unwrap();
    fs::create_dir_all(tmp.path().join("fixtures")).unwrap();
    fs::write(
        tmp.path().join("fixtures/target-names.json"),
        target_names_json(),
    )
    .unwrap();

    let files = files_under(tmp.path());
    check(tmp.path(), &files);
    // JSON has no comment syntax (scripts/check-spdx-headers.sh exempts it).
    for file in files
        .iter()
        .filter(|f| f.extension().is_none_or(|e| e != "json"))
    {
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

/// `apprafter::target::not_found` → `TARGET_NOT_FOUND`: the prefix goes, `::` becomes `_`.
fn core_errors_ts() -> String {
    let mut pairs: Vec<(String, &str)> = codes::ALL
        .iter()
        .map(|code| {
            let key = code
                .strip_prefix("apprafter::")
                .unwrap_or_else(|| panic!("{code} lacks apprafter::"))
                .replace("::", "_")
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
    pairs.sort();
    let keys: BTreeSet<&String> = pairs.iter().map(|(k, _)| k).collect();
    assert_eq!(keys.len(), pairs.len(), "two codes make one key");
    let mut out = String::from(
        "// Generated from apprafter-core's error::codes by `just desktop-ipc-types`. Do not edit.\n\
         \n\
         /** The codes the core raises or passes through as `UiError.code`. */\n\
         export const CORE_ERROR_CODES = {\n",
    );
    for (key, code) in pairs {
        out.push_str(&format!("  {key}: {},\n", ts_string(code)));
    }
    out.push_str("} as const;\n");
    out
}

/// Every tier, in order. The match stops compiling when cli-core gains a tier.
fn all_tiers() -> [Tier; 4] {
    let all = [Tier::Solo, Tier::Team, Tier::Prod, Tier::Regulated];
    for tier in all {
        match tier {
            Tier::Solo | Tier::Team | Tier::Prod | Tier::Regulated => {}
        }
    }
    all
}

/// D.3a's `apprafter_core::target::TARGET_NAME_MAX_LEN`, exported as is; the probe only
/// cross-checks it against `validate_name` (bytes: the rule is `len()`).
fn target_name_max_len() -> usize {
    let max = apprafter_core::target::TARGET_NAME_MAX_LEN;
    assert!(
        validate_name(&"a".repeat(max)).is_ok() && validate_name(&"a".repeat(max + 1)).is_err(),
        "TARGET_NAME_MAX_LEN is validate_name's limit"
    );
    max
}

/// D.3a's `cli_core::target::HETZNER_TOKEN_LEN`, exported as is; the probe only cross-checks it
/// against `validate_hetzner_token_format`.
fn hetzner_token_len() -> usize {
    let len = cli_core::target::HETZNER_TOKEN_LEN;
    for (n, ok) in [(len - 1, false), (len, true), (len + 1, false)] {
        assert_eq!(
            cli_core::validate_hetzner_token_format(&"a".repeat(n)).is_ok(),
            ok,
            "HETZNER_TOKEN_LEN vs {n}"
        );
    }
    len
}

fn target_ts() -> String {
    let tiers: String = all_tiers()
        .iter()
        .map(|t| {
            format!(
                "  {{ id: {}, level: {} }},\n",
                ts_string(&t.to_string()),
                t.level()
            )
        })
        .collect();
    format!(
        "// Generated from apprafter-core and cli-core by `just desktop-ipc-types`. Do not edit.\n\
         \n\
         /** The providers `target add` accepts. */\n\
         {}\n\
         /** The default-tier hints, in order, with the hardware tier each names. */\n\
         export const TIERS = [\n{tiers}] as const;\n\
         \n\
         /** The longest target name the core accepts, in UTF-8 bytes. */\n\
         export const TARGET_NAME_MAX_LEN = {};\n\
         \n\
         /** A Hetzner Cloud API token's length. */\n\
         export const HETZNER_TOKEN_LEN = {};\n",
        ts_array("SUPPORTED_PROVIDERS", SUPPORTED_PROVIDERS),
        target_name_max_len(),
        hetzner_token_len(),
    )
}

/// `validate_name` over cases that pin its order (length, then characters, then dashes) and its
/// unit (bytes): the frontend's rule answers each the same (rules.test.ts).
fn target_names_json() -> String {
    let max = target_name_max_len();
    let mut cases: Vec<String> = [
        "prod",
        "prod-eu-1",
        "A-1",
        "",
        "a b",
        "a_b",
        "a/b",
        "../x",
        "-prod",
        "prod-",
        "-",
        "pröd",
        "-a b",
    ]
    .map(String::from)
    .to_vec();
    cases.push("a".repeat(max));
    cases.push("a".repeat(max + 1));
    cases.push("ö".repeat(max / 2 + 1)); // over the limit in bytes, under it in UTF-16 units
    cases.push(format!("{} ", "a".repeat(max))); // too long and invalid: length first
    let rows: Vec<serde_json::Value> = cases
        .into_iter()
        .map(|name| {
            let problem = match validate_name(&name) {
                Ok(()) => serde_json::Value::Null,
                Err(problem) => UiError::from(&CoreError::InvalidTargetName {
                    name: name.clone(),
                    problem,
                })
                .fields["problem"]
                    .clone(),
            };
            serde_json::json!({ "name": name, "problem": problem })
        })
        .collect();
    format!("{}\n", serde_json::to_string_pretty(&rows).unwrap())
}

fn events_ts() -> String {
    format!(
        "// Generated from desktop/ipc/src/lock.rs and quit.rs by `just desktop-ipc-types`. Do \
         not edit.\n\
         \n\
         /** Emitted on every lock transition, with the new `LockState`. */\n\
         export const LOCK_CHANGED = {} as const;\n\
         \n\
         /** Emitted when a quit begins with operations running, with `Quitting`. */\n\
         export const QUITTING = {} as const;\n",
        ts_string(LOCK_CHANGED),
        ts_string(QUITTING),
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
