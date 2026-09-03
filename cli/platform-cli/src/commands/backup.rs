// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter export` / `apprafter backup` — 2.6d export + backup command
//! logic.
//!
//! Two kinds of data pull, sharing the same native-extraction engine
//! (`cli_providers::backup`):
//!
//! * **`export`** (Kind 1) — pull native data (pg dumps, volume tars, redis
//!   snapshots) to a plain local folder + a `manifest.json`. No CRs, no
//!   secrets, no encryption. A debugging / one-off-recovery convenience.
//!
//! * **`backup`** (Kind 2) — the same extraction PLUS the serialized config
//!   and app CRs, PLUS the decrypted user secrets, all staged and then
//!   wrapped into an encrypted `restic` repository. This is the
//!   disaster-recovery artifact [`crate::commands::restore::run_restore`]
//!   consumes.
//!
//! ## Default scope = WHOLE CLUSTER
//!
//! Both commands default to every namespace that hosts an AppRafter
//! `Application` — the *app-namespace set*, derived from
//! `kubectl get applications.apprafter.io -A`, NOT `kubectl get ns` (the
//! latter would sweep in platform/system namespaces we must never replay).
//! `--namespace <ns>` (repeatable) / `--select` narrows the set.
//!
//! ## User vs platform discrimination (H1 — load-bearing)
//!
//! A restore must NOT clobber the bootstrap's own platform objects, so the
//! backup captures user material only:
//!
//! * **Argo `Application`s** are filtered to those carrying the
//!   `apprafter.io/managed-by=apprafter` label ([`is_user_argo_app`]). The
//!   platform umbrella + component Argo Applications LACK it → never
//!   serialized (else restore double-owns them against bootstrap).
//! * **Config CRs** are captured by KIND (`PlatformStack/default`,
//!   `SourceCredential` cluster-wide), not by an namespace sweep. There is no
//!   in-cluster `Infrastructure` CR (M2: it is the local manifest) — its
//!   topology rides `manifest.platform_version`, and a missing
//!   `infrastructures.apprafter.io` listing is expected, never an error.
//!
//! ## SourceCredential material — follow-the-reference (rev-5)
//!
//! `SourceCredential` CRs and their sealed material live in
//! `apprafter-system`, OUTSIDE the app-namespace set, so the app-ns secret
//! sweep MISSES them. Instead, for each `SourceCredential` we resolve its
//! `spec.git.backend.sealedSecretRef` + `spec.registry.backend.sealedSecretRef`
//! ([`sourcecred_material_refs`]) and read the underlying
//! controller-unsealed Secret directly. Two distinct secret-capture paths:
//! (a) app user secrets — SealedSecret-backed sweep scoped to the app-ns set;
//! (b) SourceCredential material — follow-the-reference, cluster-wide.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use backup_core::engine::BackupOpts;
use backup_core::extract::plan_extraction;
use backup_core::prune::{run_prune, RetentionPolicy};
use backup_core::restic::{restic_check_argv, restic_unlock_argv};
use backup_core::{KubeExec, ResticRunner, StagingMode, SubprocessRestic};
use base64::Engine as _;
use cli_core::tools::{preflight_tools, KUBECTL, RESTIC};
use cli_core::{CliError, Result};
use cli_providers::backup::extract::run_extraction;
use cli_providers::backup::images::pg_helper_image;
use cli_providers::backup::manifest::BackupManifest;
use cli_providers::backup::restic::restic_snapshots_argv;
use cli_providers::backup::ResourceRef;
use cli_providers::k8s::kubectl::KubectlCli;
use cli_providers::k8s::sealing::{build_sealed_secret, fetch_controller_public_key};
use serde_json::Value;
use tempfile::NamedTempFile;

use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, kubectl_get_json_cluster_wide,
    kubectl_merge_patch,
};
use crate::commands::state_paths::resolve_state_paths;

/// Namespace the `PlatformStack` singleton + `SourceCredential`s + their sealed
/// material live in. Mirrors `repo_creds::SOURCECRED_NAMESPACE` /
/// `platform::PLATFORMSTACK_NAMESPACE` (both private to their modules).
/// Exported `pub(crate)` so `restore.rs` can use it without re-declaring.
pub(crate) const APPRAFTER_SYSTEM_NAMESPACE: &str = "apprafter-system";
pub(crate) const PLATFORMSTACK_NAME: &str = "default";
/// Namespace the `PlatformStack` singleton lives in — the `spec.backup`
/// merge-patch target. Alias of [`APPRAFTER_SYSTEM_NAMESPACE`], named to mirror
/// `platform::PLATFORMSTACK_NAMESPACE` at the merge-patch call sites.
pub(crate) const PLATFORMSTACK_NAMESPACE: &str = APPRAFTER_SYSTEM_NAMESPACE;

/// Platform defaults for the CRD-required `spec.backup` fields the CLI must
/// always emit (the CRD drops the CUE `*`-defaults; see [`backup_enable_patch`]).
/// These MUST stay in sync with `schemas/v1alpha1/platformstack.cue` +
/// `platform-stack` `#BackupValues` so a bare `enable` reproduces the platform
/// default exactly.
const DEFAULT_STAGING_MODE: &str = "monolithic";

/// The two cron expressions the platform shipped before 2.22g, kept as the
/// REGRESSION ANCHOR for the new composition.
///
/// Production no longer reads them: `resolve_schedule` composes the crons from
/// `--at` (default 03:00) and the derived check time. They exist so one test
/// can assert the bare default still produces these exact strings — because
/// the danger in replacing a schedule surface is not that it rejects a value,
/// it is that it silently MOVES everybody's backup window on upgrade.
#[cfg(test)]
const DEFAULT_BACKUP_SCHEDULE: &str = "0 3 * * *";
#[cfg(test)]
const DEFAULT_CHECK_SCHEDULE: &str = "0 6 * * 0";

// ---------------------------------------------------------------------------
// Schedule surface (2.22g / D2)
// ---------------------------------------------------------------------------

/// A resolved backup schedule: the two cron expressions and the IANA zone they
/// are to be interpreted in.
///
/// Composed by the CLI from `--at` / `--check` / `--timezone` so neither the
/// cron grammar nor the timezone is ever the operator's problem. An operator
/// says *when*; steps, ranges and minute-granularity mean nothing for a
/// nightly backup, and a time without a zone is not a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedSchedule {
    pub schedule: String,
    pub check_schedule: String,
    /// IANA name. Empty only if the caller deliberately omits it, which the
    /// enable path never does — it refuses instead.
    pub time_zone: String,
}

/// Where the timezone came from, so the CLI can say so rather than leaving the
/// operator to guess whether a zone was chosen or assumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZoneSource {
    Flag,
    TzEnv,
    OperatingSystem,
}

impl ZoneSource {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::Flag => "--timezone",
            Self::TzEnv => "$TZ",
            Self::OperatingSystem => "this machine",
        }
    }
}

/// Parse `--at HH:MM` on a 24-hour clock.
///
/// Strict on purpose. Accepting `3pm` or `03:00:00` would mean carrying two
/// grammars for one value, and rejecting `3:00` would buy no correctness — so
/// a single-digit hour is normalised and everything else is refused with a
/// message that shows the shape.
pub(crate) fn parse_at(raw: &str) -> Result<(u32, u32)> {
    let bad = || {
        CliError::Other(format!(
            "invalid --at '{raw}': expected a 24-hour time HH:MM between 00:00              and 23:59, e.g. --at 03:00"
        ))
    };
    let (h, m) = raw.split_once(':').ok_or_else(bad)?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return Err(bad());
    }
    if !h.bytes().all(|b| b.is_ascii_digit()) || !m.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let (h, m): (u32, u32) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if h > 23 || m > 59 {
        return Err(bad());
    }
    Ok((h, m))
}

/// `HH:MM` → the daily cron for that time.
pub(crate) fn compose_daily(h: u32, m: u32) -> String {
    format!("{m} {h} * * *")
}

/// `HH:MM` → the weekly cron for that time on Sunday.
///
/// The day is a product decision, not a flag: the check is weekly, and a
/// second knob here would be the cron field this change exists to remove.
pub(crate) fn compose_weekly_sunday(h: u32, m: u32) -> String {
    format!("{m} {h} * * 0")
}

/// The check time when `--check` is not given: three hours after the backup,
/// same minute.
///
/// Chosen so the bare default reproduces the platform's historical
/// `0 3 * * *` / `0 6 * * 0` pair BYTE-IDENTICALLY — upgrading and re-running
/// `enable` with no schedule flags must not move anybody's window. The point
/// of the offset is only that the check never starts in the same minute as a
/// backup; it is not a claim that the check follows the backup (`--at 23:00`
/// puts the check at 02:00, which is earlier in that Sunday).
pub(crate) fn derive_check_time(h: u32, m: u32) -> (u32, u32) {
    ((h + 3) % 24, m)
}

/// Cheap shape check for an IANA zone name — `Area/Location`, or one of the
/// handful of single-word zones.
///
/// Deliberately NOT a tzdb lookup: carrying a timezone database in the CLI to
/// validate a string the apiserver validates anyway would be a second source
/// of truth that goes stale. This rejects the shapes that are definitely not
/// IANA names — in particular the POSIX `TZ` specs like `CET-1CEST,M3.5.0`
/// that must never reach `spec.timeZone`.
pub(crate) fn validate_zone_shape(zone: &str) -> Result<()> {
    let bad = || {
        CliError::Other(format!(
            "invalid timezone '{zone}': expected an IANA name like              `Europe/Berlin` or `UTC`"
        ))
    };
    if zone.is_empty() || zone.len() > 64 {
        return Err(bad());
    }
    let ok_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+');
    if !zone.chars().all(ok_char) {
        return Err(bad());
    }
    // A POSIX spec such as `EST5EDT` has no slash and is not one of the known
    // single-word zones; a leading/trailing slash is malformed either way.
    if zone.starts_with('/') || zone.ends_with('/') {
        return Err(bad());
    }
    const SINGLE_WORD: &[&str] = &["UTC", "GMT", "UCT", "Zulu", "Universal", "Greenwich"];
    if !zone.contains('/') && !SINGLE_WORD.contains(&zone) {
        return Err(bad());
    }
    Ok(())
}

/// Resolve the zone the schedule runs in: the flag, then `$TZ`, then the
/// operating system — and REFUSE if none of them answers.
///
/// Injected rather than reading the environment itself, so the precedence is
/// testable without mutating process-global state in a parallel test suite.
///
/// # Why it refuses instead of defaulting to UTC
///
/// A time without a zone is not a time. UTC is a reasonable thing to *ask*
/// for and a poor thing to *assume*: an operator who types `--at 03:00` means
/// three in the morning where they are, and silently storing that as 03:00 UTC
/// produces a backup that runs at the wrong hour with nothing anywhere saying
/// so. Refusing costs one flag; guessing costs a wrong answer nobody can see.
///
/// `$TZ` is consulted because an operator who exports it reasonably expects it
/// honoured — but only when it looks like an IANA name. POSIX `TZ` specs
/// (`CET-1CEST,M3.5.0,M10.5.0/3`, `:/etc/localtime`) fall through to the OS
/// rather than being written into `spec.timeZone`, where they mean nothing.
pub(crate) fn resolve_time_zone(
    flag: Option<&str>,
    tz_env: Option<&str>,
    os_zone: Option<&str>,
) -> Result<(String, ZoneSource)> {
    if let Some(z) = flag {
        validate_zone_shape(z)?;
        return Ok((z.to_string(), ZoneSource::Flag));
    }
    if let Some(z) = tz_env.filter(|z| validate_zone_shape(z).is_ok()) {
        return Ok((z.to_string(), ZoneSource::TzEnv));
    }
    if let Some(z) = os_zone.filter(|z| validate_zone_shape(z).is_ok()) {
        return Ok((z.to_string(), ZoneSource::OperatingSystem));
    }
    Err(CliError::Other(
        "could not determine this machine's timezone, and a time of day \
         without a zone is not a time.\n\n           Tried: --timezone (not given), $TZ, and the operating system.\n\n           Pass the zone explicitly:\n               apprafter backup enable … --at 03:00 --timezone Europe/Berlin\n           Or, to run the schedule in UTC, say so:\n               apprafter backup enable … --at 03:00 --timezone UTC"
            .into(),
    ))
}

/// Turn the `--at` / `--check` / `--timezone` flags into the two crons and the
/// zone, given the two ambient zone candidates — PURE.
///
/// Extracted from [`resolve_schedule`] (which is this function plus the two
/// environment reads and the "where the zone came from" line) so the whole
/// composition — the `--at` default, the `--check off` sentinel, the `--check`
/// error rewording, the derived check time — is testable without mutating
/// process-global state in a parallel test suite.
pub(crate) fn resolve_schedule_from(
    o: &EnableOpts,
    tz_env: Option<&str>,
    os_zone: Option<&str>,
) -> Result<(ResolvedSchedule, ZoneSource)> {
    let (h, m) = match o.at.as_deref() {
        Some(raw) => parse_at(raw)?,
        None => (3, 0),
    };
    let check_schedule = match o.check.as_deref() {
        Some("off") => String::new(),
        Some(raw) => {
            let (ch, cm) = parse_at(raw).map_err(|e| {
                CliError::Other(format!("{e}").replace("--at", "--check") + " (or `--check off`)")
            })?;
            compose_weekly_sunday(ch, cm)
        }
        None => {
            let (ch, cm) = derive_check_time(h, m);
            compose_weekly_sunday(ch, cm)
        }
    };
    let (time_zone, source) = resolve_time_zone(o.timezone.as_deref(), tz_env, os_zone)?;
    Ok((
        ResolvedSchedule {
            schedule: compose_daily(h, m),
            check_schedule,
            time_zone,
        },
        source,
    ))
}

/// Turn the `--at` / `--check` / `--timezone` flags into the two crons and the
/// zone, or fail with a message the operator can act on.
///
/// Impure only in that it reads `$TZ` and asks the OS for its zone; everything
/// it decides with is passed to [`resolve_schedule_from`], which is pure.
pub(crate) fn resolve_schedule(o: &EnableOpts) -> Result<ResolvedSchedule> {
    let tz_env = std::env::var("TZ").ok();
    let os_zone = iana_time_zone::get_timezone().ok();
    let (resolved, source) = resolve_schedule_from(o, tz_env.as_deref(), os_zone.as_deref())?;
    if source != ZoneSource::Flag {
        println!(
            "  using timezone {} (from {}); pass --timezone to override",
            resolved.time_zone,
            source.describe()
        );
    }
    Ok(resolved)
}

/// One line describing a resolved schedule, for the success message.
pub(crate) fn describe_schedule(s: &ResolvedSchedule) -> String {
    let daily = cron_to_at(&s.schedule)
        .map(|(h, m)| format!("backup daily at {h:02}:{m:02}"))
        .unwrap_or_else(|| format!("backup on `{}`", s.schedule));
    let check = if s.check_schedule.is_empty() {
        "integrity check off".to_string()
    } else {
        cron_to_at(&s.check_schedule)
            .map(|(h, m)| format!("check Sundays at {h:02}:{m:02}"))
            .unwrap_or_else(|| format!("check on `{}`", s.check_schedule))
    };
    format!("{daily}, {check},")
}

/// `"0 3 * * *"` + `Some("Europe/Berlin")` → `"daily at 03:00 Europe/Berlin"`.
///
/// A cron this CLI would not have written is shown VERBATIM — summarising
/// `*/5 * * * *` as a time would be a confident wrong answer about somebody's
/// hand-edited schedule.
pub(crate) fn describe_cron_daily(cron: &str, zone: Option<&str>) -> String {
    match cron_to_at(cron) {
        Some((h, m)) => format!("daily at {h:02}:{m:02} {}", zone_label(zone)),
        None => format!("{cron} {}", zone_label(zone)),
    }
}

/// The same for the weekly check, which this CLI always writes on Sunday.
pub(crate) fn describe_cron_weekly(cron: &str, zone: Option<&str>) -> String {
    let f: Vec<&str> = cron.split_whitespace().collect();
    if f.len() == 5 && f[2] == "*" && f[3] == "*" && f[4] == "0" {
        if let (Ok(m), Ok(h)) = (f[0].parse::<u32>(), f[1].parse::<u32>()) {
            if h < 24 && m < 60 {
                return format!("Sundays at {h:02}:{m:02} {}", zone_label(zone));
            }
        }
    }
    format!("{cron} {}", zone_label(zone))
}

/// The zone suffix — or a statement that there is none.
///
/// NOT silently omitted when absent. A bare `03:00` reads as local time, and
/// on a cluster without the field it is the kube-controller-manager's zone,
/// which is precisely the thing nobody could find out.
fn zone_label(zone: Option<&str>) -> String {
    match zone {
        Some(z) if !z.is_empty() => z.to_string(),
        _ => "(cluster timezone — re-run `backup enable` to pin one)".to_string(),
    }
}

/// Render a stored cron back as `HH:MM`, for `backup status`.
///
/// `None` for anything this CLI would not have written — a hand-edited
/// expression stays shown verbatim rather than being mis-summarised as a time
/// it does not mean.
pub(crate) fn cron_to_at(cron: &str) -> Option<(u32, u32)> {
    let f: Vec<&str> = cron.split_whitespace().collect();
    if f.len() != 5 || f[2] != "*" || f[3] != "*" {
        return None;
    }
    let m: u32 = f[0].parse().ok()?;
    let h: u32 = f[1].parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some((h, m))
}

// ---------------------------------------------------------------------------
// Pure helpers (the tested core — some exported pub(crate) for restore.rs)
// ---------------------------------------------------------------------------

/// Distinct, sorted namespaces of the AppRafter `Application` CRs. When
/// `select` is non-empty the result is intersected with it (the operator
/// asked for a subset).
///
/// `apprafter_apps` is the `.items[]` array of
/// `kubectl get applications.apprafter.io -A -o json`. The app-namespace set
/// derives from THESE, never from `kubectl get ns` — that distinction is the
/// whole point of the H1 review (platform/system namespaces must never enter
/// the backup scope).
pub fn app_namespaces(apprafter_apps: &[Value], select: &[String]) -> Vec<String> {
    let mut set: Vec<String> = apprafter_apps
        .iter()
        .filter_map(|a| {
            a.pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    set.sort();
    set.dedup();
    if select.is_empty() {
        set
    } else {
        set.into_iter().filter(|ns| select.contains(ns)).collect()
    }
}

/// Resolve the backup passphrase: explicit `--passphrase` → `RESTIC_PASSWORD`
/// env → (on a TTY) an interactive masked prompt. The repository holds
/// DECRYPTED secrets, so an empty / absent passphrase is NEVER allowed: when
/// neither source is set and we're not on a TTY, this errors instead of
/// silently producing an unencrypted-by-empty-key repo.
pub fn backup_passphrase_or_error(
    arg: Option<&str>,
    env: Option<&str>,
    is_tty: bool,
) -> Result<String> {
    if let Some(p) = arg.or(env) {
        if p.is_empty() {
            return Err(CliError::Other(
                "empty backup passphrase — the repository holds decrypted secrets and must be \
                 encrypted; pass a non-empty `--passphrase` or set RESTIC_PASSWORD"
                    .into(),
            ));
        }
        return Ok(p.to_string());
    }
    if !is_tty {
        return Err(CliError::Other(
            "no backup passphrase — pass `--passphrase <value>`, set RESTIC_PASSWORD, or run from \
             an interactive shell for a prompt (the repo holds decrypted secrets and must be \
             encrypted)"
                .into(),
        ));
    }
    let pass = inquire::Password::new("Backup passphrase:")
        .with_display_mode(inquire::PasswordDisplayMode::Masked)
        .with_help_message("encrypts the restic repository; you'll need it to restore")
        .prompt()
        .map_err(|e| CliError::Other(format!("passphrase prompt: {e}")))?;
    if pass.is_empty() {
        return Err(CliError::Other(
            "passphrase cannot be empty — the repo holds decrypted secrets".into(),
        ));
    }
    Ok(pass)
}

/// `(namespace, name)` of each sealed material Secret a `SourceCredential`
/// references via `spec.git.backend.sealedSecretRef` +
/// `spec.registry.backend.sealedSecretRef`. The `namespace` field of each ref
/// is optional and DEFAULTS to the SourceCredential's own namespace
/// (`apprafter-system`) — matching the operator's resolution
/// (`operator-core::sourcecredential::SealedSecretRef`). The launch default
/// points both refs at the same material Secret, so the result may contain
/// duplicates; the caller dedups.
pub fn sourcecred_material_refs(sc: &Value) -> Vec<(String, String)> {
    let own_ns = sc
        .pointer("/metadata/namespace")
        .and_then(Value::as_str)
        .unwrap_or(APPRAFTER_SYSTEM_NAMESPACE);
    let mut refs = Vec::new();
    for ptr in [
        "/spec/git/backend/sealedSecretRef",
        "/spec/registry/backend/sealedSecretRef",
    ] {
        if let Some(r) = sc.pointer(ptr) {
            if let Some(name) = r.pointer("/name").and_then(Value::as_str) {
                let ns = r
                    .pointer("/namespace")
                    .and_then(Value::as_str)
                    .unwrap_or(own_ns)
                    .to_string();
                refs.push((ns, name.to_string()));
            }
        }
    }
    refs
}

// ---------------------------------------------------------------------------
// 2a. `apprafter backup enable` / `disable` — spec.backup patch builders (pure)
// ---------------------------------------------------------------------------

/// Construct the restic S3 repo URL from either a full URL or a bare bucket
/// name + endpoint.
///
/// If `bucket` already carries a restic backend scheme — starts with one of
/// `s3:`, `b2:`, `gs:`, `azure:`, `swift:`, `sftp:`, `rest:`, `rclone:` —
/// OR is an explicit local path (`/…`, `./…`) → return it VERBATIM. If
/// `endpoint` (or `prefix`) is also given in that case → error.
///
/// Otherwise `bucket` is a bare name:
/// * `endpoint` is REQUIRED — else error naming `--endpoint`.
/// * Strip a leading `https://` / `http://` scheme from the endpoint and strip
///   trailing `/`. Default scheme is `https`; `http://` is honored.
/// * Build `s3:<scheme>://<endpoint>/<bucket>` and append `/<prefix>` when
///   `prefix` is `Some` (leading/trailing slashes trimmed on the prefix).
pub(crate) fn construct_repo_url(
    bucket: &str,
    endpoint: Option<&str>,
    prefix: Option<&str>,
) -> Result<String> {
    // Recognised restic backend scheme prefixes.
    const SCHEMES: &[&str] = &[
        "s3:", "b2:", "gs:", "azure:", "swift:", "sftp:", "rest:", "rclone:",
    ];
    let is_full_url = SCHEMES.iter().any(|s| bucket.starts_with(s))
        || bucket.starts_with('/')
        || bucket.starts_with("./");

    if is_full_url {
        if endpoint.is_some() || prefix.is_some() {
            return Err(CliError::Other(
                "pass EITHER a full repo URL in --bucket OR --bucket <name> + --endpoint, not both"
                    .into(),
            ));
        }
        return Ok(bucket.to_string());
    }

    // Bare bucket name — endpoint is required.
    let raw_endpoint = endpoint.ok_or_else(|| {
        CliError::Other(format!(
            "bare bucket name '{bucket}' needs --endpoint <host> \
             (e.g. --endpoint nbg1.your-objectstorage.com), \
             or pass a full restic URL like s3:https://<host>/<bucket>"
        ))
    })?;

    // Normalise the endpoint: detect and strip leading scheme; remember whether
    // the user explicitly wrote http:// (honour it) or not (default https).
    let (scheme, host_rest) = if let Some(rest) = raw_endpoint.strip_prefix("http://") {
        ("http", rest)
    } else if let Some(rest) = raw_endpoint.strip_prefix("https://") {
        ("https", rest)
    } else {
        ("https", raw_endpoint)
    };
    let host = host_rest.trim_end_matches('/');

    let mut url = format!("s3:{scheme}://{host}/{bucket}");
    if let Some(p) = prefix {
        let trimmed = p.trim_matches('/');
        if !trimmed.is_empty() {
            url.push('/');
            url.push_str(trimmed);
        }
    }
    Ok(url)
}

/// Options for `apprafter backup enable`, mapped 1:1 onto the
/// `PlatformStack.spec.backup` CRD block (camelCase). `bucket` + `credential`
/// are mandatory; every other field is an override the operator may leave to
/// the chart/operator default (omitted from the patch when `None`).
#[derive(Default)]
pub(crate) struct EnableOpts {
    /// Restic S3 repository URL → `spec.backup.bucket`.
    pub bucket: String,
    /// Cluster credential Secret name → `spec.backup.credentialRef.name`.
    pub credential: String,
    /// `--at HH:MM` — the local time of day the backup runs.
    pub at: Option<String>,
    /// `--timezone` — IANA zone override; `None` resolves from the machine.
    pub timezone: Option<String>,
    /// `spec.backup.retention.keepDaily`.
    pub keep_daily: Option<u32>,
    /// `spec.backup.retention.keepWeekly`.
    pub keep_weekly: Option<u32>,
    /// `spec.backup.retention.keepMonthly`.
    pub keep_monthly: Option<u32>,
    /// `spec.backup.retention.enforce` (`operator` | `cluster`).
    pub enforce: Option<String>,
    /// `spec.backup.stagingMode` (`monolithic` | `sequential`).
    pub staging_mode: Option<String>,
    /// `--check off|HH:MM` — disable the weekly check, or set its time.
    pub check: Option<String>,
    /// `spec.backup.failureWebhook` URL.
    pub failure_webhook: Option<String>,
}

/// Build the JSON merge-patch body `{"spec":{"backup":{…}}}` for
/// `apprafter backup enable`.
///
/// `enabled:true`, `bucket`, and `credentialRef:{name}` are always present.
/// `schedule` / `stagingMode` / `checkSchedule` / `failureWebhook` appear only
/// when their option is `Some`. The nested `retention` object contains only the
/// keys whose option is `Some`, and is omitted ENTIRELY when none of
/// `keep_daily` / `keep_weekly` / `keep_monthly` / `enforce` is set — a bare
/// enable then leaves retention to the operator/chart default rather than
/// merge-patching an empty object.
///
/// Pure: no I/O, no validation of enum values (the impure caller
/// [`run_backup_enable`] validates `enforce` / `staging_mode` before calling).
pub(crate) fn backup_enable_patch(o: &EnableOpts, s: &ResolvedSchedule) -> serde_json::Value {
    let mut backup = serde_json::Map::new();
    backup.insert("enabled".to_string(), Value::Bool(true));
    backup.insert("bucket".to_string(), Value::String(o.bucket.clone()));
    backup.insert(
        "credentialRef".to_string(),
        serde_json::json!({ "name": o.credential }),
    );

    // The PlatformStack CRD marks schedule / stagingMode / checkSchedule /
    // checkReadData REQUIRED whenever `spec.backup` is present (the CRD drops
    // the CUE `*`-defaults, so the apiserver won't fill them and would reject a
    // partial patch; the operator's `BackupConfig` likewise deserializes them
    // as non-`Option` `String`s). So the patch must always carry a concrete
    // value — the flag when given, else the platform default (identical to the
    // CUE / chart `#BackupValues` defaults, so behaviour is unchanged from
    // "the platform default").
    backup.insert("schedule".to_string(), Value::String(s.schedule.clone()));
    backup.insert(
        "stagingMode".to_string(),
        Value::String(
            o.staging_mode
                .clone()
                .unwrap_or_else(|| DEFAULT_STAGING_MODE.to_string()),
        ),
    );
    // Always present, and possibly EMPTY: `checkSchedule` is CRD-required, so
    // an empty string is the only way to say "no weekly check" — which is what
    // `--check off` writes and what the chart's guard omits the CronJob on.
    backup.insert(
        "checkSchedule".to_string(),
        Value::String(s.check_schedule.clone()),
    );
    if !s.time_zone.is_empty() {
        backup.insert("timeZone".to_string(), Value::String(s.time_zone.clone()));
    }
    backup.insert("checkReadData".to_string(), Value::Bool(false));
    if let Some(hook) = &o.failure_webhook {
        backup.insert("failureWebhook".to_string(), Value::String(hook.clone()));
    }

    let mut retention = serde_json::Map::new();
    if let Some(d) = o.keep_daily {
        retention.insert("keepDaily".to_string(), Value::from(d));
    }
    if let Some(w) = o.keep_weekly {
        retention.insert("keepWeekly".to_string(), Value::from(w));
    }
    if let Some(m) = o.keep_monthly {
        retention.insert("keepMonthly".to_string(), Value::from(m));
    }
    if let Some(e) = &o.enforce {
        retention.insert("enforce".to_string(), Value::String(e.clone()));
    }
    if !retention.is_empty() {
        backup.insert("retention".to_string(), Value::Object(retention));
    }

    serde_json::json!({ "spec": { "backup": Value::Object(backup) } })
}

/// Build the JSON merge-patch body `{"spec":{"backup":{"enabled":false}}}` for
/// `apprafter backup disable` — flips `enabled` off while retaining every other
/// configured field (merge-patch only touches the keys it names).
pub(crate) fn backup_disable_patch() -> serde_json::Value {
    serde_json::json!({ "spec": { "backup": { "enabled": false } } })
}

// ---------------------------------------------------------------------------
// Impure helpers (walk-validated)
// ---------------------------------------------------------------------------

/// `.items[]` of `kubectl get <resource> [-n ns | -A] -o json`, or an empty
/// `Vec` when the resource lists nothing / its CRD is absent (e.g.
/// `infrastructures.apprafter.io`, which legitimately has no instances — M2).
/// Exported `pub(crate)` for use by `restore.rs`.
pub(crate) fn list_items(
    resource: &str,
    namespace: Option<&str>,
    kubeconfig: &Path,
) -> Result<Vec<Value>> {
    match kubectl_get_json_cluster_wide(resource, namespace, kubeconfig) {
        Ok(Some(v)) => Ok(items_of(&v)),
        Ok(None) => Ok(Vec::new()),
        Err(e) => {
            // A missing CRD (no `infrastructures` kind) is not a backup failure.
            if is_missing_resource_kind(&e) {
                Ok(Vec::new())
            } else {
                Err(e)
            }
        }
    }
}

/// `.items[]` of a kubectl list response, or an empty `Vec` when the document
/// carries no `items` array. Pure — extracted from [`list_items`] and called
/// from both there and the tests.
fn items_of(list: &Value) -> Vec<Value> {
    list.get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Is this kubectl failure the "that kind does not exist on this server" one?
///
/// Pure — extracted from [`list_items`] and called from both there and the
/// tests. INVARIANT: only THIS shape is swallowed into an empty list. Backup
/// legitimately lists kinds a cluster may not have (`infrastructures` has no
/// instances at M2), but widening this to any kubectl error would turn a
/// connection failure mid-backup into a silently empty, restorable-looking
/// backup.
fn is_missing_resource_kind(e: &CliError) -> bool {
    let msg = format!("{e}");
    msg.contains("the server doesn't have a resource type")
        || msg.contains("doesn't have a resource type")
}

/// Read the full `.data` of a Secret (all keys), base64-decoding each value,
/// as a `name → bytes` map, plus the secret's `.type` field (defaulting to
/// `"Opaque"` when absent). Returns `Ok(None)` when the Secret is absent.
///
/// Exported `pub(crate)` for use by `restore.rs` (which needs raw connection
/// creds for the pg load path — it ignores the returned type for that use).
#[allow(clippy::type_complexity)]
pub(crate) fn read_secret_data(
    name: &str,
    namespace: &str,
    kubeconfig: &Path,
) -> Result<Option<(BTreeMap<String, Vec<u8>>, String)>> {
    let json = kubectl_get_json("secret", Some(name), Some(namespace), kubeconfig)?;
    let Some(json) = json else { return Ok(None) };
    decode_secret_json(&json, name, namespace).map(Some)
}

/// Decode a Secret document's `.data` (base64 per value) plus its `.type`.
///
/// Pure — extracted from [`read_secret_data`], which is only this function
/// plus the kubectl fetch, and called from both there and the tests.
#[allow(clippy::type_complexity)]
fn decode_secret_json(
    json: &Value,
    name: &str,
    namespace: &str,
) -> Result<(BTreeMap<String, Vec<u8>>, String)> {
    let secret_type = json
        .pointer("/type")
        .and_then(Value::as_str)
        .unwrap_or("Opaque")
        .to_string();
    let mut out = BTreeMap::new();
    if let Some(data) = json.pointer("/data").and_then(Value::as_object) {
        for (k, v) in data {
            let b64 = v.as_str().unwrap_or("");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    CliError::Other(format!("decode secret {namespace}/{name} key {k}: {e}"))
                })?;
            out.insert(k.clone(), bytes);
        }
    }
    Ok((out, secret_type))
}

/// Read `PlatformStack/default.status.currentVersion` (the live platform-stack
/// version) so `restore --reprovision` bootstraps the target at the same
/// version. Falls back to `"unknown"` when the field is unset (a freshly
/// bootstrapped cluster whose operator hasn't stamped status yet).
/// Exported `pub(crate)` for use by `restore.rs`.
pub(crate) fn read_platform_version(kubeconfig: &Path) -> Result<String> {
    let ps = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(APPRAFTER_SYSTEM_NAMESPACE),
        kubeconfig,
    )?;
    Ok(platform_version_of(ps.as_ref()))
}

/// `PlatformStack.status.currentVersion`, or `"unknown"`.
///
/// Pure — extracted from [`read_platform_version`] and called from both there
/// and the tests. INVARIANT: the fallback is the literal `"unknown"`, which
/// `restore --reprovision` treats as "no version to pin"; an empty string here
/// would be passed on as a version and bootstrap a target at nothing.
fn platform_version_of(ps: Option<&Value>) -> String {
    ps.and_then(|p| p.pointer("/status/currentVersion"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// Build the `ResourceRef`s recorded in `manifest.json` from the captured
/// config CRs + app CRs + claims. Pure-ish (operates on already-fetched
/// JSON), kept private since it just shapes the manifest body.
fn resource_refs(crs: &[(&str, &Value)], claims: &[Value]) -> Vec<ResourceRef> {
    let mut refs = Vec::new();
    for (kind, cr) in crs {
        refs.push(ResourceRef {
            namespace: cr
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            kind: (*kind).to_string(),
            name: cr
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            claim_type: None,
        });
    }
    for c in claims {
        refs.push(ResourceRef {
            namespace: c
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            kind: "ResourceClaim".to_string(),
            name: c
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            claim_type: c
                .pointer("/spec/type")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    refs
}

/// The CNPG operator's own namespace, where the lazily-provisioned shared
/// integrated `platform-postgres` Cluster lives (never in an app namespace).
const CNPG_OPERATOR_NS: &str = "cnpg-system";

/// The CNPG operand image of the first CNPG Cluster found across the app
/// namespaces AND `cnpg-system`, used to pick a major-matched `pg_dump` helper
/// image. Falls back to the default pg image when none is found.
///
/// The `cnpg-system` scan is load-bearing: integrated-tier claims all share the
/// `platform-postgres` Cluster there (see the resourceclaim-provisioner), which
/// never appears in an app namespace, so an app-ns-only scan structurally
/// misses it and always falls back to the default major. For each Cluster CR
/// the image is read from `spec.imageName` first and, when that is unset (CNPG
/// derives the operand image from its own default or an ImageCatalogRef — the
/// common case, so `spec.imageName` is typically EMPTY), from `status.image`
/// (the resolved operand image CNPG stamps once the Cluster is running).
/// Without the `status.image` fallback a modern CNPG (PG 18) would silently
/// mismatch a `postgres:16` `pg_dump` (`pg_dump: server version mismatch`).
pub(crate) fn first_cnpg_image(namespaces: &[String], kubeconfig: &Path) -> Option<String> {
    for ns in cnpg_scan_namespaces(namespaces) {
        if let Ok(items) = list_items("clusters.postgresql.cnpg.io", Some(ns), kubeconfig) {
            if let Some(img) = items.iter().find_map(cnpg_cluster_image) {
                return Some(img);
            }
        }
    }
    None
}

/// The namespaces [`first_cnpg_image`] scans, in order: the app namespaces
/// first (per-claim owned clusters, if any), then the CNPG operator's own
/// namespace — deduped so `cnpg-system` is not scanned twice when it is itself
/// an app namespace.
///
/// Pure — extracted from [`first_cnpg_image`] and called from both there and
/// the tests. INVARIANT: `cnpg-system` is ALWAYS in the set. The shared
/// integrated `platform-postgres` Cluster lives only there, so an app-ns-only
/// scan structurally misses it and every integrated-tier backup silently falls
/// back to the default pg major → `pg_dump: server version mismatch`.
fn cnpg_scan_namespaces(namespaces: &[String]) -> Vec<&str> {
    let mut scan: Vec<&str> = namespaces.iter().map(String::as_str).collect();
    if !scan.contains(&CNPG_OPERATOR_NS) {
        scan.push(CNPG_OPERATOR_NS);
    }
    scan
}

/// Resolve a CNPG `Cluster`'s operand image: `spec.imageName` when set, else the
/// resolved `status.image` (populated once the Cluster is running, even when the
/// image comes from a default/ImageCatalogRef rather than an explicit spec).
fn cnpg_cluster_image(c: &Value) -> Option<String> {
    c.pointer("/spec/imageName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            c.pointer("/status/image")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
}

/// Enumerate ResourceClaims across the given namespaces (cluster-wide when the
/// set is the whole cluster), flattened into a single `Vec`.
fn claims_in_namespaces(namespaces: &[String], kubeconfig: &Path) -> Result<Vec<Value>> {
    let mut all = Vec::new();
    for ns in namespaces {
        all.extend(list_items(
            "resourceclaims.apprafter.io",
            Some(ns),
            kubeconfig,
        )?);
    }
    Ok(all)
}

/// Resolve the default output directory for `export`: `<cwd>/apprafter-export`.
fn default_export_dir() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("apprafter-export")
}

/// Default restic repo path for a target: `<config>/backups/<target>`.
fn default_backup_repo(target_name: &str) -> Result<PathBuf> {
    let root = cli_core::target::default_config_root()?;
    Ok(root.join("backups").join(target_name))
}

/// The local restic repo a `backup` / `backup list` acts on: the `--repo`
/// override, else the target's default under the config root.
///
/// Extracted from [`run_backup`] / [`run_backup_list`] and called from both
/// those and the tests. INVARIANT: the default is PER TARGET — two clusters
/// sharing one repo path would interleave their snapshots and each other's
/// retention.
fn backup_repo_path(repo: Option<&str>, target_name: &str) -> Result<PathBuf> {
    match repo {
        Some(r) => Ok(PathBuf::from(r)),
        None => default_backup_repo(target_name),
    }
}

/// The refusal when the cluster hosts no AppRafter `Application` at all.
///
/// Pure — extracted from [`run_export`] / [`run_backup`] and called from both
/// those and the tests. INVARIANT: it names where the scope came from. The
/// scope is the app-namespace set derived from the `Application` CRs, NOT
/// `kubectl get ns`, and an operator staring at a cluster full of namespaces
/// needs to be told that is deliberate.
fn no_applications_error(action: &str) -> CliError {
    CliError::Other(format!(
        "no AppRafter Applications found — nothing to {action}. (Scope derives from \
         `kubectl get applications.apprafter.io -A`.)"
    ))
}

// ---------------------------------------------------------------------------
// Concrete KubeExec impl — subprocess kubectl
// ---------------------------------------------------------------------------

/// Maximum stderr lines to retain for error reporting.
const STDERR_CAPTURE_LIMIT: usize = 20;

/// Grace period after `child.wait()` to let the stderr-drainer thread flush.
const STDERR_FLUSH_GRACE_MS: u64 = 100;

/// CLI's concrete implementation of [`backup_core::KubeExec`]: shells out to
/// `kubectl` with `KUBECONFIG=<path>`.
pub(crate) struct KubectlExec {
    pub kubeconfig: PathBuf,
    /// The `kubectl` binary to spawn. Always `"kubectl"` in production (see
    /// [`KubectlExec::new`]); a seam so the tests can drive these methods
    /// against a stub binary and actually observe what they do with a child
    /// process's streams and exit status, rather than leaving the whole
    /// subprocess layer unexercised.
    kubectl_bin: PathBuf,
}

impl KubectlExec {
    pub(crate) fn new(kubeconfig: PathBuf) -> Self {
        Self {
            kubeconfig,
            kubectl_bin: PathBuf::from(KUBECTL_BIN),
        }
    }
}

/// The `kubectl` executable [`KubectlExec`] spawns, resolved through `PATH`.
const KUBECTL_BIN: &str = "kubectl";

/// Spawn a thread that drains `reader` to EOF, retaining the last
/// `STDERR_CAPTURE_LIMIT` lines in a shared buffer for error reporting.
fn spawn_capturing_drainer<R: Read + Send + 'static>(reader: R) -> Arc<Mutex<Vec<String>>> {
    let buf: Arc<Mutex<Vec<String>>> =
        Arc::new(Mutex::new(Vec::with_capacity(STDERR_CAPTURE_LIMIT)));
    let buf_clone = Arc::clone(&buf);
    thread::spawn(move || {
        let reader = BufReader::new(reader);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let mut guard = buf_clone.lock().unwrap();
            if guard.len() >= STDERR_CAPTURE_LIMIT {
                guard.remove(0);
            }
            guard.push(line);
        }
    });
    buf
}

fn format_exec_error(
    context: &str,
    status: std::process::ExitStatus,
    stderr_buf: &Arc<Mutex<Vec<String>>>,
) -> CliError {
    thread::sleep(Duration::from_millis(STDERR_FLUSH_GRACE_MS));
    let captured = stderr_buf.lock().unwrap();
    if captured.is_empty() {
        CliError::Other(format!(
            "{context}: kubectl exec exited with {status} and produced no stderr output"
        ))
    } else {
        let text = captured.join("\n");
        CliError::Other(format!(
            "{context}: kubectl exec exited with {status}.\nkubectl stderr:\n  {}",
            text.replace('\n', "\n  ")
        ))
    }
}

impl KubeExec for KubectlExec {
    fn apply_and_wait_pod_ready(&self, spec: &serde_json::Value) -> Result<()> {
        let name = spec["metadata"]["name"]
            .as_str()
            .ok_or_else(|| CliError::Other("pod spec missing metadata.name".into()))?;
        let ns = spec["metadata"]["namespace"]
            .as_str()
            .ok_or_else(|| CliError::Other("pod spec missing metadata.namespace".into()))?;

        let json_bytes = serde_json::to_vec(spec)
            .map_err(|e| CliError::Other(format!("serialize pod spec: {e}")))?;

        let mut apply_child = Command::new(&self.kubectl_bin)
            .args(["apply", "-f", "-", "-n", ns])
            .env("KUBECONFIG", &self.kubeconfig)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CliError::Other(format!("spawn kubectl apply: {e}")))?;

        {
            let mut stdin = apply_child
                .stdin
                .take()
                .ok_or_else(|| CliError::Other("kubectl apply has no stdin".into()))?;
            stdin
                .write_all(&json_bytes)
                .map_err(|e| CliError::Other(format!("write pod spec to kubectl apply: {e}")))?;
        }

        let apply_stderr = apply_child
            .stderr
            .take()
            .ok_or_else(|| CliError::Other("kubectl apply has no stderr".into()))?;
        let apply_stderr_buf = spawn_capturing_drainer(apply_stderr);
        let apply_status = apply_child
            .wait()
            .map_err(|e| CliError::Other(format!("wait kubectl apply: {e}")))?;
        if !apply_status.success() {
            return Err(format_exec_error(
                "apply_and_wait_pod_ready(apply)",
                apply_status,
                &apply_stderr_buf,
            ));
        }

        let wait_status = Command::new(&self.kubectl_bin)
            .args([
                "wait",
                "--for=condition=Ready",
                &format!("pod/{name}"),
                "-n",
                ns,
                "--timeout=300s",
            ])
            .env("KUBECONFIG", &self.kubeconfig)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| CliError::Other(format!("spawn kubectl wait: {e}")))?;

        if wait_status.success() {
            Ok(())
        } else {
            Err(CliError::Other(format!(
                "pod {name} in {ns} did not reach Ready within 300s (kubectl wait exited {wait_status})"
            )))
        }
    }

    fn exec_stream_to_file(
        &self,
        pod: &str,
        ns: &str,
        argv: &[&str],
        out_path: &Path,
    ) -> Result<()> {
        let mut cmd = Command::new(&self.kubectl_bin);
        cmd.arg("exec")
            .arg(pod)
            .arg("-n")
            .arg(ns)
            .arg("--")
            .args(argv)
            .env("KUBECONFIG", &self.kubeconfig)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| CliError::Other(format!("spawn kubectl exec (stream-to-file): {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CliError::Other("kubectl exec has no stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| CliError::Other("kubectl exec has no stderr".into()))?;

        let stderr_buf = spawn_capturing_drainer(stderr);

        let mut out_file = std::fs::File::create(out_path).map_err(|e| {
            CliError::Other(format!("create output file {}: {e}", out_path.display()))
        })?;
        let mut reader = BufReader::new(stdout);
        io::copy(&mut reader, &mut out_file)
            .map_err(|e| CliError::Other(format!("copy kubectl exec stdout → file: {e}")))?;

        let status = child
            .wait()
            .map_err(|e| CliError::Other(format!("wait kubectl exec: {e}")))?;

        if status.success() {
            Ok(())
        } else {
            Err(format_exec_error(
                "exec_stream_to_file",
                status,
                &stderr_buf,
            ))
        }
    }

    fn exec_stream_from_file(
        &self,
        pod: &str,
        ns: &str,
        argv: &[&str],
        in_path: &Path,
    ) -> Result<()> {
        let mut cmd = Command::new(&self.kubectl_bin);
        cmd.arg("exec")
            .arg("-i")
            .arg(pod)
            .arg("-n")
            .arg(ns)
            .arg("--")
            .args(argv)
            .env("KUBECONFIG", &self.kubeconfig)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| CliError::Other(format!("spawn kubectl exec (stream-from-file): {e}")))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| CliError::Other("kubectl exec has no stdin".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| CliError::Other("kubectl exec has no stderr".into()))?;

        let stderr_buf = spawn_capturing_drainer(stderr);

        let mut in_file = std::fs::File::open(in_path)
            .map_err(|e| CliError::Other(format!("open input file {}: {e}", in_path.display())))?;
        match io::copy(&mut in_file, &mut stdin) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {}
            Err(e) => {
                return Err(CliError::Other(format!(
                    "copy file → kubectl exec stdin: {e}"
                )));
            }
        }
        drop(stdin);

        let status = child
            .wait()
            .map_err(|e| CliError::Other(format!("wait kubectl exec: {e}")))?;

        if status.success() {
            Ok(())
        } else {
            Err(format_exec_error(
                "exec_stream_from_file",
                status,
                &stderr_buf,
            ))
        }
    }

    fn delete_pod_best_effort(&self, name: &str, ns: &str) {
        let _ = Command::new(&self.kubectl_bin)
            .args([
                "delete",
                "pod",
                name,
                "-n",
                ns,
                "--ignore-not-found",
                "--wait=false",
            ])
            .env("KUBECONFIG", &self.kubeconfig)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn get_secret_key(&self, secret: &str, ns: &str, key: &str) -> Result<String> {
        let out = Command::new(&self.kubectl_bin)
            .args([
                "get",
                "secret",
                secret,
                "-n",
                ns,
                "-o",
                &format!("jsonpath={{.data.{key}}}"),
            ])
            .env("KUBECONFIG", &self.kubeconfig)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| CliError::Other(format!("spawn kubectl get secret: {e}")))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            return Err(CliError::Other(format!(
                "kubectl get secret {secret} -n {ns} -o jsonpath={{.data.{key}}} \
                 failed (exit {:?}): {stderr}",
                out.status.code()
            )));
        }

        let b64 = String::from_utf8(out.stdout)
            .map_err(|e| CliError::Other(format!("kubectl get secret stdout not utf-8: {e}")))?;

        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| {
                CliError::Other(format!(
                    "decode secret {secret}/{key} (value was not valid base64): {e}"
                ))
            })?;

        String::from_utf8(decoded)
            .map_err(|e| CliError::Other(format!("secret {secret}/{key} is not utf-8: {e}")))
    }

    fn get_json(&self, args: &[&str]) -> Result<Option<serde_json::Value>> {
        let mut c = Command::new(&self.kubectl_bin);
        c.args(args).env("KUBECONFIG", &self.kubeconfig);

        let out = c
            .output()
            .map_err(|e| CliError::Other(format!("spawn kubectl: {e}")))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("NotFound") || stderr.contains("not found") {
                return Ok(None);
            }
            return Err(CliError::Other(format!(
                "kubectl {:?} failed (exit {:?}): {stderr}",
                args.first().unwrap_or(&"?"),
                out.status.code()
            )));
        }

        let value: serde_json::Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| CliError::Other(format!("kubectl JSON parse: {e}")))?;
        Ok(Some(value))
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// `apprafter export` — pull native data (Kind 1) to a plain local folder.
///
/// Scope: the app-namespace set (whole cluster by default), narrowed by
/// `namespaces` when `select` is set. Writes `<out>/{pg,volumes,redis}/…`
/// plus a `<out>/manifest.json`. No CRs, no secrets, no encryption.
pub fn run_export(namespaces: &[String], select: bool, out: Option<&str>) -> Result<()> {
    // D11 / 2.22a: the external binaries this command spawns, checked
    // BEFORE any prompt, kubeconfig or provider call. The reported bug
    // was a passphrase typed into a command that could not have worked.
    preflight_tools(&[&KUBECTL], "apprafter export")?;

    let resolved = resolve_state_paths(None)?;
    let cluster_id = resolved.target_name.clone();
    let kc = ensure_kubeconfig_tempfile()?;

    let subset: &[String] = if select { namespaces } else { &[] };
    let apps = list_items("applications.apprafter.io", None, kc.path())?;
    let ns_set = app_namespaces(&apps, subset);
    if ns_set.is_empty() {
        return Err(no_applications_error("export"));
    }

    let out_dir = export_out_dir(out);
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| CliError::Other(format!("create export dir {}: {e}", out_dir.display())))?;

    let k = KubectlExec::new(kc.path().to_path_buf());
    let claims = claims_in_namespaces(&ns_set, kc.path())?;
    let plan = plan_extraction(&claims);
    let pg_image = pg_helper_image(first_cnpg_image(&ns_set, kc.path()).as_deref());
    run_extraction(&k, &plan, &out_dir, &pg_image)?;

    let platform_version = read_platform_version(kc.path())?;
    let manifest = export_manifest(&cluster_id, &platform_version, &ns_set, &claims);
    write_manifest(&manifest, &out_dir)?;

    print!(
        "{}",
        export_summary(&cluster_id, &out_dir, &ns_set, claims.len(), plan.len())
    );
    Ok(())
}

/// The output directory for `export`: the `--out` path, else
/// `<cwd>/apprafter-export`.
///
/// Extracted from [`run_export`] and called from both there and the tests.
fn export_out_dir(out: Option<&str>) -> PathBuf {
    match out {
        Some(p) => PathBuf::from(p),
        None => default_export_dir(),
    }
}

/// The `manifest.json` body an `export` writes. Pure — extracted from
/// [`run_export`] and called from both there and the tests.
///
/// INVARIANT: `resources` carries the claims and NO config CRs. `export` is
/// Kind 1 (native data only); a config CR appearing here would advertise
/// replayable cluster config that the export never actually captured.
fn export_manifest(
    cluster_id: &str,
    platform_version: &str,
    namespaces: &[String],
    claims: &[Value],
) -> BackupManifest {
    BackupManifest {
        manifest_version: backup_core::manifest::MANIFEST_VERSION_CURRENT,
        cluster_id: cluster_id.to_string(),
        created_at: now_rfc3339(),
        platform_version: platform_version.to_string(),
        namespaces: namespaces.to_vec(),
        resources: resource_refs(&[], claims),
    }
}

/// The operator-facing summary `export` prints on success. Pure — extracted
/// from [`run_export`], which prints exactly this.
fn export_summary(
    cluster_id: &str,
    out_dir: &Path,
    namespaces: &[String],
    claim_count: usize,
    extractable_count: usize,
) -> String {
    format!(
        "✓ Exported {} namespace(s) from cluster '{cluster_id}' → {}\n  namespaces: {}\n  claims:     {claim_count} ({extractable_count} extractable)\n",
        namespaces.len(),
        out_dir.display(),
        namespaces.join(", "),
    )
}

/// Parse the local-pull `apprafter backup --staging-mode` flag into a
/// [`StagingMode`].
///
/// * `None` / `Some("monolithic")` → [`StagingMode::Monolithic`] (the default —
///   stage every namespace's native data at once, one restic snapshot).
/// * `Some("sequential")` → [`StagingMode::Sequential`] (stage + snapshot one
///   namespace at a time; bounds peak staging disk on large clusters).
/// * anything else → `Err` naming the two accepted values.
pub(crate) fn parse_staging_mode(s: Option<&str>) -> Result<StagingMode> {
    match s {
        None | Some("monolithic") => Ok(StagingMode::Monolithic),
        Some("sequential") => Ok(StagingMode::Sequential),
        Some(other) => Err(CliError::Other(format!(
            "invalid --staging-mode '{other}': expected 'monolithic' or 'sequential'"
        ))),
    }
}

/// `apprafter backup` — full encrypted backup (Kind 2): native extraction +
/// serialized config/app CRs + decrypted user secrets, wrapped into a restic
/// repository.
pub fn run_backup(
    namespaces: &[String],
    select: bool,
    repo: Option<&str>,
    passphrase: Option<&str>,
    staging_mode: Option<&str>,
) -> Result<()> {
    // D11 / 2.22a: the external binaries this command spawns, checked
    // BEFORE any prompt, kubeconfig or provider call. The reported bug
    // was a passphrase typed into a command that could not have worked.
    preflight_tools(&[&RESTIC, &KUBECTL], "apprafter backup create")?;

    let staging_mode = parse_staging_mode(staging_mode)?;

    let resolved = resolve_state_paths(None)?;
    let cluster_id = resolved.target_name.clone();

    let env_pass = std::env::var("RESTIC_PASSWORD").ok();
    let is_tty = std::io::stdin().is_terminal();
    let pass = backup_passphrase_or_error(passphrase, env_pass.as_deref(), is_tty)?;

    // Keep the kubeconfig tempfile alive for the WHOLE sequence (every kubectl
    // shell-out below depends on it; dropping it deletes the file).
    let kc = ensure_kubeconfig_tempfile()?;

    // Resolve ns_set BEFORE handing off to the engine (the engine's list_items
    // uses KubeExec which doesn't know about the "app-namespace set" concept —
    // that's a CLI-layer concern).
    let k = KubectlExec::new(kc.path().to_path_buf());
    let subset: &[String] = if select { namespaces } else { &[] };
    let apps = list_items("applications.apprafter.io", None, kc.path())?;
    let ns_set = app_namespaces(&apps, subset);
    if ns_set.is_empty() {
        return Err(no_applications_error("back up"));
    }

    let repo_path = backup_repo_path(repo, &cluster_id)?;
    let repo_str = repo_path.to_string_lossy().to_string();

    let pg_image = pg_helper_image(first_cnpg_image(&ns_set, kc.path()).as_deref());
    let platform_version = read_platform_version(kc.path())?;

    // Stage everything under a tempdir; the engine writes data/ under this root.
    let staging = tempfile::Builder::new()
        .prefix("apprafter-backup-")
        .tempdir()
        .map_err(|e| CliError::Other(format!("create staging dir: {e}")))?;

    let opts = local_pull_backup_opts(
        &repo_str,
        pass,
        &cluster_id,
        &platform_version,
        &ns_set,
        select,
        staging.path(),
        pg_image,
        staging_mode,
    );

    let r = SubprocessRestic;
    let summary = backup_core::engine::run_backup_with_summary(&k, &r, &opts)?;

    print!(
        "{}",
        backup_summary_report(&cluster_id, &repo_str, &ns_set, &summary)
    );
    Ok(())
}

/// Assemble the [`BackupOpts`] the CLI local-pull path hands to the engine.
///
/// Pure — extracted from [`run_backup`] and called from both there and the
/// tests. INVARIANT: `backup_host` is `None`. The CLI pull keeps the operator
/// workstation's own hostname as the restic group, which is what makes
/// per-station grouping work; only the in-cluster runner pins
/// `Some("apprafter-backup")` because its pod name is ephemeral (spec
/// §Retention M-r3-1a).
#[allow(clippy::too_many_arguments)]
fn local_pull_backup_opts(
    repo: &str,
    passphrase: String,
    cluster_id: &str,
    platform_version: &str,
    namespaces: &[String],
    is_subset: bool,
    staging_root: &Path,
    pg_image: String,
    staging_mode: StagingMode,
) -> BackupOpts {
    BackupOpts {
        repo: repo.to_string(),
        passphrase,
        cluster_id: cluster_id.to_string(),
        created_at: now_rfc3339(),
        platform_version: platform_version.to_string(),
        namespaces: namespaces.to_vec(),
        is_subset,
        staging_root: staging_root.to_path_buf(),
        pg_image,
        staging_mode,
        backup_host: None,
    }
}

/// The operator-facing summary `backup` prints on success. Pure — extracted
/// from [`run_backup`], which prints exactly this.
///
/// INVARIANT: the `snapshot:` line is present only when restic reported a
/// snapshot id. Printing an empty one would read as a stored snapshot that
/// does not exist.
fn backup_summary_report(
    cluster_id: &str,
    repo: &str,
    namespaces: &[String],
    summary: &backup_core::engine::BackupSummary,
) -> String {
    let mut out = format!(
        "✓ Backed up cluster '{cluster_id}' → {repo}\n  namespaces: {}\n  captured:   {} CR(s), {} secret(s), {} claim(s) ({} extracted)\n  tag:        {}\n",
        namespaces.join(", "),
        summary.cr_count,
        summary.secret_count,
        summary.claim_count,
        summary.extracted_count,
        summary.tag,
    );
    if let Some(id) = &summary.snapshot_id {
        out.push_str(&format!("  snapshot:   {id}\n"));
    }
    out
}

/// `apprafter backup list` — list the snapshots in a restic repo.
pub fn run_backup_list(repo: Option<&str>, passphrase: Option<&str>) -> Result<()> {
    // D11 / 2.22a: the external binaries this command spawns, checked
    // BEFORE any prompt, kubeconfig or provider call. The reported bug
    // was a passphrase typed into a command that could not have worked.
    preflight_tools(&[&RESTIC], "apprafter backup list")?;

    let resolved = resolve_state_paths(None)?;
    let env_pass = std::env::var("RESTIC_PASSWORD").ok();
    let is_tty = std::io::stdin().is_terminal();
    let pass = backup_passphrase_or_error(passphrase, env_pass.as_deref(), is_tty)?;

    let repo_path = backup_repo_path(repo, &resolved.target_name)?;
    let repo_str = repo_path.to_string_lossy().to_string();

    let r = SubprocessRestic;
    let json = r.run_stdout(&restic_snapshots_argv(&repo_str), &pass)?;
    let snapshots = parse_snapshots_json(&json)?;

    print!("{}", format_snapshot_table(&repo_str, &snapshots));
    Ok(())
}

/// Parse `restic snapshots --json` output into the snapshot array.
///
/// Pure — extracted from [`run_backup_list`] and called from both there and
/// the tests. A document that is valid JSON but not an array yields an empty
/// list (rendered as "no snapshots"), never a panic.
fn parse_snapshots_json(json: &str) -> Result<Vec<Value>> {
    let parsed: Value = serde_json::from_str(json)
        .map_err(|e| CliError::Other(format!("parse restic snapshots JSON: {e}")))?;
    Ok(parsed.as_array().cloned().unwrap_or_default())
}

/// Render the `backup list` snapshot table. Pure — extracted from
/// [`run_backup_list`], which prints exactly this.
///
/// INVARIANT: an absent `short_id` falls back to the full `id` TRUNCATED to 8
/// characters. `restic` takes either, and printing a full 64-hex id in a
/// 12-wide column would wreck the table it is supposed to line up.
fn format_snapshot_table(repo: &str, snapshots: &[Value]) -> String {
    if snapshots.is_empty() {
        return format!("No snapshots in {repo}.\n");
    }
    let mut out = format!("Snapshots in {repo}:\n{:<12}  {:<25}  TAGS\n", "ID", "TIME");
    for s in snapshots {
        let id = s
            .pointer("/short_id")
            .or_else(|| s.pointer("/id"))
            .and_then(Value::as_str)
            .map(|i| i.chars().take(8).collect::<String>())
            .unwrap_or_else(|| "?".to_string());
        let time = s.pointer("/time").and_then(Value::as_str).unwrap_or("?");
        let tags = s
            .pointer("/tags")
            .and_then(Value::as_array)
            .map(|t| {
                t.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        out.push_str(&format!("{id:<12}  {time:<25}  {tags}\n"));
    }
    out
}

fn write_manifest(manifest: &BackupManifest, dir: &Path) -> Result<()> {
    let body = serde_json::to_vec_pretty(manifest)
        .map_err(|e| CliError::Other(format!("serialize manifest: {e}")))?;
    std::fs::write(dir.join("manifest.json"), body)
        .map_err(|e| CliError::Other(format!("write manifest.json: {e}")))
}

/// Current time as an RFC3339 string (manifest `created_at` + tag timestamp).
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---------------------------------------------------------------------------
// Operator S3 credential helpers (pub(crate) — consumed by backup
// enable/prune/check/unlock/restore in later tasks).
// ---------------------------------------------------------------------------

/// Parse a dotenv-style string into a `KEY → VALUE` map.
///
/// Rules:
/// * Blank lines and lines whose first non-whitespace character is `#` are
///   skipped.
/// * Split on the **first** `=` only — values may contain `=`.
/// * Whitespace around both key and value is trimmed.
/// * Lines with no `=` are ignored.
pub(crate) fn parse_credential_file(contents: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_string();
            let value = trimmed[eq_pos + 1..].trim().to_string();
            if !key.is_empty() {
                map.insert(key, value);
            }
        }
    }
    map
}

/// Canonical internal credential key set (stored in Secrets + used internally).
/// The in-cluster CronJob also reads these canonical names and maps them to
/// `AWS_*` before invoking restic.
///
/// `RESTIC_PASSWORD` is already S3-vendor-neutral so it keeps its name.
/// `S3_REGION` is optional — many S3-compatible stores don't need it.
const REQUIRED_CRED_KEYS: &[&str] = &[
    "S3_ACCESS_KEY_ID",
    "S3_SECRET_ACCESS_KEY",
    "RESTIC_PASSWORD",
];

/// Human-readable description of the required credential keys, with alias note.
/// Referenced by every error that needs to enumerate the keys.
const CRED_KEYS_HELP: &str =
    "the backup credential needs these keys: S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY, \
     RESTIC_PASSWORD (optional: S3_REGION). \
     AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / AWS_DEFAULT_REGION are accepted as aliases. \
     Provide them via --credential-file <dotenv> (KEY=VALUE lines), or point --credential at \
     a Secret already sealed with those keys (canonical S3_* or AWS_* aliases).";

/// All env keys probed in the fallback env-lookup path (both canonical + aliases).
/// The alias lookup is used ONLY for the env-var path; the file path normalises
/// after parsing.
const ALL_S3_ENV_KEYS: &[&str] = &[
    "S3_ACCESS_KEY_ID",
    "S3_SECRET_ACCESS_KEY",
    "S3_REGION",
    "RESTIC_PASSWORD",
    // AWS aliases:
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_DEFAULT_REGION",
];

/// Normalise a raw `KEY → VALUE` map (from a dotenv file or env-var lookup) to
/// the canonical `S3_*` internal key set.
///
/// Accepted input forms:
/// * `S3_ACCESS_KEY_ID` / `S3_SECRET_ACCESS_KEY` / `S3_REGION` — kept as-is.
/// * `AWS_ACCESS_KEY_ID` → `S3_ACCESS_KEY_ID`
/// * `AWS_SECRET_ACCESS_KEY` → `S3_SECRET_ACCESS_KEY`
/// * `AWS_DEFAULT_REGION` → `S3_REGION`
/// * `RESTIC_PASSWORD` — kept as-is (already neutral).
/// * Any other key is passed through unchanged (dotenv files may carry extras).
///
/// When both the canonical and alias form are present, the canonical form wins.
/// Two-pass: first insert alias→canonical mappings, then overwrite with any
/// explicit canonical (`S3_*`) keys so they always beat aliases regardless of
/// iteration order.
pub(crate) fn normalize_s3_creds(raw: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();

    // Pass 1: insert everything, translating alias keys to their canonical name.
    for (k, v) in &raw {
        let canonical = match k.as_str() {
            "AWS_ACCESS_KEY_ID" => "S3_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY" => "S3_SECRET_ACCESS_KEY",
            "AWS_DEFAULT_REGION" => "S3_REGION",
            _ => k.as_str(),
        };
        out.insert(canonical.to_string(), v.clone());
    }

    // Pass 2: canonical (S3_*) keys always overwrite any alias value that
    // landed in the same slot during pass 1.
    for (k, v) in &raw {
        match k.as_str() {
            "S3_ACCESS_KEY_ID" | "S3_SECRET_ACCESS_KEY" | "S3_REGION" | "RESTIC_PASSWORD" => {
                out.insert(k.clone(), v.clone());
            }
            _ => {}
        }
    }

    out
}

/// Translate the canonical `S3_*` credential map to the `AWS_*` names that
/// `restic` expects on its subprocess environment.
///
/// * `S3_ACCESS_KEY_ID` → `AWS_ACCESS_KEY_ID`
/// * `S3_SECRET_ACCESS_KEY` → `AWS_SECRET_ACCESS_KEY`
/// * `S3_REGION` → `AWS_DEFAULT_REGION`
/// * `RESTIC_PASSWORD` — passed through unchanged.
/// * Any other key — passed through unchanged (for extra env entries).
pub(crate) fn translate_creds_for_restic(
    canonical: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (k, v) in canonical {
        let restic_key = match k.as_str() {
            "S3_ACCESS_KEY_ID" => "AWS_ACCESS_KEY_ID",
            "S3_SECRET_ACCESS_KEY" => "AWS_SECRET_ACCESS_KEY",
            "S3_REGION" => "AWS_DEFAULT_REGION",
            _ => k.as_str(),
        };
        out.insert(restic_key.to_string(), v.clone());
    }
    out
}

/// Validate that all required canonical credential keys are present and
/// non-empty. Returns `Err` naming any missing key plus the full help text.
pub(crate) fn validate_required_cred_keys(canonical: &BTreeMap<String, String>) -> Result<()> {
    let missing: Vec<&str> = REQUIRED_CRED_KEYS
        .iter()
        .copied()
        .filter(|k| {
            canonical
                .get(*k)
                .map(String::as_str)
                .unwrap_or("")
                .is_empty()
        })
        .collect();
    if !missing.is_empty() {
        return Err(CliError::Other(format!(
            "missing credential key(s): {} — {CRED_KEYS_HELP}",
            missing.join(", ")
        )));
    }
    Ok(())
}

/// Resolve operator-side S3 credentials for restic off-site backup verbs.
///
/// * `cred_file = Some(path)` — read and parse that dotenv file; normalises
///   `AWS_*` aliases to canonical `S3_*` keys.
/// * `cred_file = None` — probes `env_lookup` for both canonical (`S3_*`) and
///   alias (`AWS_*`) names; normalises to canonical.
///
/// In both cases the result uses canonical `S3_*` key names. Callers that drive
/// a local `restic` subprocess MUST call [`translate_creds_for_restic`] before
/// injecting the map as env vars.
///
/// Returns an error when any of the required canonical keys
/// (`S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `RESTIC_PASSWORD`) is absent
/// or empty. The error message names the missing keys and explains both input
/// paths.
///
/// The `env_lookup` parameter is an injectable seam for testing; production
/// callers pass `&|k| std::env::var(k).ok()`.
pub(crate) fn resolve_operator_s3_creds(
    cred_file: Option<&std::path::Path>,
    env_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>> {
    let raw = if let Some(path) = cred_file {
        let contents = std::fs::read_to_string(path).map_err(|e| {
            CliError::Other(format!("read credential file {}: {e}", path.display()))
        })?;
        parse_credential_file(&contents)
    } else {
        let mut m = BTreeMap::new();
        for &key in ALL_S3_ENV_KEYS {
            if let Some(val) = env_lookup(key) {
                m.insert(key.to_string(), val);
            }
        }
        m
    };

    let canonical = normalize_s3_creds(raw);
    validate_required_cred_keys(&canonical)?;
    Ok(canonical)
}

/// Inject all entries from `creds` as environment variables on `cmd`,
/// translating canonical `S3_*` keys to the `AWS_*` names restic expects.
///
/// Used by the backup operator verbs (prune / check / unlock / restore) to
/// forward S3 + restic credentials to the subprocess without persisting them
/// in shell history or temporary files.
pub(crate) fn apply_creds_to_command(cmd: &mut Command, creds: &BTreeMap<String, String>) {
    for (k, v) in translate_creds_for_restic(creds) {
        cmd.env(k, v);
    }
}

// ---------------------------------------------------------------------------
// Operator-side restic maintenance verbs — prune / check / unlock
//
// These run OUTSIDE the cluster, on the operator's workstation, with the
// operator's FULL S3 creds (from `--credential-file` or env). They reach an
// `s3:` repo directly via a [`CredentialedRestic`] runner that injects the
// AWS_* + RESTIC_PASSWORD env on every restic Command (unlike the in-cluster
// scheduled path, which uses scoped creds mounted into the CronJob).
// ---------------------------------------------------------------------------

/// A [`ResticRunner`] that injects operator S3 credentials (AWS_* +
/// RESTIC_PASSWORD) onto every restic subprocess, WITHOUT mutating the global
/// process environment. Mirrors [`SubprocessRestic`]'s error handling
/// (non-zero exit → `Err` carrying stderr), adding the creds on top so restic
/// can reach an `s3:` repo the plain `SubprocessRestic` can't.
///
/// The `ResticRunner` trait is declared over `cli_core::Result` — the SAME
/// `Result`/`CliError` platform-cli uses — so these methods return exactly the
/// caller's error type; no cross-error mapping is needed at the call sites.
struct CredentialedRestic {
    creds: BTreeMap<String, String>,
}

impl CredentialedRestic {
    /// Build the restic Command for `argv`, applying the operator creds and the
    /// `RESTIC_PASSWORD` env. `pass` and `creds["RESTIC_PASSWORD"]` are the same
    /// value (`resolve_operator_s3_creds` guarantees the key is present); the
    /// explicit `RESTIC_PASSWORD` set from `pass` honours the trait contract
    /// while `apply_creds_to_command` carries the AWS_* keys.
    fn command(&self, argv: &[String], pass: &str) -> Command {
        let mut c = Command::new("restic");
        c.args(argv);
        apply_creds_to_command(&mut c, &self.creds);
        c.env("RESTIC_PASSWORD", pass);
        c
    }
}

/// The error a non-zero `restic` exit becomes. Pure — extracted so both
/// [`CredentialedRestic::run`] and [`CredentialedRestic::run_stdout`] state it
/// once, and so the tests can pin it without a restic binary.
///
/// INVARIANT: restic's stderr is carried through verbatim. It is the only
/// place the actual cause (wrong key, no such bucket, locked repo) is named,
/// and an operator doing disaster recovery has nothing else to go on.
fn restic_failure_error(argv: &[String], code: Option<i32>, stderr: &[u8]) -> CliError {
    CliError::Other(format!(
        "restic {} failed (exit {code:?}): {}",
        argv.first().map(String::as_str).unwrap_or("?"),
        String::from_utf8_lossy(stderr)
    ))
}

/// The `snapshot_id` of the `summary` line in `restic backup --json` output.
///
/// Pure — extracted from [`CredentialedRestic::run_backup`] and called from
/// both there and the tests. INVARIANT: only the line whose `message_type` is
/// `summary` counts. restic streams `status` lines carrying other ids, and
/// taking the first id in the stream would report a snapshot that is not the
/// one just written.
fn snapshot_id_from_backup_json(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let obj: Value = serde_json::from_str(line.trim()).ok()?;
        if obj.pointer("/message_type").and_then(Value::as_str) == Some("summary") {
            obj.pointer("/snapshot_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        }
    })
}

impl ResticRunner for CredentialedRestic {
    fn run(&self, argv: &[String], pass: &str) -> Result<()> {
        let out = self
            .command(argv, pass)
            .output()
            .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
        if !out.status.success() {
            return Err(restic_failure_error(argv, out.status.code(), &out.stderr));
        }
        Ok(())
    }

    fn run_stdout(&self, argv: &[String], pass: &str) -> Result<String> {
        let out = self
            .command(argv, pass)
            .output()
            .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
        if !out.status.success() {
            return Err(restic_failure_error(argv, out.status.code(), &out.stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn run_backup(&self, argv: &[String], pass: &str) -> Result<Option<String>> {
        // Not exercised by prune/check/unlock, but implemented for real (mirrors
        // SubprocessRestic) so the trait stays honest for any future caller.
        let stdout = self.run_stdout(argv, pass)?;
        Ok(snapshot_id_from_backup_json(&stdout))
    }
}

// ---------------------------------------------------------------------------
// 2b-bis. Offline operation of the operator maintenance verbs
//
// `check`, `prune` and `unlock` run OUTSIDE the cluster with the operator's own
// S3 credentials — the whole point is that they work on the REPOSITORY, not on
// the cluster. Up through v0.2.48 all three reached for the cached kubeconfig
// as their first statement, which made them unusable in the one situation they
// matter most: `apprafter destroy` clears `state.hetzner_cloud`, so verifying an
// off-site backup BEFORE restoring from it failed with "state has no
// hetzner_cloud section; run `apprafter apply` first".
//
// The kubeconfig is only ever needed to read inputs off the PlatformStack CR.
// [`cluster_need`] states — once — exactly which inputs are still unresolved,
// and therefore whether the cluster must be reached at all.
// ---------------------------------------------------------------------------

/// The retention inputs a maintenance verb carries, for the purpose of deciding
/// whether it must reach the cluster.
///
/// `check` and `unlock` have no retention inputs at all
/// ([`RetentionArgs::NotApplicable`]); `prune` carries the three `--keep-*`
/// overrides, any of which, when absent, must be read from
/// `spec.backup.retention`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetentionArgs {
    /// The verb has no retention inputs (`check`, `unlock`).
    NotApplicable,
    /// `backup prune`'s `--keep-daily` / `--keep-weekly` / `--keep-monthly`.
    Prune {
        keep_daily: Option<u32>,
        keep_weekly: Option<u32>,
        keep_monthly: Option<u32>,
    },
}

impl RetentionArgs {
    /// The `--keep-*` flags this invocation did NOT supply, in flag spelling.
    /// Empty for [`RetentionArgs::NotApplicable`] (no retention inputs exist)
    /// and for a fully-specified prune.
    fn missing_flags(self) -> Vec<&'static str> {
        match self {
            RetentionArgs::NotApplicable => Vec::new(),
            RetentionArgs::Prune {
                keep_daily,
                keep_weekly,
                keep_monthly,
            } => [
                ("--keep-daily", keep_daily),
                ("--keep-weekly", keep_weekly),
                ("--keep-monthly", keep_monthly),
            ]
            .into_iter()
            .filter(|(_, v)| v.is_none())
            .map(|(flag, _)| flag)
            .collect(),
        }
    }
}

/// What this invocation still has to read from the PlatformStack CR — and
/// therefore why it needs a reachable cluster. Empty `reasons` ⇒ the verb runs
/// entirely off the cluster.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ClusterNeed {
    /// Human phrases naming the unresolved inputs.
    reasons: Vec<&'static str>,
    /// The flags that would resolve them locally, in `--flag <value>` form.
    flags: Vec<String>,
}

impl ClusterNeed {
    fn is_needed(&self) -> bool {
        !self.reasons.is_empty()
    }

    /// Operator-facing explanation for the moment the cluster is genuinely
    /// needed but unreachable. Names the unresolved inputs AND the exact flags
    /// that would remove the need — for someone doing disaster recovery, whose
    /// cluster is *supposed* to be gone, "run `apprafter apply` first" is not
    /// an answer.
    fn hint(&self, verb: &str) -> String {
        if !self.is_needed() {
            return format!("`apprafter backup {verb}` does not need a cluster.");
        }
        format!(
            "`apprafter backup {verb}` reads {} from the PlatformStack CR, so it needs a \
             reachable cluster. If the cluster no longer exists (disaster recovery — verifying \
             an off-site repo before restoring into a new cluster), pass {} and the command runs \
             entirely off the cluster.",
            self.reasons.join(" and "),
            self.flags.join(" ")
        )
    }
}

/// State the rule ONCE: which inputs of a maintenance verb cannot be resolved
/// from the command line alone. Pure — table-tested without a cluster.
pub(crate) fn cluster_need(repo_override: Option<&str>, retention: RetentionArgs) -> ClusterNeed {
    let mut need = ClusterNeed::default();
    if repo_override.is_none() {
        need.reasons.push("the repository URL (spec.backup.bucket)");
        need.flags.push("--repo <restic-repo>".to_string());
    }
    let missing = retention.missing_flags();
    if !missing.is_empty() {
        need.reasons
            .push("the retention policy (spec.backup.retention)");
        need.flags
            .extend(missing.into_iter().map(|f| format!("{f} <n>")));
    }
    need
}

/// Does this invocation of `backup check` / `prune` / `unlock` need to reach the
/// cluster?
///
/// `--repo` removes the repo lookup; for prune, explicit retention removes the
/// other reason. Anything still unresolved must come from the PlatformStack CR.
pub(crate) fn backup_verb_needs_cluster(
    repo_override: Option<&str>,
    retention: RetentionArgs,
) -> bool {
    cluster_need(repo_override, retention).is_needed()
}

/// Acquire the kubeconfig ONLY on the paths that genuinely need it.
///
/// Returns `Ok(None)` when every CR-backed input was supplied on the command
/// line — the verb then never touches state, kubectl or the cluster. When the
/// cluster IS needed and cannot be resolved, the underlying error is annotated
/// with [`ClusterNeed::hint`] so the operator learns which flags would let the
/// command run offline.
fn kubeconfig_if_cluster_needed(
    verb: &str,
    repo_override: Option<&str>,
    retention: RetentionArgs,
) -> Result<Option<NamedTempFile>> {
    if !backup_verb_needs_cluster(repo_override, retention) {
        return Ok(None);
    }
    match ensure_kubeconfig_tempfile() {
        Ok(kc) => Ok(Some(kc)),
        Err(e) => Err(CliError::Other(format!(
            "{e}\n{}",
            cluster_need(repo_override, retention).hint(verb)
        ))),
    }
}

/// Pick the restic repo from the `--repo` override, else the CR's
/// `spec.backup.bucket`. Pure — the impure caller supplies `spec_backup`
/// (`None` when there was no cluster to read it from, which is indistinguishable
/// from "backup was never configured" as far as this decision goes).
fn repo_from_spec_backup(
    repo_override: Option<&str>,
    spec_backup: Option<&Value>,
) -> Result<String> {
    if let Some(r) = repo_override {
        return Ok(r.to_string());
    }
    spec_backup
        .and_then(|s| s.pointer("/bucket"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::Other(
                "backup not configured — pass --repo or run `apprafter backup enable`".into(),
            )
        })
}

/// Resolve the target restic repo for an operator maintenance verb.
///
/// * `Some(repo)` — use the explicit `--repo` override verbatim; the cluster is
///   never touched (`kubeconfig` may be `None`).
/// * `None` — read `PlatformStack/default.spec.backup.bucket`; error when
///   backup is unconfigured (no `spec.backup.bucket`) or there was no cluster
///   to read, directing the operator to pass `--repo` or run
///   `apprafter backup enable`.
fn resolve_backup_repo(repo_override: Option<&str>, kubeconfig: Option<&Path>) -> Result<String> {
    if repo_override.is_some() {
        return repo_from_spec_backup(repo_override, None);
    }
    let ps = match kubeconfig {
        Some(kc) => kubectl_get_json(
            "platformstack",
            Some(PLATFORMSTACK_NAME),
            Some(PLATFORMSTACK_NAMESPACE),
            kc,
        )?,
        None => None,
    };
    repo_from_spec_backup(None, ps.as_ref().and_then(|p| p.pointer("/spec/backup")))
}

/// Compute the retention policy for a prune from the CR's `spec.backup` plus CLI
/// `--keep-*` overrides.
///
/// Precedence per field: CLI override (`Some`) wins → else the CR's
/// `.retention.{keepDaily,keepWeekly,keepMonthly}` when present → else the
/// [`RetentionPolicy::default`] (7 / 4 / 6). Pure — the impure caller fetches
/// `spec.backup` and reads the CLI flags.
fn retention_from_spec_backup(
    spec_backup: Option<&Value>,
    keep_daily: Option<u32>,
    keep_weekly: Option<u32>,
    keep_monthly: Option<u32>,
) -> RetentionPolicy {
    let default = RetentionPolicy::default();
    let cr = |key: &str| -> Option<u32> {
        spec_backup
            .and_then(|s| s.pointer(&format!("/retention/{key}")))
            .and_then(Value::as_u64)
            .map(|n| n as u32)
    };
    RetentionPolicy {
        keep_daily: keep_daily
            .or_else(|| cr("keepDaily"))
            .unwrap_or(default.keep_daily),
        keep_weekly: keep_weekly
            .or_else(|| cr("keepWeekly"))
            .unwrap_or(default.keep_weekly),
        keep_monthly: keep_monthly
            .or_else(|| cr("keepMonthly"))
            .unwrap_or(default.keep_monthly),
    }
}

/// `apprafter backup prune` — format-aware retention prune of an off-site restic
/// repo, run OUTSIDE the cluster with the operator's full S3 creds.
///
/// Resolves the repo (`--repo` → `spec.backup.bucket`) + creds
/// (`--credential-file` → env), computes the retention policy (CLI overrides →
/// CR → 7/4/6 default), then delegates the run-aware forget-set + prune to the
/// chunk-1 [`run_prune`]. On success it stamps the PlatformStack
/// `apprafter.io/last-prune` annotation with the current RFC3339 time so
/// `apprafter backup status` can surface when the repo was last pruned.
///
/// ## Cluster access is LAZY (v0.2.49)
///
/// The CR is read for TWO things — the repo fallback and the retention defaults
/// — so prune needs a cluster unless `--repo` AND all three `--keep-*` flags
/// are supplied ([`backup_verb_needs_cluster`]). In that fully-specified case
/// the prune runs entirely off the cluster and the `last-prune` stamp (a
/// cluster-side audit annotation) is skipped with a printed note: there is no
/// CR to stamp, and a prune that already succeeded must not be reported as a
/// failure.
pub fn run_backup_prune(
    repo_override: Option<&str>,
    credential_file: Option<&Path>,
    keep_daily: Option<u32>,
    keep_weekly: Option<u32>,
    keep_monthly: Option<u32>,
) -> Result<()> {
    // D11 / 2.22a: the external binaries this command spawns, checked
    // BEFORE any prompt, kubeconfig or provider call. The reported bug
    // was a passphrase typed into a command that could not have worked.
    preflight_tools(&[&RESTIC], "apprafter backup prune")?;

    let retention = RetentionArgs::Prune {
        keep_daily,
        keep_weekly,
        keep_monthly,
    };
    let kc = kubeconfig_if_cluster_needed("prune", repo_override, retention)?;
    let creds = resolve_operator_s3_creds(credential_file, &|k| std::env::var(k).ok())?;
    let pass = creds["RESTIC_PASSWORD"].clone();

    // Fetch the CR once (when we have a cluster at all): repo fallback
    // (spec.backup.bucket) + retention defaults (spec.backup.retention) both
    // read from it.
    let ps = match &kc {
        Some(kc) => kubectl_get_json(
            "platformstack",
            Some(PLATFORMSTACK_NAME),
            Some(PLATFORMSTACK_NAMESPACE),
            kc.path(),
        )?,
        None => None,
    };
    let spec_backup = ps.as_ref().and_then(|p| p.pointer("/spec/backup"));

    let repo = repo_from_spec_backup(repo_override, spec_backup)?;
    let policy = retention_from_spec_backup(spec_backup, keep_daily, keep_weekly, keep_monthly);

    let runner = CredentialedRestic { creds };
    run_prune(&runner, &repo, &pass, &policy)?;

    print!("{}", prune_summary(&repo, &policy));

    // Stamp last-prune so `backup status` can report it. Best-effort ordering:
    // the prune already succeeded, so a merge-patch failure here surfaces as an
    // error (the annotation is the audit trail — we don't want to swallow it).
    // With no cluster there is nothing to stamp; say so rather than failing.
    match &kc {
        Some(kc) => {
            let ts = chrono::Utc::now().to_rfc3339();
            let body = last_prune_patch_body(&ts);
            kubectl_merge_patch(
                "platformstack",
                PLATFORMSTACK_NAME,
                Some(PLATFORMSTACK_NAMESPACE),
                None,
                &body,
                kc.path(),
            )?;
            println!("  last-prune stamped: {ts}");
        }
        None => {
            println!(
                "  last-prune NOT stamped — ran without a cluster (repo and retention were \
                 fully specified on the command line)"
            );
        }
    }
    Ok(())
}

/// What `backup prune` prints after a successful prune. Pure — extracted from
/// [`run_backup_prune`], which prints exactly this.
fn prune_summary(repo: &str, policy: &RetentionPolicy) -> String {
    format!(
        "✓ Pruned {repo}\n  retention: keepDaily={} keepWeekly={} keepMonthly={}\n",
        policy.keep_daily, policy.keep_weekly, policy.keep_monthly
    )
}

/// The merge-patch body stamping `apprafter.io/last-prune`.
///
/// Pure — extracted from [`run_backup_prune`] and called from both there and
/// the tests. INVARIANT: the annotation KEY is `apprafter.io/last-prune`, the
/// exact string `backup status` reads back (as the escaped JSON pointer
/// `apprafter.io~1last-prune`); the two spellings must not drift apart or the
/// stamp is written and never shown.
fn last_prune_patch_body(ts: &str) -> String {
    serde_json::json!({
        "metadata": { "annotations": { "apprafter.io/last-prune": ts } }
    })
    .to_string()
}

/// `apprafter backup check` — verify an off-site restic repo's integrity
/// (`restic check`, opt-in `--read-data` for a deep, full-download verify), run
/// OUTSIDE the cluster with the operator's full S3 creds.
///
/// The cluster is reached ONLY to resolve the repo URL from
/// `spec.backup.bucket`; with `--repo` the command needs no cluster at all —
/// which is the point, since verifying a repo before restoring from it happens
/// when the cluster is gone.
pub fn run_backup_check(
    repo_override: Option<&str>,
    credential_file: Option<&Path>,
    read_data: bool,
) -> Result<()> {
    // D11 / 2.22a: the external binaries this command spawns, checked
    // BEFORE any prompt, kubeconfig or provider call. The reported bug
    // was a passphrase typed into a command that could not have worked.
    preflight_tools(&[&RESTIC], "apprafter backup check")?;

    let kc = kubeconfig_if_cluster_needed("check", repo_override, RetentionArgs::NotApplicable)?;
    let creds = resolve_operator_s3_creds(credential_file, &|k| std::env::var(k).ok())?;
    let pass = creds["RESTIC_PASSWORD"].clone();
    let repo = resolve_backup_repo(repo_override, kc.as_ref().map(|f| f.path()))?;

    let runner = CredentialedRestic { creds };
    runner.run(&restic_check_argv(&repo, read_data), &pass)?;

    if read_data {
        println!("✓ Repository check passed (deep --read-data verify).");
    } else {
        println!("✓ Repository check passed.");
    }
    Ok(())
}

/// `apprafter backup unlock` — remove STALE locks from an off-site restic repo
/// (`restic unlock`; never touches live locks held by a concurrent run), run
/// OUTSIDE the cluster with the operator's full S3 creds.
///
/// Like [`run_backup_check`], the cluster is reached ONLY to resolve the repo
/// URL from `spec.backup.bucket`; with `--repo` no cluster is required.
pub fn run_backup_unlock(
    repo_override: Option<&str>,
    credential_file: Option<&Path>,
) -> Result<()> {
    // D11 / 2.22a: the external binaries this command spawns, checked
    // BEFORE any prompt, kubeconfig or provider call. The reported bug
    // was a passphrase typed into a command that could not have worked.
    preflight_tools(&[&RESTIC], "apprafter backup unlock")?;

    let kc = kubeconfig_if_cluster_needed("unlock", repo_override, RetentionArgs::NotApplicable)?;
    let creds = resolve_operator_s3_creds(credential_file, &|k| std::env::var(k).ok())?;
    let pass = creds["RESTIC_PASSWORD"].clone();
    let repo = resolve_backup_repo(repo_override, kc.as_ref().map(|f| f.path()))?;

    let runner = CredentialedRestic { creds };
    runner.run(&restic_unlock_argv(&repo), &pass)?;

    println!("✓ Stale locks removed.");
    Ok(())
}

// ---------------------------------------------------------------------------
// 2c. `apprafter backup enable` / `disable` — preflight + spec.backup patch
// ---------------------------------------------------------------------------

/// Minimum restic version the off-site backup path relies on (compression +
/// `s3:` repo behaviour). Anything confidently older is rejected up front.
const MIN_RESTIC_MAJOR: u64 = 0;
const MIN_RESTIC_MINOR: u64 = 14;

/// Parse the `x.y.z` semver out of a `restic version` stdout line
/// (e.g. `restic 0.16.4 compiled with go1.21.6 on linux/amd64`). Returns
/// `(major, minor, patch)` or `None` when no dotted-triple token is found.
/// Pure — unit-testable without a restic binary.
fn parse_restic_version(stdout: &str) -> Option<(u64, u64, u64)> {
    for tok in stdout.split_whitespace() {
        // Strip a leading `v` if present (restic prints bare, but be lenient).
        let t = tok.strip_prefix('v').unwrap_or(tok);
        let mut parts = t.split('.');
        let (Some(a), Some(b), Some(c)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        // Only accept when the third segment starts with digits (guards against
        // matching e.g. `go1.21.6` — that would parse, so we additionally
        // require the token to not be prefixed by non-version text).
        if let (Ok(major), Ok(minor)) = (a.parse::<u64>(), b.parse::<u64>()) {
            // `c` may carry a trailing suffix; take its leading digits.
            let patch_digits: String = c.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if let Ok(patch) = patch_digits.parse::<u64>() {
                return Some((major, minor, patch));
            }
        }
    }
    None
}

/// Is `(major, minor, _)` confidently BELOW the required `MIN_RESTIC_*`?
fn restic_version_too_old(v: (u64, u64, u64)) -> bool {
    let (major, minor, _) = v;
    (major, minor) < (MIN_RESTIC_MAJOR, MIN_RESTIC_MINOR)
}

/// Default name used when sealing the backup credential Secret and no
/// `--credential` override is given.
const DEFAULT_BACKUP_CREDENTIAL_NAME: &str = "apprafter-backup-s3";

/// `apprafter backup enable` — validate the repo + credential material +
/// operator intent, then merge-patch `PlatformStack.spec.backup` to turn on
/// scheduled off-site backup.
///
/// ## Two mutually-exclusive credential input paths (one is REQUIRED)
///
/// ### Path A — `--credential-file <dotenv>` given (fresh setup)
/// 1. Parse + normalise the dotenv → canonical `S3_*` map.
/// 2. Validate required keys.
/// 3. `restic version` preflight.
/// 4. Probe repo reachability (`restic cat config` → `restic init`).
/// 5. **Auto-seal** the creds as a `SealedSecret` in `apprafter-system` with
///    the canonical `S3_*` keys (name = `--credential` when given, else
///    `apprafter-backup-s3`).
/// 6. DR confirmation.
/// 7. Merge-patch `spec.backup` (credentialRef → sealed Secret name).
///
/// ### Path B — no `--credential-file`, `--credential <name>` given (secret already exists)
/// 1. Read the live Secret's `.data` from the cluster (base64-decoded).
/// 2. Normalise (accept `S3_*` or `AWS_*` aliases).
/// 3. Validate required keys.
/// 4. `restic version` preflight.
/// 5. Probe repo reachability using the live creds.
/// 6. DR confirmation.
/// 7. Merge-patch `spec.backup` (credentialRef → the named Secret).
///
/// ### Neither path
/// If no `--credential-file` AND no `--credential` → clear error with key
/// enumeration.
///
/// The credential name stored in `spec.backup.credentialRef.name` is always
/// the name of the in-cluster Secret (sealed or plain) the operator's CronJob
/// will mount to get its S3 credentials.
pub fn run_backup_enable(
    mut opts: EnableOpts,
    endpoint: Option<&str>,
    prefix: Option<&str>,
    credential_file: Option<&Path>,
    i_have_saved: bool,
) -> Result<()> {
    // 0. Build the canonical restic repo URL from bucket + optional endpoint/prefix.
    opts.bucket = construct_repo_url(&opts.bucket, endpoint, prefix)?;

    // 1. Validate enum-valued options before touching the cluster.
    validate_enable_enums(&opts)?;

    // 1b. Resolve the schedule and the zone (2.22g / D2). Before the
    //     kubeconfig, before any prompt, before anything billable — a bad
    //     `--at` should cost nothing, and an unresolvable zone must fail here
    //     rather than after the credentials have been sealed.
    let resolved = resolve_schedule(&opts)?;

    // 2. Resolve creds via one of the two paths.
    //    `creds` is always the canonical S3_* map.
    //    `seal_from_file` tracks whether we must seal a new Secret.
    let kc = ensure_kubeconfig_tempfile()?;
    let (creds, seal_from_file, effective_credential_name) = if let Some(path) = credential_file {
        // PATH A: parse the dotenv file, normalise to canonical S3_*.
        let contents = std::fs::read_to_string(path).map_err(|e| {
            CliError::Other(format!("read credential file {}: {e}", path.display()))
        })?;
        let raw = parse_credential_file(&contents);
        let canonical = normalize_s3_creds(raw);
        validate_required_cred_keys(&canonical)?;

        // Credential name: explicit --credential, else the platform default.
        (canonical, true, effective_credential_name(&opts.credential))
    } else if !opts.credential.is_empty() {
        // PATH B: read the live Secret from the cluster.
        let secret_data = read_secret_data(&opts.credential, PLATFORMSTACK_NAMESPACE, kc.path())?;
        let Some((raw_bytes, _)) = secret_data else {
            return Err(CliError::Other(format!(
                "credential Secret '{}' not found in {PLATFORMSTACK_NAMESPACE} — \
                 either pass --credential-file <dotenv> to create it automatically, or \
                 seal the Secret first.\n\n{CRED_KEYS_HELP}",
                opts.credential
            )));
        };
        // Base64 has already been decoded by read_secret_data; values are bytes.
        let canonical = normalize_s3_creds(secret_bytes_to_strings(raw_bytes));
        validate_required_cred_keys(&canonical)?;
        let name = opts.credential.clone();
        (canonical, false, name)
    } else {
        // NEITHER: no file and no credential name → explicit error.
        return Err(CliError::Other(format!(
            "no credential source — provide one of:\n  \
             --credential-file <dotenv>  (creates + seals the Secret automatically)\n  \
             --credential <name>         (names an existing Secret in {PLATFORMSTACK_NAMESPACE})\n\n\
             {CRED_KEYS_HELP}"
        )));
    };

    // Update opts.credential to the effective name (may be the default).
    opts.credential = effective_credential_name.clone();

    // 3. restic version preflight.
    preflight_restic_version()?;

    // 4. Repo reachability probe (uses translated AWS_* env for restic subprocess).
    preflight_repo_reachable(&opts.bucket, &creds)?;

    // 5. If path A, auto-seal the creds into apprafter-system.
    if seal_from_file {
        let pub_key = fetch_controller_public_key(&KubectlCli, kc.path())?;
        // Seal the canonical S3_* keys. The in-cluster backup CronJob maps
        // S3_*→AWS_* (via secretKeyRef) before invoking restic — see
        // platform-stack/cue/render_tool.cue.
        let secret_data: BTreeMap<String, Vec<u8>> = creds
            .iter()
            .map(|(k, v)| (k.clone(), v.as_bytes().to_vec()))
            .collect();
        let cr = build_sealed_secret(
            &pub_key,
            PLATFORMSTACK_NAMESPACE,
            &effective_credential_name,
            &secret_data,
            "Opaque",
        )?;
        apply_sealed_secret_manifest(&cr, kc.path())?;
        println!(
            "  ✓ Sealed credential Secret '{effective_credential_name}' in \
             {PLATFORMSTACK_NAMESPACE}."
        );
    }

    // 6. DR credential confirmation.
    if !i_have_saved {
        if std::io::stdin().is_terminal() {
            let confirmed = inquire::Confirm::new(
                "Have you saved the restic passphrase AND S3 credentials somewhere OUTSIDE \
                 this cluster? Without them, backups are UNRECOVERABLE.",
            )
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
            if !confirmed {
                println!(
                    "Aborted — no changes made. Save the restic passphrase + S3 credentials \
                     outside the cluster, then re-run."
                );
                return Ok(());
            }
        } else {
            return Err(CliError::Other(
                "non-interactive: re-run with --i-have-saved-credentials once you've saved the \
                 passphrase + S3 creds outside the cluster"
                    .into(),
            ));
        }
    }

    // 7. Merge-patch spec.backup (path-scoped; spec.backup has no required
    //    siblings, so a JSON merge-patch is correct — no SSA field-manager).
    let patch = backup_enable_patch(&opts, &resolved);
    let body = serde_json::to_string(&patch)
        .map_err(|e| CliError::Other(format!("serialize spec.backup patch: {e}")))?;
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;

    // 7b. READ IT BACK. `spec.backup` is fully structural in the CRD — the
    //     only preserve-unknown-fields markers are on `spec.overrides.*.values`,
    //     `spec.values` and `status` — so an operator whose CRD predates
    //     `timeZone` gets HTTP 200, every field it knows stored, and this one
    //     silently DROPPED. The command would half-succeed: backups genuinely
    //     enabled, running in the wrong zone, with this CLI reporting the zone
    //     it thought it set. `kubectl` writes the pruning warning to stderr,
    //     which `kubectl_merge_patch` reads only on failure — so the write
    //     looks clean either way and the read-back is the only sound check.
    if !resolved.time_zone.is_empty() {
        let stored = kubectl_get_json(
            "platformstack",
            Some(PLATFORMSTACK_NAME),
            Some(PLATFORMSTACK_NAMESPACE),
            kc.path(),
        )
        .ok()
        .flatten();
        check_time_zone_readback(
            &resolved.time_zone,
            stored_time_zone(stored.as_ref()).as_deref(),
        )?;
    }

    // 8. Success + GitOps advisory.
    print!(
        "{}",
        enable_success_report(&opts.bucket, &opts.credential, &resolved)
    );
    Ok(())
}

/// Refuse the two enum-valued `enable` flags before anything is touched.
///
/// Pure — extracted from [`run_backup_enable`] and called from both there and
/// the tests. It runs BEFORE the kubeconfig, the credential seal and the DR
/// prompt precisely so a typo costs nothing.
fn validate_enable_enums(o: &EnableOpts) -> Result<()> {
    if let Some(enforce) = &o.enforce {
        if enforce != "operator" && enforce != "cluster" {
            return Err(CliError::Other(format!(
                "invalid --enforce '{enforce}': expected 'operator' or 'cluster'"
            )));
        }
    }
    if let Some(mode) = &o.staging_mode {
        if mode != "monolithic" && mode != "sequential" {
            return Err(CliError::Other(format!(
                "invalid --staging-mode '{mode}': expected 'monolithic' or 'sequential'"
            )));
        }
    }
    Ok(())
}

/// The Secret name the credential is sealed under: `--credential` when given,
/// else the platform default. Pure — extracted from [`run_backup_enable`].
fn effective_credential_name(explicit: &str) -> String {
    if explicit.is_empty() {
        DEFAULT_BACKUP_CREDENTIAL_NAME.to_string()
    } else {
        explicit.to_string()
    }
}

/// Decode a credential Secret's raw values into strings.
///
/// Pure — extracted from [`run_backup_enable`]'s path B and called from both
/// there and the tests.
///
/// INVARIANT: a TRAILING NEWLINE is stripped. `kubectl create secret generic
/// --from-file` stores the file verbatim, newline included, and an
/// `AWS_SECRET_ACCESS_KEY` with a trailing `\n` fails S3 signing with an
/// authentication error that names nothing. Non-UTF-8 values are dropped
/// rather than lossily mangled — a mangled key would also fail to sign, but
/// silently and with a plausible-looking value.
fn secret_bytes_to_strings(raw: BTreeMap<String, Vec<u8>>) -> BTreeMap<String, String> {
    raw.into_iter()
        .filter_map(|(k, v)| {
            String::from_utf8(v)
                .ok()
                .map(|s| (k, s.trim_end_matches('\n').to_string()))
        })
        .collect()
}

/// `spec.backup.timeZone` as the cluster stores it. Pure — extracted from
/// [`run_backup_enable`]'s read-back guard.
fn stored_time_zone(ps: Option<&Value>) -> Option<String> {
    ps.and_then(|v| v.pointer("/spec/backup/timeZone"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// THE READ-BACK GUARD (2.22g). Compare the zone we asked the cluster to store
/// against the zone it reads back, and refuse when they differ.
///
/// Pure — extracted from [`run_backup_enable`] and called from both there and
/// the tests.
///
/// # Why this exists
///
/// `spec.backup` is fully structural in the CRD, so an apiserver whose CRD
/// predates `timeZone` answers the merge-patch with HTTP 200, stores every
/// field it recognises, and silently PRUNES this one. kubectl writes its
/// pruning warning to stderr, which the merge-patch helper reads only on
/// failure — so the write looks clean from every angle. The command would
/// half-succeed: backups genuinely enabled, running in the wrong zone, with
/// this CLI reporting the zone it thought it set. Reading the field back is
/// the only sound check, and a mismatch must be an ERROR — a warning here is
/// a wrong backup window nobody notices until they need the backup.
fn check_time_zone_readback(expected: &str, stored: Option<&str>) -> Result<()> {
    if expected.is_empty() || stored == Some(expected) {
        return Ok(());
    }
    Err(CliError::Other(format!(
        "the cluster did not store the timezone '{expected}' (it reads back as {:?}).\n\n                   This operator's PlatformStack CRD predates the `spec.backup.timeZone`                  field, so the apiserver accepted the write and discarded it — the backup                  would run in the cluster's own zone with no sign of it.\n\n                   Upgrade the platform, then re-run this command.",
        stored.map(str::to_string)
    )))
}

/// What a successful `backup enable` prints. Pure — extracted from
/// [`run_backup_enable`], which prints exactly this.
fn enable_success_report(bucket: &str, credential: &str, s: &ResolvedSchedule) -> String {
    format!(
        "✓ Scheduled off-site backup enabled → {bucket} (credential Secret '{credential}').\n  schedule: {} {}\n{BACKUP_GITOPS_ADVISORY}\n",
        describe_schedule(s),
        s.time_zone
    )
}

/// Apply a `SealedSecret` manifest via `kubectl apply -f <tempfile>`.
/// Reuses the same approach as `commands::secret::apply_manifest` but is
/// inlined here to avoid a cross-module private function reference.
fn apply_sealed_secret_manifest(manifest: &Value, kubeconfig_path: &Path) -> Result<()> {
    use std::io::Write as _;
    let mut file = tempfile::Builder::new()
        .prefix("apprafter-sealed-")
        .suffix(".json")
        .tempfile()
        .map_err(|e| CliError::Other(format!("create SealedSecret tempfile: {e}")))?;
    let body = serde_json::to_vec_pretty(manifest)
        .map_err(|e| CliError::Other(format!("serialise SealedSecret: {e}")))?;
    file.write_all(&body)
        .map_err(|e| CliError::Other(format!("write SealedSecret tempfile: {e}")))?;
    file.flush()
        .map_err(|e| CliError::Other(format!("flush SealedSecret tempfile: {e}")))?;

    let out = std::process::Command::new("kubectl")
        .arg("apply")
        .arg("-f")
        .arg(file.path())
        .env("KUBECONFIG", kubeconfig_path)
        .output()
        .map_err(|e| CliError::Other(format!("spawn kubectl apply (SealedSecret): {e}")))?;
    if !out.status.success() {
        return Err(CliError::Other(format!(
            "kubectl apply SealedSecret failed (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// `apprafter backup disable` — merge-patch `spec.backup.enabled=false`,
/// retaining every other configured field for a later re-enable.
pub fn run_backup_disable() -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let body = serde_json::to_string(&backup_disable_patch())
        .map_err(|e| CliError::Other(format!("serialize spec.backup patch: {e}")))?;
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc.path(),
    )?;
    println!(
        "✓ Scheduled backup disabled (config retained; re-enable with `apprafter backup enable`)."
    );
    Ok(())
}

/// One-line advisory printed after a successful `spec.backup` merge-patch, in
/// the same spirit as `platform env set` / `platform egress set`: a live
/// merge-patch is not durable if the field is git-managed via Argo CD.
const BACKUP_GITOPS_ADVISORY: &str =
    "If PlatformStack.spec.backup is git-managed via Argo CD, the next sync will overwrite this \
     — set it in your infra repo for a durable change.";

/// Run `restic version`, parse the semver, and error when it is confidently
/// older than the required minimum. `restic` not on PATH → error. An
/// unparseable version → warn to stderr and continue (don't hard-fail purely on
/// a parse miss — only on a confidently-lower version).
fn preflight_restic_version() -> Result<()> {
    let out = Command::new("restic")
        .arg("version")
        .output()
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                CliError::Other("restic not on PATH — install restic >= 0.14 first".into())
            } else {
                CliError::Other(format!("spawn restic version: {e}"))
            }
        })?;
    if !out.status.success() {
        // `restic version` failing is unusual but shouldn't itself block enable
        // — warn and continue; the repo probe below is the real gate.
        eprintln!(
            "warning: `restic version` exited with {} — continuing (repo probe still validates)",
            out.status
        );
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    restic_version_gate(&stdout)
}

/// Decide the restic version gate from `restic version` stdout.
///
/// Extracted from [`preflight_restic_version`] (which is this function plus
/// the subprocess spawn) and called from both there and the tests.
///
/// INVARIANT: an UNPARSEABLE version warns and passes; only a confidently
/// lower one fails. A future restic that reworks its version line must not
/// make `backup enable` impossible on a perfectly good binary.
fn restic_version_gate(stdout: &str) -> Result<()> {
    match parse_restic_version(stdout) {
        Some(v) if restic_version_too_old(v) => Err(CliError::Other(format!(
            "restic >= {MIN_RESTIC_MAJOR}.{MIN_RESTIC_MINOR} required, found {}.{}.{}",
            v.0, v.1, v.2
        ))),
        Some(_) => Ok(()),
        None => {
            eprintln!(
                "warning: could not parse restic version from `{}` — continuing",
                stdout.trim()
            );
            Ok(())
        }
    }
}

/// Probe repo reachability: `restic cat config` (repo already initialised) or,
/// failing that, `restic init`. If both fail the repo is unreachable or the
/// creds are wrong → error carrying restic's stderr. Creds are injected via
/// [`apply_creds_to_command`] (AWS_* + RESTIC_PASSWORD), never persisted.
fn preflight_repo_reachable(bucket: &str, creds: &BTreeMap<String, String>) -> Result<()> {
    let mut cat = Command::new("restic");
    cat.args(["cat", "config", "-r", bucket]);
    apply_creds_to_command(&mut cat, creds);
    let cat_out = cat
        .output()
        .map_err(|e| CliError::Other(format!("spawn restic cat config: {e}")))?;
    if cat_out.status.success() {
        return Ok(());
    }

    // Not initialised (or unreachable) — try to init it.
    let mut init = Command::new("restic");
    init.args(["init", "-r", bucket]);
    apply_creds_to_command(&mut init, creds);
    let init_out = init
        .output()
        .map_err(|e| CliError::Other(format!("spawn restic init: {e}")))?;
    if init_out.status.success() {
        return Ok(());
    }

    Err(repo_unreachable_error(
        bucket,
        &cat_out.stderr,
        &init_out.stderr,
    ))
}

/// The error raised when neither `restic cat config` nor `restic init` could
/// reach the repo. Pure — extracted from [`preflight_repo_reachable`] and
/// called from both there and the tests.
///
/// INVARIANT: BOTH stderrs are carried. They usually say different things —
/// `cat config` reports "repository does not exist", `init` reports the real
/// obstacle (bad key, no such bucket, permission denied) — and dropping either
/// leaves the operator guessing which of the two problems they have.
fn repo_unreachable_error(bucket: &str, cat_stderr: &[u8], init_stderr: &[u8]) -> CliError {
    CliError::Other(format!(
        "backup repo '{bucket}' unreachable / bad credentials — neither `restic cat config` nor \
         `restic init` succeeded.\n  cat config stderr: {}\n  init stderr: {}",
        String::from_utf8_lossy(cat_stderr).trim(),
        String::from_utf8_lossy(init_stderr).trim(),
    ))
}

// ---------------------------------------------------------------------------
// 3a. `apprafter backup status` — pure formatter
// ---------------------------------------------------------------------------

/// Extract `.metadata.name` from a Job JSON object (empty string when absent).
fn job_metadata_name(j: &serde_json::Value) -> &str {
    j.pointer("/metadata/name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Extract `.status.startTime` from a Job JSON object (empty string when absent).
fn job_start_time(j: &serde_json::Value) -> &str {
    j.pointer("/status/startTime")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// Pick the most-recent Job from a slice by `.status.startTime` (lexicographic;
/// RFC3339 timestamps sort correctly as strings). Returns `None` when the slice
/// is empty.
fn most_recent_job<'a>(jobs: &[&'a serde_json::Value]) -> Option<&'a serde_json::Value> {
    jobs.iter().copied().max_by_key(|j| job_start_time(j))
}

/// Summarise a Job's terminal state from `.status.succeeded/.failed/.active`.
fn job_outcome(j: &serde_json::Value) -> &'static str {
    let succeeded = j
        .pointer("/status/succeeded")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let active = j
        .pointer("/status/active")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let failed = j
        .pointer("/status/failed")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if succeeded > 0 {
        "Succeeded"
    } else if active > 0 {
        "Running"
    } else if failed > 0 {
        "Failed"
    } else {
        "Unknown"
    }
}

/// Render a human-readable status block for `apprafter backup status`.
///
/// All four inputs are optional / may be empty so the function works honestly
/// for every cluster state (backup never configured, no Jobs yet, CM absent).
///
/// # ConfigMap data keys (from `apprafter-backup/src/status.rs`)
/// * `lastRunFormat`  — staging mode of the last run (always written).
/// * `lastSuccess`    — RFC3339 timestamp of the last successful run.
/// * `lastFailure`    — RFC3339 timestamp of the last failed run.
/// * `lastError`      — error message from the last failed run.
///
/// # CronJob names (from `platform-stack/cue/render_tool.cue _backupTemplate`)
/// * `apprafter-backup`       — the scheduled backup CronJob.
/// * `apprafter-backup-check` — the weekly check CronJob.
///
/// Jobs are selected by their `.metadata.name` prefix `apprafter-backup` (both
/// CronJob-spawned Jobs share that prefix). For each of the two flavours (with
/// and without `-check`) the most-recent Job (by `.status.startTime`) is shown.
pub(crate) fn format_backup_status(
    spec_backup: Option<&serde_json::Value>,
    jobs: &[serde_json::Value],
    status_cm: Option<&serde_json::Value>,
    last_prune: Option<&str>,
) -> String {
    let mut out = String::new();

    // --- Config block ---
    let enabled = spec_backup
        .and_then(|s| s.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if !enabled {
        out.push_str("Backup: DISABLED — enable with `apprafter backup enable ...`\n");
        if let Some(spec) = spec_backup {
            if let Some(bucket) = spec.get("bucket").and_then(serde_json::Value::as_str) {
                out.push_str(&format!("  bucket:   {bucket} (config retained)\n"));
            }
        }
        return out;
    }

    let spec = spec_backup.unwrap(); // enabled=true implies Some

    out.push_str("Backup: ENABLED\n");
    if let Some(v) = spec.get("bucket").and_then(serde_json::Value::as_str) {
        out.push_str(&format!("  bucket:        {v}\n"));
    }
    // 2.22g / D2: print the schedule back AS A TIME, in the zone it was given.
    // A cron expression is not what the operator said, and a time without its
    // zone is the trap this whole change closes — so if the zone is missing,
    // say that rather than printing a bare time that reads as local.
    let zone = spec.get("timeZone").and_then(serde_json::Value::as_str);
    if let Some(v) = spec.get("schedule").and_then(serde_json::Value::as_str) {
        out.push_str(&format!(
            "  schedule:      {}\n",
            describe_cron_daily(v, zone)
        ));
    }
    if let Some(v) = spec.get("stagingMode").and_then(serde_json::Value::as_str) {
        out.push_str(&format!("  stagingMode:   {v}\n"));
    }
    match spec
        .get("checkSchedule")
        .and_then(serde_json::Value::as_str)
    {
        // Empty is not missing: it is `--check off`, and the chart omits the
        // whole CronJob for it. Saying "off" is the difference between an
        // operator believing the check runs and knowing it does not.
        Some("") => out.push_str("  check:         off\n"),
        Some(v) => out.push_str(&format!(
            "  check:         {}\n",
            describe_cron_weekly(v, zone)
        )),
        None => {}
    }
    // Retention sub-block.
    if let Some(ret) = spec.get("retention") {
        out.push_str("  retention:\n");
        for key in ["keepDaily", "keepWeekly", "keepMonthly"] {
            if let Some(n) = ret.get(key) {
                out.push_str(&format!("    {key}: {n}\n"));
            }
        }
        if let Some(e) = ret.get("enforce").and_then(serde_json::Value::as_str) {
            out.push_str(&format!("    enforce: {e}\n"));
        }
    }

    // --- Job outcomes ---
    // Partition into backup Jobs (name prefix `apprafter-backup` but NOT
    // `apprafter-backup-check`) and check Jobs (prefix `apprafter-backup-check`).
    let backup_jobs: Vec<&serde_json::Value> = jobs
        .iter()
        .filter(|j| {
            let n = job_metadata_name(j);
            n.starts_with("apprafter-backup") && !n.contains("check")
        })
        .collect();
    let check_jobs: Vec<&serde_json::Value> = jobs
        .iter()
        .filter(|j| job_metadata_name(j).contains("apprafter-backup-check"))
        .collect();

    out.push_str("\nJobs:\n");
    match most_recent_job(&backup_jobs) {
        Some(j) => out.push_str(&format!(
            "  Last backup Job: {} — {}\n",
            job_metadata_name(j),
            job_outcome(j)
        )),
        None => out.push_str("  Last backup Job: none\n"),
    }
    match most_recent_job(&check_jobs) {
        Some(j) => out.push_str(&format!(
            "  Last check Job:  {} — {}\n",
            job_metadata_name(j),
            job_outcome(j)
        )),
        None => out.push_str("  Last check Job:  none\n"),
    }

    // --- Runner status CM ---
    out.push_str("\nRunner status:\n");
    if let Some(cm) = status_cm {
        // The CM may be passed as the full CM object (with a .data map) or as
        // just the .data section. Check both to stay robust to caller choice.
        let data = cm.get("data").filter(|d| d.is_object()).unwrap_or(cm);
        let get_str = |key: &str| -> &str {
            data.get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        };
        let last_success = get_str("lastSuccess");
        let last_failure = get_str("lastFailure");
        let last_error = get_str("lastError");
        let last_run_format = get_str("lastRunFormat");

        if !last_success.is_empty() {
            out.push_str(&format!("  lastSuccess:    {last_success}\n"));
        } else {
            out.push_str("  lastSuccess:    never\n");
        }
        if !last_failure.is_empty() {
            out.push_str(&format!("  lastFailure:    {last_failure}\n"));
        }
        if !last_error.is_empty() {
            out.push_str(&format!("  lastError:      {last_error}\n"));
        }
        if !last_run_format.is_empty() {
            out.push_str(&format!("  lastRunFormat:  {last_run_format}\n"));
        }
    } else {
        out.push_str("  (no status ConfigMap yet — backup may not have run)\n");
    }

    // --- Last prune ---
    out.push_str(&format!(
        "\nLast prune: {}\n",
        last_prune.unwrap_or("never")
    ));

    out
}

/// Read the `apprafter.io/last-prune` stamp off the PlatformStack.
///
/// Pure — extracted from [`run_backup_status`] and called from both there and
/// the tests. INVARIANT: the JSON pointer escapes the `/` in the annotation
/// key as `~1`. Written unescaped it silently resolves to nothing, and every
/// pruned cluster reports "Last prune: never".
fn last_prune_annotation(ps: Option<&Value>) -> Option<String> {
    ps.and_then(|p| p.pointer("/metadata/annotations/apprafter.io~1last-prune"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The Jobs `backup status` reports on: `.items[]` of a Jobs listing, narrowed
/// to the `apprafter-backup` name prefix.
///
/// Pure — extracted from [`run_backup_status`] and called from both there and
/// the tests. INVARIANT: the prefix filter is applied here, so an unrelated
/// Job in `apprafter-system` never gets reported as somebody's backup.
fn backup_jobs_of(jobs_list: Option<&Value>) -> Vec<Value> {
    jobs_list
        .map(items_of)
        .unwrap_or_default()
        .into_iter()
        .filter(|j| job_metadata_name(j).starts_with("apprafter-backup"))
        .collect()
}

/// `apprafter backup status` — show the operator's backup configuration, last
/// Job outcomes, runner self-reported status, and last prune time.
pub fn run_backup_status() -> Result<()> {
    // D11 / 2.22a: the external binaries this command spawns, checked
    // BEFORE any prompt, kubeconfig or provider call. The reported bug
    // was a passphrase typed into a command that could not have worked.
    preflight_tools(&[&KUBECTL], "apprafter backup status")?;

    let kc = ensure_kubeconfig_tempfile()?;

    // 1. Fetch PlatformStack to get spec.backup + last-prune annotation.
    let ps = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;
    let spec_backup = ps.as_ref().and_then(|p| p.pointer("/spec/backup")).cloned();
    let last_prune = last_prune_annotation(ps.as_ref());

    // 2. List Jobs in apprafter-system and filter by name prefix.
    let jobs_list = kubectl_get_json("jobs", None, Some(PLATFORMSTACK_NAMESPACE), kc.path())?;
    let jobs = backup_jobs_of(jobs_list.as_ref());

    // 3. Fetch the runner status ConfigMap.
    let status_cm = kubectl_get_json(
        "configmap",
        Some("apprafter-backup-status"),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;

    println!(
        "{}",
        format_backup_status(
            spec_backup.as_ref(),
            &jobs,
            status_cm.as_ref(),
            last_prune.as_deref(),
        )
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (pure helpers — the tested core)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ------------------------------------------------------------------
    // Schedule surface (2.22g / D2)
    // ------------------------------------------------------------------

    #[test]
    fn the_bare_default_reproduces_the_historical_window_byte_for_byte() {
        // THE regression guard for this whole change. An operator who upgrades
        // and re-runs `enable` with no schedule flags must get the same two
        // crons they had before — otherwise the fix silently moves everybody's
        // backup, which is a worse defect than the one it closes.
        let (h, m) = parse_at("03:00").unwrap();
        assert_eq!(compose_daily(h, m), DEFAULT_BACKUP_SCHEDULE);
        let (ch, cm) = derive_check_time(h, m);
        assert_eq!(compose_weekly_sunday(ch, cm), DEFAULT_CHECK_SCHEDULE);
    }

    #[test]
    fn parse_at_takes_a_24_hour_time_and_normalises_a_single_digit_hour() {
        assert_eq!(parse_at("03:00").unwrap(), (3, 0));
        assert_eq!(parse_at("3:00").unwrap(), (3, 0));
        assert_eq!(parse_at("00:00").unwrap(), (0, 0));
        assert_eq!(parse_at("23:59").unwrap(), (23, 59));
        assert_eq!(compose_daily(22, 30), "30 22 * * *");
    }

    #[test]
    fn parse_at_refuses_every_other_shape_and_says_what_it_wanted() {
        // One grammar, not two. Accepting `3pm` or seconds would mean the
        // help text has to describe both, and the operator has to guess.
        for bad in [
            "3", "3pm", "03:00:00", "24:00", "03:5", "", ":00", "03:", "0300", "-1:00",
        ] {
            let err = parse_at(bad).unwrap_err();
            assert!(
                format!("{err}").contains("HH:MM"),
                "{bad:?} produced a message that does not show the shape: {err}"
            );
        }
    }

    #[test]
    fn the_check_never_starts_in_the_same_minute_as_a_backup() {
        // That is the whole claim of the offset — not that the check follows
        // the backup. `--at 23:00` puts the check at 02:00, EARLIER in that
        // Sunday, and that is fine.
        for h in 0..24 {
            let (ch, _) = derive_check_time(h, 0);
            assert_ne!(ch, h, "check hour collides with the backup hour at {h}");
        }
        assert_eq!(derive_check_time(23, 0), (2, 0));
        assert_eq!(derive_check_time(22, 30), (1, 30));
    }

    #[test]
    fn a_posix_tz_spec_never_reaches_the_cluster() {
        // `spec.timeZone` takes an IANA name. A POSIX TZ value means nothing
        // there, and an operator who has `TZ=CET-1CEST,M3.5.0` exported must
        // not have it written into their CronJob.
        for posix in [
            "CET-1CEST,M3.5.0,M10.5.0/3",
            "EST5EDT",
            ":/etc/localtime",
            "GMT+5",
        ] {
            assert!(
                validate_zone_shape(posix).is_err(),
                "{posix:?} was accepted as an IANA zone"
            );
        }
        for good in [
            "Europe/Berlin",
            "America/Argentina/Buenos_Aires",
            "UTC",
            "Etc/GMT+5",
        ] {
            assert!(validate_zone_shape(good).is_ok(), "{good:?} was rejected");
        }
    }

    #[test]
    fn the_zone_precedence_is_flag_then_env_then_os() {
        assert_eq!(
            resolve_time_zone(Some("UTC"), Some("Europe/Berlin"), Some("Asia/Tokyo")).unwrap(),
            ("UTC".into(), ZoneSource::Flag)
        );
        assert_eq!(
            resolve_time_zone(None, Some("Europe/Berlin"), Some("Asia/Tokyo")).unwrap(),
            ("Europe/Berlin".into(), ZoneSource::TzEnv)
        );
        assert_eq!(
            resolve_time_zone(None, None, Some("Asia/Tokyo")).unwrap(),
            ("Asia/Tokyo".into(), ZoneSource::OperatingSystem)
        );
        // A POSIX $TZ is not an answer — fall through rather than write it.
        assert_eq!(
            resolve_time_zone(None, Some("EST5EDT"), Some("Asia/Tokyo")).unwrap(),
            ("Asia/Tokyo".into(), ZoneSource::OperatingSystem)
        );
    }

    #[test]
    fn an_unknown_zone_refuses_rather_than_assuming_utc() {
        // UTC is a reasonable thing to ASK for and a poor thing to ASSUME.
        // Guessing it produces a backup at the wrong hour with nothing saying
        // so; refusing costs one flag.
        let err = resolve_time_zone(None, None, None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not a time"), "{msg}");
        assert!(msg.contains("--timezone Europe/Berlin"), "{msg}");
        assert!(msg.contains("--timezone UTC"), "{msg}");
    }

    #[test]
    fn an_explicitly_bad_zone_flag_is_an_error_not_a_fallback() {
        // The flag is the operator stating intent. Falling back to $TZ after
        // they typed something would deploy a schedule they did not ask for.
        assert!(resolve_time_zone(Some("Mars/Olympus!"), Some("UTC"), Some("UTC")).is_err());
    }

    #[test]
    fn cron_to_at_summarises_only_what_this_cli_would_have_written() {
        assert_eq!(cron_to_at("0 3 * * *"), Some((3, 0)));
        assert_eq!(cron_to_at("30 22 * * *"), Some((22, 30)));
        assert_eq!(cron_to_at("0 6 * * 0"), Some((6, 0)));
        // A hand-edited expression must be shown verbatim, not mis-summarised
        // as a time it does not mean.
        assert_eq!(cron_to_at("*/5 * * * *"), None);
        assert_eq!(cron_to_at("0 3 1 * *"), None);
        assert_eq!(cron_to_at("0 3 * 6 *"), None);
        assert_eq!(cron_to_at("bogus"), None);
        assert_eq!(cron_to_at(""), None);
    }

    // ------------------------------------------------------------------
    // construct_repo_url — pure URL construction
    // ------------------------------------------------------------------

    #[test]
    fn construct_repo_url_bare_and_endpoint_builds_s3_https() {
        let url = construct_repo_url("apprafter", Some("nbg1.your-objectstorage.com"), None)
            .expect("should succeed");
        assert_eq!(url, "s3:https://nbg1.your-objectstorage.com/apprafter");
    }

    #[test]
    fn construct_repo_url_bare_endpoint_prefix_appended() {
        let url = construct_repo_url(
            "mybucket",
            Some("nbg1.your-objectstorage.com"),
            Some("backups/prod"),
        )
        .expect("should succeed");
        assert_eq!(
            url,
            "s3:https://nbg1.your-objectstorage.com/mybucket/backups/prod"
        );
    }

    #[test]
    fn construct_repo_url_endpoint_with_https_scheme_stripped() {
        // User passed https://host — strip it, default scheme is https.
        let url = construct_repo_url("bucket", Some("https://nbg1.your-objectstorage.com"), None)
            .expect("should succeed");
        assert_eq!(url, "s3:https://nbg1.your-objectstorage.com/bucket");
    }

    #[test]
    fn construct_repo_url_endpoint_with_http_scheme_honoured() {
        let url = construct_repo_url("bucket", Some("http://my-minio.internal"), None)
            .expect("should succeed");
        assert_eq!(url, "s3:http://my-minio.internal/bucket");
    }

    #[test]
    fn construct_repo_url_full_s3_url_passthrough() {
        let full = "s3:https://s3.eu-central-1.amazonaws.com/mybucket/prefix";
        let url = construct_repo_url(full, None, None).expect("should succeed");
        assert_eq!(url, full);
    }

    #[test]
    fn construct_repo_url_full_url_plus_endpoint_is_error() {
        let err = construct_repo_url(
            "s3:https://host/bucket",
            Some("other-host.example.com"),
            None,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("EITHER"),
            "error should mention 'EITHER': {msg}"
        );
    }

    #[test]
    fn construct_repo_url_bare_name_without_endpoint_is_error() {
        let err = construct_repo_url("mybucket", None, None).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("--endpoint"),
            "error should mention '--endpoint': {msg}"
        );
    }

    #[test]
    fn construct_repo_url_local_path_passthrough() {
        let url = construct_repo_url("/tmp/myrepo", None, None).expect("should succeed");
        assert_eq!(url, "/tmp/myrepo");
    }

    #[test]
    fn app_namespaces_derive_from_apprafter_applications_not_all_ns() {
        let apps = vec![
            json!({"metadata":{"name":"alpha","namespace":"demo"}}),
            json!({"metadata":{"name":"beta","namespace":"demo"}}),
            json!({"metadata":{"name":"shop","namespace":"prod"}}),
        ];
        assert_eq!(app_namespaces(&apps, &[]), vec!["demo", "prod"]);
        assert_eq!(app_namespaces(&apps, &["prod".to_string()]), vec!["prod"]);
    }

    #[test]
    fn backup_requires_passphrase() {
        assert!(backup_passphrase_or_error(None, None, false).is_err());
        assert!(backup_passphrase_or_error(Some("p"), None, false).is_ok());
    }

    #[test]
    fn cnpg_cluster_image_prefers_spec_then_status() {
        // Explicit spec.imageName wins.
        let spec = json!({"spec":{"imageName":"ghcr.io/cloudnative-pg/postgresql:16.2"}});
        assert_eq!(
            cnpg_cluster_image(&spec).as_deref(),
            Some("ghcr.io/cloudnative-pg/postgresql:16.2")
        );
        // Absent/empty spec.imageName falls back to the resolved status.image —
        // the integrated shared cluster path (CNPG derives PG 18 from its own
        // default, leaving spec.imageName empty). This is the regression: an
        // app-ns-only, spec-only lookup returned None → default postgres:16 →
        // `pg_dump: server version mismatch` against the PG 18 server.
        let status_only =
            json!({"spec":{},"status":{"image":"ghcr.io/cloudnative-pg/postgresql:18.3-1"}});
        assert_eq!(
            cnpg_cluster_image(&status_only).as_deref(),
            Some("ghcr.io/cloudnative-pg/postgresql:18.3-1")
        );
        let empty_spec = json!({"spec":{"imageName":""},"status":{"image":"postgres:17"}});
        assert_eq!(
            cnpg_cluster_image(&empty_spec).as_deref(),
            Some("postgres:17")
        );
        // The chosen image drives the helper major — 18.3-1 → postgres:18-alpine.
        assert_eq!(
            pg_helper_image(cnpg_cluster_image(&status_only).as_deref()),
            "postgres:18-alpine"
        );
        // Neither present → None (caller uses the pinned default).
        assert_eq!(cnpg_cluster_image(&json!({"spec":{}})), None);
    }

    #[test]
    fn sourcecred_material_refs_follow_git_and_registry() {
        // a SourceCredential CR with both git + registry sealedSecretRefs
        let sc = json!({"metadata":{"name":"ghcr","namespace":"apprafter-system"},
            "spec":{"git":{"backend":{"sealedSecretRef":{"name":"ghcr-git"}}},
                    "registry":{"backend":{"sealedSecretRef":{"name":"ghcr-reg"}}}}});
        let refs = sourcecred_material_refs(&sc);
        // each ref defaults ns to the CR's own namespace (apprafter-system)
        assert!(refs
            .iter()
            .any(|(ns, n)| ns == "apprafter-system" && n == "ghcr-git"));
        assert!(refs
            .iter()
            .any(|(ns, n)| ns == "apprafter-system" && n == "ghcr-reg"));
    }

    // ------------------------------------------------------------------
    // 1a. parse_credential_file
    // ------------------------------------------------------------------

    #[test]
    fn credential_file_parses_dotenv_keys() {
        // parse_credential_file returns the RAW map (no normalisation yet).
        let m = parse_credential_file(
            "# creds\nAWS_ACCESS_KEY_ID=AK\nAWS_SECRET_ACCESS_KEY=sk\nRESTIC_PASSWORD=p\n\n\
             AWS_DEFAULT_REGION = eu \n",
        );
        assert_eq!(m.get("AWS_ACCESS_KEY_ID").map(String::as_str), Some("AK"));
        assert_eq!(m.get("RESTIC_PASSWORD").map(String::as_str), Some("p"));
        assert_eq!(m.get("AWS_DEFAULT_REGION").map(String::as_str), Some("eu")); // trimmed
        assert!(!m.contains_key("# creds"));
    }

    #[test]
    fn credential_file_value_may_contain_equals() {
        let m = parse_credential_file("RESTIC_PASSWORD=a=b=c\n");
        assert_eq!(m.get("RESTIC_PASSWORD").map(String::as_str), Some("a=b=c"));
    }

    // ------------------------------------------------------------------
    // 1b. normalize_s3_creds — FIX A: AWS_* input → S3_* canonical
    // ------------------------------------------------------------------

    #[test]
    fn normalize_aws_aliases_to_canonical_s3_keys() {
        let raw: BTreeMap<String, String> = [
            ("AWS_ACCESS_KEY_ID", "AKID"),
            ("AWS_SECRET_ACCESS_KEY", "SKEY"),
            ("AWS_DEFAULT_REGION", "eu-central-1"),
            ("RESTIC_PASSWORD", "pass"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let canonical = normalize_s3_creds(raw);
        assert_eq!(
            canonical.get("S3_ACCESS_KEY_ID").map(String::as_str),
            Some("AKID")
        );
        assert_eq!(
            canonical.get("S3_SECRET_ACCESS_KEY").map(String::as_str),
            Some("SKEY")
        );
        assert_eq!(
            canonical.get("S3_REGION").map(String::as_str),
            Some("eu-central-1")
        );
        assert_eq!(
            canonical.get("RESTIC_PASSWORD").map(String::as_str),
            Some("pass")
        );
        // Original AWS_* keys must NOT be in the output.
        assert!(!canonical.contains_key("AWS_ACCESS_KEY_ID"));
        assert!(!canonical.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!canonical.contains_key("AWS_DEFAULT_REGION"));
    }

    #[test]
    fn normalize_canonical_s3_keys_unchanged() {
        let raw: BTreeMap<String, String> = [
            ("S3_ACCESS_KEY_ID", "AKID"),
            ("S3_SECRET_ACCESS_KEY", "SKEY"),
            ("S3_REGION", "eu-central-1"),
            ("RESTIC_PASSWORD", "pass"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let canonical = normalize_s3_creds(raw);
        assert_eq!(
            canonical.get("S3_ACCESS_KEY_ID").map(String::as_str),
            Some("AKID")
        );
        assert_eq!(
            canonical.get("S3_SECRET_ACCESS_KEY").map(String::as_str),
            Some("SKEY")
        );
        assert_eq!(
            canonical.get("S3_REGION").map(String::as_str),
            Some("eu-central-1")
        );
        assert_eq!(
            canonical.get("RESTIC_PASSWORD").map(String::as_str),
            Some("pass")
        );
    }

    #[test]
    fn normalize_canonical_wins_over_alias_when_both_present() {
        // Explicit S3_ACCESS_KEY_ID wins over AWS_ACCESS_KEY_ID alias.
        let raw: BTreeMap<String, String> = [
            ("S3_ACCESS_KEY_ID", "canonical-key"),
            ("AWS_ACCESS_KEY_ID", "alias-key"),
            ("S3_SECRET_ACCESS_KEY", "SKEY"),
            ("RESTIC_PASSWORD", "pass"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let canonical = normalize_s3_creds(raw);
        // The canonical form (S3_ACCESS_KEY_ID) must win.
        assert_eq!(
            canonical.get("S3_ACCESS_KEY_ID").map(String::as_str),
            Some("canonical-key")
        );
        assert!(!canonical.contains_key("AWS_ACCESS_KEY_ID"));
    }

    // ------------------------------------------------------------------
    // 1c. translate_creds_for_restic — FIX A: S3_* → AWS_* for restic subprocess
    // ------------------------------------------------------------------

    #[test]
    fn translate_canonical_to_restic_aws_names() {
        let canonical: BTreeMap<String, String> = [
            ("S3_ACCESS_KEY_ID", "AKID"),
            ("S3_SECRET_ACCESS_KEY", "SKEY"),
            ("S3_REGION", "eu-central-1"),
            ("RESTIC_PASSWORD", "pass"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let restic_env = translate_creds_for_restic(&canonical);
        assert_eq!(
            restic_env.get("AWS_ACCESS_KEY_ID").map(String::as_str),
            Some("AKID")
        );
        assert_eq!(
            restic_env.get("AWS_SECRET_ACCESS_KEY").map(String::as_str),
            Some("SKEY")
        );
        assert_eq!(
            restic_env.get("AWS_DEFAULT_REGION").map(String::as_str),
            Some("eu-central-1")
        );
        assert_eq!(
            restic_env.get("RESTIC_PASSWORD").map(String::as_str),
            Some("pass")
        );
        // S3_* keys must NOT be in the restic-facing env.
        assert!(!restic_env.contains_key("S3_ACCESS_KEY_ID"));
        assert!(!restic_env.contains_key("S3_SECRET_ACCESS_KEY"));
        assert!(!restic_env.contains_key("S3_REGION"));
    }

    // ------------------------------------------------------------------
    // 1d. validate_required_cred_keys — FIX C: missing key → error naming it
    // ------------------------------------------------------------------

    #[test]
    fn validate_required_keys_missing_key_names_it_in_error() {
        // Map with S3_ACCESS_KEY_ID + S3_SECRET_ACCESS_KEY but NO RESTIC_PASSWORD
        let canonical: BTreeMap<String, String> = [
            ("S3_ACCESS_KEY_ID", "AKID"),
            ("S3_SECRET_ACCESS_KEY", "SKEY"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let err = validate_required_cred_keys(&canonical).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("RESTIC_PASSWORD"),
            "error must name the missing key: {msg}"
        );
        // Must also contain the full help text pointing to both input paths.
        assert!(
            msg.contains("S3_ACCESS_KEY_ID"),
            "error must name canonical keys: {msg}"
        );
        assert!(
            msg.contains("--credential-file"),
            "error must mention --credential-file: {msg}"
        );
    }

    #[test]
    fn validate_required_keys_all_present_ok() {
        let canonical: BTreeMap<String, String> = [
            ("S3_ACCESS_KEY_ID", "AKID"),
            ("S3_SECRET_ACCESS_KEY", "SKEY"),
            ("RESTIC_PASSWORD", "pass"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        assert!(validate_required_cred_keys(&canonical).is_ok());
    }

    // ------------------------------------------------------------------
    // 1e. resolve_operator_s3_creds — FIX A + FIX C: normalises + validates
    // ------------------------------------------------------------------

    #[test]
    fn resolve_creds_from_env_lookup_when_no_file_normalises_aws_aliases() {
        // AWS_* aliases in env → canonical S3_* output.
        let env: BTreeMap<&str, &str> = [
            ("AWS_ACCESS_KEY_ID", "AK"),
            ("AWS_SECRET_ACCESS_KEY", "SK"),
            ("RESTIC_PASSWORD", "p"),
        ]
        .into();
        let m = resolve_operator_s3_creds(None, &|k| env.get(k).map(|s| s.to_string())).unwrap();
        // Result must be in canonical S3_* form.
        assert_eq!(m.get("S3_ACCESS_KEY_ID").map(String::as_str), Some("AK"));
        assert_eq!(
            m.get("S3_SECRET_ACCESS_KEY").map(String::as_str),
            Some("SK")
        );
        assert_eq!(m.get("RESTIC_PASSWORD").map(String::as_str), Some("p"));
        // AWS_* must NOT leak into the canonical output.
        assert!(!m.contains_key("AWS_ACCESS_KEY_ID"));
    }

    #[test]
    fn resolve_creds_from_env_lookup_canonical_s3_keys_passthrough() {
        // S3_* canonical keys in env → unchanged canonical S3_* output.
        let env: BTreeMap<&str, &str> = [
            ("S3_ACCESS_KEY_ID", "AK"),
            ("S3_SECRET_ACCESS_KEY", "SK"),
            ("RESTIC_PASSWORD", "p"),
        ]
        .into();
        let m = resolve_operator_s3_creds(None, &|k| env.get(k).map(|s| s.to_string())).unwrap();
        assert_eq!(m.get("S3_ACCESS_KEY_ID").map(String::as_str), Some("AK"));
        assert_eq!(m.get("RESTIC_PASSWORD").map(String::as_str), Some("p"));
    }

    #[test]
    fn resolve_creds_errors_when_no_password() {
        let err = resolve_operator_s3_creds(None, &|_| None);
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        // FIX C: error must enumerate the required keys.
        assert!(
            msg.contains("RESTIC_PASSWORD") || msg.contains("S3_ACCESS_KEY_ID"),
            "error must enumerate required keys: {msg}"
        );
    }

    #[test]
    fn resolve_creds_from_credential_file_normalises_aws_aliases() {
        use std::io::Write as _;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        // File uses AWS_* aliases — must be normalised to S3_* canonical.
        writeln!(
            f,
            "AWS_ACCESS_KEY_ID=FILEKEY\nAWS_SECRET_ACCESS_KEY=FILESEC\nRESTIC_PASSWORD=filepass\n"
        )
        .unwrap();
        let m = resolve_operator_s3_creds(Some(f.path()), &|_| None).unwrap();
        // Must be in canonical S3_* form.
        assert_eq!(
            m.get("S3_ACCESS_KEY_ID").map(String::as_str),
            Some("FILEKEY")
        );
        assert_eq!(
            m.get("S3_SECRET_ACCESS_KEY").map(String::as_str),
            Some("FILESEC")
        );
        assert_eq!(
            m.get("RESTIC_PASSWORD").map(String::as_str),
            Some("filepass")
        );
        // AWS_* must NOT appear in the canonical output.
        assert!(!m.contains_key("AWS_ACCESS_KEY_ID"));
    }

    #[test]
    fn resolve_creds_from_credential_file_canonical_s3_keys_passthrough() {
        use std::io::Write as _;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        // File uses S3_* canonical keys — must be kept as-is.
        writeln!(
            f,
            "S3_ACCESS_KEY_ID=FILEKEY\nS3_SECRET_ACCESS_KEY=FILESEC\nRESTIC_PASSWORD=filepass\n"
        )
        .unwrap();
        let m = resolve_operator_s3_creds(Some(f.path()), &|_| None).unwrap();
        assert_eq!(
            m.get("S3_ACCESS_KEY_ID").map(String::as_str),
            Some("FILEKEY")
        );
        assert_eq!(
            m.get("RESTIC_PASSWORD").map(String::as_str),
            Some("filepass")
        );
    }

    // ------------------------------------------------------------------
    // 1f. apply_creds_to_command — translates S3_* → AWS_* for restic process
    // ------------------------------------------------------------------

    #[test]
    fn apply_creds_to_command_translates_canonical_to_aws_for_restic() {
        // Use canonical S3_* keys (as stored in the canonical map).
        let mut creds = BTreeMap::new();
        creds.insert("RESTIC_PASSWORD".to_string(), "testpass".to_string());
        creds.insert("S3_ACCESS_KEY_ID".to_string(), "AKID".to_string());
        creds.insert("S3_SECRET_ACCESS_KEY".to_string(), "SKEY".to_string());
        let mut cmd = Command::new("true");
        apply_creds_to_command(&mut cmd, &creds);

        // READ THE ENV BACK. This test used to call the function, assert
        // nothing, and say so: "if we reach here without panic the function is
        // wired correctly". That passes if `apply_creds_to_command` sets
        // NOTHING — and a restic subprocess with no credentials fails far from
        // here, against a bucket, with an authentication error nobody traces to
        // an empty env map. `Command::get_envs` makes the real assertion cheap.
        let env: std::collections::BTreeMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();

        // restic reads AWS_* natively; the canonical S3_* names are ours.
        assert_eq!(
            env.get("AWS_ACCESS_KEY_ID").map(String::as_str),
            Some("AKID")
        );
        assert_eq!(
            env.get("AWS_SECRET_ACCESS_KEY").map(String::as_str),
            Some("SKEY")
        );
        // The passphrase passes through under its own name.
        assert_eq!(
            env.get("RESTIC_PASSWORD").map(String::as_str),
            Some("testpass")
        );
        // And the translation is a RENAME, not a copy: leaving the S3_* names
        // on the subprocess would mean two spellings of one secret in the
        // child's environment, and a reader could not tell which restic used.
        assert!(
            !env.contains_key("S3_ACCESS_KEY_ID") && !env.contains_key("S3_SECRET_ACCESS_KEY"),
            "canonical S3_* names must not reach the subprocess: {env:?}"
        );
    }

    // ------------------------------------------------------------------
    // 2z. parse_staging_mode (local-pull `apprafter backup --staging-mode`)
    // ------------------------------------------------------------------

    #[test]
    fn staging_mode_defaults_monolithic() {
        assert!(matches!(
            parse_staging_mode(None).unwrap(),
            StagingMode::Monolithic
        ));
    }

    #[test]
    fn staging_mode_explicit_monolithic() {
        assert!(matches!(
            parse_staging_mode(Some("monolithic")).unwrap(),
            StagingMode::Monolithic
        ));
    }

    #[test]
    fn staging_mode_sequential() {
        assert!(matches!(
            parse_staging_mode(Some("sequential")).unwrap(),
            StagingMode::Sequential
        ));
    }

    #[test]
    fn staging_mode_rejects_garbage() {
        assert!(parse_staging_mode(Some("weird")).is_err());
    }

    // ------------------------------------------------------------------
    // 2a. backup_enable_patch / backup_disable_patch (pure patch builders)
    // ------------------------------------------------------------------

    #[test]
    fn enable_patch_sets_spec_backup_fields() {
        let sched = ResolvedSchedule {
            schedule: "0 2 * * *".into(),
            check_schedule: "0 5 * * 0".into(),
            time_zone: "Europe/Berlin".into(),
        };
        let p = backup_enable_patch(
            &EnableOpts {
                bucket: "s3:x".into(),
                credential: "c".into(),
                enforce: Some("cluster".into()),
                staging_mode: Some("sequential".into()),
                keep_daily: Some(5),
                ..Default::default()
            },
            &sched,
        );
        assert_eq!(
            p["spec"]["backup"]["timeZone"],
            serde_json::json!("Europe/Berlin"),
            "a schedule without its zone is not a schedule: {p}"
        );
        assert_eq!(p["spec"]["backup"]["enabled"], serde_json::json!(true));
        assert_eq!(p["spec"]["backup"]["bucket"], serde_json::json!("s3:x"));
        assert_eq!(
            p["spec"]["backup"]["credentialRef"]["name"],
            serde_json::json!("c")
        );
        assert_eq!(
            p["spec"]["backup"]["schedule"],
            serde_json::json!("0 2 * * *")
        );
        assert_eq!(
            p["spec"]["backup"]["retention"]["enforce"],
            serde_json::json!("cluster")
        );
        assert_eq!(
            p["spec"]["backup"]["retention"]["keepDaily"],
            serde_json::json!(5)
        );
        assert_eq!(
            p["spec"]["backup"]["stagingMode"],
            serde_json::json!("sequential")
        );
    }

    #[test]
    fn enable_patch_omits_retention_when_no_retention_flags() {
        // No keep_*/enforce set → the whole retention block is absent (a bare
        // enable that leaves retention to the operator/chart default).
        let p = backup_enable_patch(
            &EnableOpts {
                bucket: "s3:x".into(),
                credential: "c".into(),
                failure_webhook: Some("https://hook".into()),
                ..Default::default()
            },
            &ResolvedSchedule {
                schedule: "0 3 * * *".into(),
                check_schedule: "0 6 * * 0".into(),
                time_zone: "UTC".into(),
            },
        );
        assert!(
            p["spec"]["backup"].get("retention").is_none(),
            "retention must be absent when no retention flag is set: {p}"
        );
        // Optional non-retention fields still flow through when present.
        assert_eq!(
            p["spec"]["backup"]["checkSchedule"],
            serde_json::json!("0 6 * * 0")
        );
        assert_eq!(
            p["spec"]["backup"]["failureWebhook"],
            serde_json::json!("https://hook")
        );
        // schedule / stagingMode are CRD-REQUIRED, so a bare enable defaults
        // them (NOT omitted — the apiserver would reject a partial patch).
        assert_eq!(
            p["spec"]["backup"]["schedule"],
            serde_json::json!("0 3 * * *")
        );
        assert_eq!(
            p["spec"]["backup"]["stagingMode"],
            serde_json::json!("monolithic")
        );
    }

    #[test]
    fn enable_patch_always_carries_every_crd_required_field() {
        // Regression: the PlatformStack CRD marks
        // [enabled, schedule, bucket, credentialRef, stagingMode,
        //  checkSchedule, checkReadData] required whenever spec.backup is
        // present. A minimal `apprafter backup enable --bucket --credential`
        // (no other flags) MUST still produce all of them, else the apiserver
        // rejects the merge-patch ("schedule: Required value") and every
        // enable fails.
        let p = backup_enable_patch(
            &EnableOpts {
                bucket: "s3:b".into(),
                credential: "cred".into(),
                ..Default::default()
            },
            &ResolvedSchedule {
                schedule: "0 3 * * *".into(),
                check_schedule: "0 6 * * 0".into(),
                time_zone: "UTC".into(),
            },
        );
        let b = &p["spec"]["backup"];
        for key in [
            "enabled",
            "schedule",
            "bucket",
            "credentialRef",
            "stagingMode",
            "checkSchedule",
            "checkReadData",
        ] {
            assert!(
                b.get(key).is_some(),
                "CRD-required field '{key}' missing from a bare enable patch: {p}"
            );
        }
        assert_eq!(b["enabled"], serde_json::json!(true));
        assert_eq!(b["schedule"], serde_json::json!("0 3 * * *"));
        assert_eq!(b["stagingMode"], serde_json::json!("monolithic"));
        assert_eq!(b["checkSchedule"], serde_json::json!("0 6 * * 0"));
        assert_eq!(b["checkReadData"], serde_json::json!(false));
    }

    #[test]
    fn enable_patch_retention_includes_only_set_keys() {
        // Only keep_weekly set → retention present with just keepWeekly.
        let p = backup_enable_patch(
            &EnableOpts {
                bucket: "s3:x".into(),
                credential: "c".into(),
                keep_weekly: Some(3),
                ..Default::default()
            },
            &ResolvedSchedule {
                schedule: "0 3 * * *".into(),
                check_schedule: "0 6 * * 0".into(),
                time_zone: "UTC".into(),
            },
        );
        let ret = &p["spec"]["backup"]["retention"];
        assert_eq!(ret["keepWeekly"], serde_json::json!(3));
        assert!(ret.get("keepDaily").is_none());
        assert!(ret.get("keepMonthly").is_none());
        assert!(ret.get("enforce").is_none());
    }

    #[test]
    fn disable_patch_sets_enabled_false() {
        assert_eq!(
            backup_disable_patch()["spec"]["backup"]["enabled"],
            serde_json::json!(false)
        );
    }

    // ------------------------------------------------------------------
    // 5. retention_from_spec_backup (CLI override → CR → 7/4/6 default)
    // ------------------------------------------------------------------

    #[test]
    fn retention_from_spec_backup_uses_cr_values() {
        let spec = json!({
            "bucket": "s3:x",
            "retention": { "keepDaily": 10, "keepWeekly": 8, "keepMonthly": 12 }
        });
        let p = retention_from_spec_backup(Some(&spec), None, None, None);
        assert_eq!(p.keep_daily, 10);
        assert_eq!(p.keep_weekly, 8);
        assert_eq!(p.keep_monthly, 12);
    }

    #[test]
    fn retention_from_spec_backup_override_wins_over_cr() {
        let spec = json!({
            "retention": { "keepDaily": 10, "keepWeekly": 8, "keepMonthly": 12 }
        });
        // keep_daily override wins; the other two fall back to the CR.
        let p = retention_from_spec_backup(Some(&spec), Some(3), None, None);
        assert_eq!(p.keep_daily, 3);
        assert_eq!(p.keep_weekly, 8);
        assert_eq!(p.keep_monthly, 12);
    }

    #[test]
    fn retention_from_spec_backup_all_unset_is_default_7_4_6() {
        // No CR retention block and no overrides → the 7/4/6 default.
        let p = retention_from_spec_backup(None, None, None, None);
        assert_eq!(p.keep_daily, 7);
        assert_eq!(p.keep_weekly, 4);
        assert_eq!(p.keep_monthly, 6);
        // A CR with no `.retention` also falls through to the default.
        let spec = json!({ "bucket": "s3:x" });
        let p2 = retention_from_spec_backup(Some(&spec), None, None, None);
        assert_eq!(p2.keep_daily, 7);
        assert_eq!(p2.keep_weekly, 4);
        assert_eq!(p2.keep_monthly, 6);
    }

    #[test]
    fn retention_override_applies_with_no_cr_retention() {
        let spec = json!({ "bucket": "s3:x" });
        let p = retention_from_spec_backup(Some(&spec), Some(1), Some(2), Some(3));
        assert_eq!(p.keep_daily, 1);
        assert_eq!(p.keep_weekly, 2);
        assert_eq!(p.keep_monthly, 3);
    }

    // ------------------------------------------------------------------
    // 2c. restic version preflight (pure parse + comparison)
    // ------------------------------------------------------------------

    #[test]
    fn parse_restic_version_reads_dotted_triple() {
        assert_eq!(
            parse_restic_version("restic 0.16.4 compiled with go1.21.6 on linux/amd64"),
            Some((0, 16, 4))
        );
        assert_eq!(parse_restic_version("restic 0.14.0"), Some((0, 14, 0)));
        // Leading `v` tolerated.
        assert_eq!(parse_restic_version("v1.2.3"), Some((1, 2, 3)));
        // No dotted triple at all → None (warn+continue path).
        assert_eq!(parse_restic_version("restic unknown"), None);
    }

    #[test]
    fn restic_version_gate_rejects_below_014() {
        assert!(restic_version_too_old((0, 13, 0)));
        assert!(restic_version_too_old((0, 9, 6)));
        assert!(!restic_version_too_old((0, 14, 0)));
        assert!(!restic_version_too_old((0, 16, 4)));
        assert!(!restic_version_too_old((1, 0, 0)));
    }

    // ------------------------------------------------------------------
    // 3a. format_backup_status
    // ------------------------------------------------------------------

    #[test]
    fn status_disabled_when_no_spec_backup() {
        let s = format_backup_status(None, &[], None, None);
        assert!(s.to_lowercase().contains("disabled"));
    }

    #[test]
    fn status_disabled_when_enabled_false() {
        let spec = json!({"enabled": false, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), &[], None, None);
        assert!(s.to_lowercase().contains("disabled"));
        // Config is retained and shown even when disabled.
        assert!(s.contains("s3:x"));
    }

    #[test]
    fn status_renders_enabled_config_and_last_prune() {
        let spec = json!({"enabled": true, "bucket": "s3:x", "schedule": "0 3 * * *", "stagingMode": "monolithic"});
        let s = format_backup_status(Some(&spec), &[], None, Some("2026-07-17T03:00:00Z"));
        assert!(s.contains("s3:x"));
        assert!(s.contains("2026-07-17T03:00:00Z"));
        // 2.22g: rendered as a TIME now, not a cron expression.
        assert!(s.contains("daily at 03:00"), "{s}");
        assert!(s.contains("monolithic"));
    }

    #[test]
    fn status_prints_the_schedule_back_in_the_zone_it_was_given() {
        let spec = json!({
            "enabled": true, "bucket": "s3:x", "stagingMode": "monolithic",
            "schedule": "30 22 * * *", "checkSchedule": "30 1 * * 0",
            "timeZone": "Europe/Berlin"
        });
        let s = format_backup_status(Some(&spec), &[], None, None);
        assert!(s.contains("daily at 22:30 Europe/Berlin"), "{s}");
        assert!(s.contains("Sundays at 01:30 Europe/Berlin"), "{s}");
    }

    #[test]
    fn status_says_off_rather_than_showing_an_empty_schedule() {
        // Empty is not missing: it is `--check off`, and the chart omits the
        // CronJob entirely. An operator must be able to tell "no check" from
        // "a check I cannot read".
        let spec = json!({
            "enabled": true, "bucket": "s3:x", "schedule": "0 3 * * *",
            "checkSchedule": "", "timeZone": "UTC"
        });
        let s = format_backup_status(Some(&spec), &[], None, None);
        assert!(s.contains("check:         off"), "{s}");
    }

    #[test]
    fn status_names_the_missing_zone_instead_of_printing_a_bare_time() {
        // A cluster enabled before 2.22g has no timeZone. Printing "03:00"
        // alone reads as local time; it is actually the
        // kube-controller-manager's zone, which is the trap D2 is about.
        let spec = json!({"enabled": true, "bucket": "s3:x", "schedule": "0 3 * * *"});
        let s = format_backup_status(Some(&spec), &[], None, None);
        assert!(s.contains("cluster timezone"), "{s}");
        assert!(s.contains("backup enable"), "{s}");
    }

    #[test]
    fn status_shows_a_hand_edited_cron_verbatim() {
        // Summarising `*/5 * * * *` as a time would be a confident wrong
        // answer about somebody's own schedule.
        let spec = json!({
            "enabled": true, "bucket": "s3:x", "schedule": "*/5 * * * *",
            "timeZone": "UTC"
        });
        let s = format_backup_status(Some(&spec), &[], None, None);
        assert!(s.contains("*/5 * * * * UTC"), "{s}");
    }

    #[test]
    fn status_reports_job_outcome() {
        let job = json!({
            "metadata": {"name": "apprafter-backup-28900000"},
            "status": {"succeeded": 1}
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), std::slice::from_ref(&job), None, None);
        assert!(s.contains("apprafter-backup-28900000"));
        assert!(s.contains("Succeeded"));
    }

    #[test]
    fn status_cm_last_success_and_error_keys_render() {
        // Uses the REAL keys from apprafter-backup/src/status.rs:
        // lastSuccess, lastFailure, lastError, lastRunFormat.
        let cm = json!({
            "data": {
                "lastSuccess": "2026-07-17T03:00:00Z",
                "lastFailure": "2026-07-16T03:00:00Z",
                "lastError": "restic: connection refused",
                "lastRunFormat": "monolithic"
            }
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), &[], Some(&cm), None);
        assert!(
            s.contains("2026-07-17T03:00:00Z"),
            "lastSuccess not rendered: {s}"
        );
        assert!(
            s.contains("2026-07-16T03:00:00Z"),
            "lastFailure not rendered: {s}"
        );
        assert!(
            s.contains("restic: connection refused"),
            "lastError not rendered: {s}"
        );
        assert!(s.contains("monolithic"), "lastRunFormat not rendered: {s}");
    }

    #[test]
    fn status_last_prune_never_when_absent() {
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), &[], None, None);
        assert!(s.contains("Last prune: never"));
    }

    #[test]
    fn status_picks_most_recent_job_by_start_time() {
        let job_old = json!({
            "metadata": {"name": "apprafter-backup-28800000"},
            "status": {"startTime": "2026-07-16T03:00:00Z", "failed": 1}
        });
        let job_new = json!({
            "metadata": {"name": "apprafter-backup-28900000"},
            "status": {"startTime": "2026-07-17T03:00:00Z", "succeeded": 1}
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), &[job_old, job_new], None, None);
        // Most-recent (new) should appear in the "Last backup Job" line.
        assert!(s.contains("apprafter-backup-28900000"));
        assert!(s.contains("Succeeded"));
    }

    // ------------------------------------------------------------------
    // 2d. backup_verb_needs_cluster — when may check/prune/unlock run with
    //     no cluster at all (the DR case: the cluster is gone by design)
    // ------------------------------------------------------------------

    /// `--keep-*` triple, for terse table rows below.
    fn prune_keeps(d: Option<u32>, w: Option<u32>, m: Option<u32>) -> RetentionArgs {
        RetentionArgs::Prune {
            keep_daily: d,
            keep_weekly: w,
            keep_monthly: m,
        }
    }

    #[test]
    fn needs_cluster_table_check_and_unlock() {
        // check / unlock carry no retention inputs: `--repo` is the ONLY
        // reason they'd have to reach the cluster.
        let table = [
            (None, true, "no --repo → must read spec.backup.bucket"),
            (
                Some("s3:https://h/b"),
                false,
                "--repo given → fully offline",
            ),
        ];
        for (repo, expect, why) in table {
            assert_eq!(
                backup_verb_needs_cluster(repo, RetentionArgs::NotApplicable),
                expect,
                "{why}"
            );
        }
    }

    #[test]
    fn needs_cluster_table_prune() {
        let repo = Some("s3:https://h/b");
        let table = [
            // (repo, keeps, needs_cluster, why)
            (
                repo,
                prune_keeps(Some(7), Some(4), Some(6)),
                false,
                "--repo + all three --keep-* → nothing left to read from the CR",
            ),
            (
                repo,
                prune_keeps(None, Some(4), Some(6)),
                true,
                "--keep-daily missing → retention defaults come from the CR",
            ),
            (
                repo,
                prune_keeps(Some(7), None, Some(6)),
                true,
                "--keep-weekly missing → retention defaults come from the CR",
            ),
            (
                repo,
                prune_keeps(Some(7), Some(4), None),
                true,
                "--keep-monthly missing → retention defaults come from the CR",
            ),
            (
                repo,
                prune_keeps(None, None, None),
                true,
                "no --keep-* at all → retention defaults come from the CR",
            ),
            (
                None,
                prune_keeps(Some(7), Some(4), Some(6)),
                true,
                "no --repo → the bucket still comes from the CR",
            ),
            (None, prune_keeps(None, None, None), true, "nothing given"),
        ];
        for (r, keeps, expect, why) in table {
            assert_eq!(backup_verb_needs_cluster(r, keeps), expect, "{why}");
        }
    }

    #[test]
    fn needs_cluster_repo_alone_is_enough_for_check_but_not_for_prune() {
        // The asymmetry, stated on its own: `--repo` fully frees check/unlock,
        // but prune ALSO needs the retention policy, which otherwise comes from
        // `spec.backup.retention`.
        let repo = Some("s3:https://h/b");
        assert!(!backup_verb_needs_cluster(
            repo,
            RetentionArgs::NotApplicable
        ));
        assert!(backup_verb_needs_cluster(
            repo,
            prune_keeps(Some(7), Some(4), None)
        ));
    }

    #[test]
    fn offline_hint_for_check_points_at_repo_only() {
        let h = cluster_need(None, RetentionArgs::NotApplicable).hint("check");
        assert!(h.contains("backup check"), "names the verb: {h}");
        assert!(h.contains("--repo"), "names --repo: {h}");
        assert!(
            !h.contains("--keep-"),
            "check has no retention inputs — must not mention --keep-*: {h}"
        );
    }

    #[test]
    fn offline_hint_for_prune_names_only_the_missing_keep_flags() {
        // --repo and --keep-daily supplied; the hint must ask for exactly the
        // two that are missing and NOT re-ask for what was already given.
        let h =
            cluster_need(Some("s3:https://h/b"), prune_keeps(Some(7), None, Some(6))).hint("prune");
        assert!(h.contains("--keep-weekly"), "names the missing flag: {h}");
        assert!(
            !h.contains("--keep-daily"),
            "--keep-daily was supplied — must not be re-asked: {h}"
        );
        assert!(
            !h.contains("--keep-monthly"),
            "--keep-monthly was supplied — must not be re-asked: {h}"
        );
        assert!(
            !h.contains("--repo <"),
            "--repo was supplied — must not be re-asked: {h}"
        );
    }

    #[test]
    fn offline_hint_mentions_disaster_recovery_when_repo_missing() {
        let h = cluster_need(None, prune_keeps(None, None, None)).hint("prune");
        assert!(h.contains("--repo"), "{h}");
        assert!(h.contains("--keep-daily"), "{h}");
        assert!(h.contains("--keep-weekly"), "{h}");
        assert!(h.contains("--keep-monthly"), "{h}");
        assert!(
            h.to_lowercase().contains("no longer exists")
                || h.to_lowercase().contains("disaster recovery"),
            "the DR case must be named — the operator's cluster is SUPPOSED to be gone: {h}"
        );
    }

    #[test]
    fn resolve_backup_repo_with_override_needs_no_kubeconfig() {
        // Passing `None` for the kubeconfig proves the override path never
        // reaches for the cluster (a kubectl shell-out here would fail).
        let repo = resolve_backup_repo(Some("s3:https://h/b"), None).expect("override honoured");
        assert_eq!(repo, "s3:https://h/b");
    }

    #[test]
    fn resolve_backup_repo_without_override_or_cluster_is_an_error() {
        let err = resolve_backup_repo(None, None).expect_err("no repo, no cluster → error");
        let msg = format!("{err}");
        assert!(msg.contains("--repo"), "must point at --repo: {msg}");
    }

    #[test]
    fn repo_from_spec_backup_prefers_override_then_bucket() {
        let spec = json!({ "bucket": "s3:from-cr" });
        assert_eq!(
            repo_from_spec_backup(Some("s3:from-flag"), Some(&spec)).unwrap(),
            "s3:from-flag"
        );
        assert_eq!(
            repo_from_spec_backup(None, Some(&spec)).unwrap(),
            "s3:from-cr"
        );
        // Empty bucket is treated as unconfigured.
        let empty = json!({ "bucket": "" });
        assert!(repo_from_spec_backup(None, Some(&empty)).is_err());
    }

    // ------------------------------------------------------------------
    // Schedule composition — `resolve_schedule_from` (the pure core of
    // `resolve_schedule`) and the two describers.
    // ------------------------------------------------------------------

    /// `EnableOpts` carrying only the schedule surface, for the tests below.
    fn sched_opts(at: Option<&str>, check: Option<&str>, tz: Option<&str>) -> EnableOpts {
        EnableOpts {
            bucket: "s3:https://h/b".into(),
            credential: "c".into(),
            at: at.map(str::to_string),
            check: check.map(str::to_string),
            timezone: tz.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn one_at_flag_composes_both_crons_and_the_check_is_derived_from_it() {
        // The operator says WHEN once; the daily cron, the weekly check cron
        // and its Sunday field all follow from that single answer.
        let (s, source) = resolve_schedule_from(
            &sched_opts(Some("22:30"), None, Some("Europe/Berlin")),
            None,
            None,
        )
        .unwrap();
        assert_eq!(s.schedule, "30 22 * * *");
        assert_eq!(s.check_schedule, "30 1 * * 0");
        assert_eq!(s.time_zone, "Europe/Berlin");
        assert_eq!(source, ZoneSource::Flag);
    }

    #[test]
    fn a_bare_enable_keeps_the_historical_window_and_takes_the_zone_from_tz() {
        // The upgrade case: no schedule flags at all. The window must not
        // move, and $TZ must be honoured rather than the operator being asked
        // for something their shell already answered.
        let (s, source) =
            resolve_schedule_from(&sched_opts(None, None, None), Some("Europe/Berlin"), None)
                .unwrap();
        assert_eq!(s.schedule, DEFAULT_BACKUP_SCHEDULE);
        assert_eq!(s.check_schedule, DEFAULT_CHECK_SCHEDULE);
        assert_eq!(s.time_zone, "Europe/Berlin");
        assert_eq!(source, ZoneSource::TzEnv);
    }

    #[test]
    fn check_off_writes_an_empty_check_schedule_not_a_cron() {
        // `checkSchedule` is CRD-required, so the empty string is the ONLY way
        // to say "no weekly check"; the chart omits the CronJob on exactly
        // that value. Writing any cron here would leave the check running
        // after the operator turned it off.
        let (s, _) = resolve_schedule_from(
            &sched_opts(Some("03:00"), Some("off"), Some("UTC")),
            None,
            None,
        )
        .unwrap();
        assert_eq!(s.check_schedule, "");
        assert_eq!(s.schedule, "0 3 * * *");

        // An explicit check time lands on Sunday.
        let (s, _) = resolve_schedule_from(
            &sched_opts(Some("03:00"), Some("07:15"), Some("UTC")),
            None,
            None,
        )
        .unwrap();
        assert_eq!(s.check_schedule, "15 7 * * 0");
    }

    #[test]
    fn a_bad_check_time_is_reported_against_check_not_against_at() {
        // Both flags share one parser. Reporting a bad `--check` as a bad
        // `--at` sends the operator to edit the flag that was correct.
        let err = resolve_schedule_from(
            &sched_opts(Some("03:00"), Some("25:00"), Some("UTC")),
            None,
            None,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--check"), "{msg}");
        assert!(
            msg.contains("--check off"),
            "the off sentinel is offered: {msg}"
        );
        assert!(
            !msg.contains("--at"),
            "must not blame the other flag: {msg}"
        );
    }

    #[test]
    fn the_zone_source_is_named_distinctly_so_the_operator_knows_who_chose() {
        // "assumed" and "asked for" must not read the same in the output.
        let all = [
            ZoneSource::Flag.describe(),
            ZoneSource::TzEnv.describe(),
            ZoneSource::OperatingSystem.describe(),
        ];
        let mut sorted = all.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            3,
            "zone sources must be distinguishable: {all:?}"
        );
        assert!(all[0].contains("timezone"), "{all:?}");
        assert!(all[1].contains("TZ"), "{all:?}");
    }

    #[test]
    fn describe_schedule_reads_the_crons_back_as_times() {
        let s = describe_schedule(&ResolvedSchedule {
            schedule: "30 22 * * *".into(),
            check_schedule: "30 1 * * 0".into(),
            time_zone: "UTC".into(),
        });
        assert!(s.contains("backup daily at 22:30"), "{s}");
        assert!(s.contains("check Sundays at 01:30"), "{s}");
    }

    #[test]
    fn describe_schedule_says_off_and_shows_a_hand_edited_cron_verbatim() {
        let off = describe_schedule(&ResolvedSchedule {
            schedule: "0 3 * * *".into(),
            check_schedule: String::new(),
            time_zone: "UTC".into(),
        });
        assert!(off.contains("integrity check off"), "{off}");

        // A cron this CLI would not have written is never summarised as a
        // time it does not mean.
        let odd = describe_schedule(&ResolvedSchedule {
            schedule: "*/5 * * * *".into(),
            check_schedule: "0 6 1 * 0".into(),
            time_zone: "UTC".into(),
        });
        assert!(odd.contains("backup on `*/5 * * * *`"), "{odd}");
        assert!(odd.contains("check on `0 6 1 * 0`"), "{odd}");
    }

    #[test]
    fn describe_cron_weekly_only_summarises_a_sunday_cron() {
        assert_eq!(
            describe_cron_weekly("0 6 * * 0", Some("UTC")),
            "Sundays at 06:00 UTC"
        );
        // Any other day field, or an out-of-range time, is shown verbatim
        // rather than being relabelled "Sundays".
        assert!(describe_cron_weekly("0 6 * * 3", Some("UTC")).starts_with("0 6 * * 3"));
        assert!(describe_cron_weekly("0 99 * * 0", Some("UTC")).starts_with("0 99 * * 0"));
        assert!(describe_cron_weekly("nonsense", None).starts_with("nonsense"));
    }

    #[test]
    fn an_over_long_zone_name_is_refused() {
        // The shape check bounds the length too — `spec.timeZone` is not a
        // free-text field.
        assert!(validate_zone_shape(&format!("Europe/{}", "x".repeat(70))).is_err());
    }

    // ------------------------------------------------------------------
    // `backup enable` — the pure decisions, including the 2.22g read-back
    // ------------------------------------------------------------------

    #[test]
    fn enable_refuses_an_unknown_enum_before_anything_is_touched() {
        // These run before the kubeconfig, the seal and the DR prompt, so a
        // typo costs nothing.
        let bad_enforce = EnableOpts {
            enforce: Some("nobody".into()),
            ..Default::default()
        };
        let msg = format!("{}", validate_enable_enums(&bad_enforce).unwrap_err());
        assert!(msg.contains("operator") && msg.contains("cluster"), "{msg}");

        let bad_mode = EnableOpts {
            staging_mode: Some("weird".into()),
            ..Default::default()
        };
        let msg = format!("{}", validate_enable_enums(&bad_mode).unwrap_err());
        assert!(
            msg.contains("monolithic") && msg.contains("sequential"),
            "{msg}"
        );

        assert!(validate_enable_enums(&EnableOpts::default()).is_ok());
        assert!(validate_enable_enums(&EnableOpts {
            enforce: Some("operator".into()),
            staging_mode: Some("sequential".into()),
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn the_credential_name_defaults_only_when_the_flag_is_absent() {
        assert_eq!(
            effective_credential_name(""),
            DEFAULT_BACKUP_CREDENTIAL_NAME
        );
        assert_eq!(effective_credential_name("my-own-creds"), "my-own-creds");
    }

    #[test]
    fn a_credential_secrets_trailing_newline_never_reaches_s3_signing() {
        // `kubectl create secret --from-file` stores the file verbatim,
        // newline included. An AWS secret key with a trailing `\n` fails S3
        // signing with an authentication error that names nothing.
        let raw: BTreeMap<String, Vec<u8>> = [
            ("S3_SECRET_ACCESS_KEY", b"sk-value\n".to_vec()),
            ("S3_ACCESS_KEY_ID", b"ak-value".to_vec()),
            ("BINARY", vec![0xff, 0xfe]),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let out = secret_bytes_to_strings(raw);
        assert_eq!(
            out.get("S3_SECRET_ACCESS_KEY").map(String::as_str),
            Some("sk-value")
        );
        assert_eq!(
            out.get("S3_ACCESS_KEY_ID").map(String::as_str),
            Some("ak-value")
        );
        // Non-UTF-8 is dropped, not lossily mangled into a plausible-looking
        // credential that fails to sign.
        assert!(!out.contains_key("BINARY"), "{out:?}");
    }

    #[test]
    fn the_readback_guard_refuses_when_the_cluster_silently_dropped_the_zone() {
        // THE 2.22g defence. `spec.backup` is fully structural, so an
        // apiserver whose CRD predates `timeZone` answers HTTP 200 and prunes
        // the field. Nothing else in the write path can see that: kubectl's
        // pruning warning goes to stderr, which the merge-patch helper reads
        // only on failure. A mismatch MUST be an error — half-succeeding
        // leaves backups genuinely enabled in the wrong zone, with this CLI
        // reporting the zone it thought it set.
        let dropped = check_time_zone_readback("Europe/Berlin", None).unwrap_err();
        let msg = format!("{dropped}");
        assert!(
            msg.contains("Europe/Berlin"),
            "names the zone we asked for: {msg}"
        );
        assert!(msg.contains("predates"), "explains the cause: {msg}");
        assert!(
            msg.contains("Upgrade the platform"),
            "says what to do: {msg}"
        );

        // Stored, but as something else — equally a refusal.
        assert!(check_time_zone_readback("Europe/Berlin", Some("UTC")).is_err());

        // Stored as asked → proceed.
        assert!(check_time_zone_readback("Europe/Berlin", Some("Europe/Berlin")).is_ok());

        // No zone was asked for → there is nothing to verify.
        assert!(check_time_zone_readback("", None).is_ok());
    }

    #[test]
    fn the_readback_guard_reads_the_very_field_the_enable_patch_writes() {
        // The guard is only a guard if its reader and the patch builder agree
        // on the path. Round-trip the real patch through the real reader:
        // either one drifting to a different key makes the guard fire on
        // every healthy cluster (or, worse, never fire at all).
        let patch = backup_enable_patch(
            &EnableOpts {
                bucket: "s3:b".into(),
                credential: "c".into(),
                ..Default::default()
            },
            &ResolvedSchedule {
                schedule: "0 3 * * *".into(),
                check_schedule: "0 6 * * 0".into(),
                time_zone: "Asia/Tokyo".into(),
            },
        );
        assert_eq!(
            stored_time_zone(Some(&patch)).as_deref(),
            Some("Asia/Tokyo")
        );
        assert!(
            check_time_zone_readback("Asia/Tokyo", stored_time_zone(Some(&patch)).as_deref())
                .is_ok()
        );

        // A patch built with no zone stores no field.
        let zoneless = backup_enable_patch(
            &EnableOpts {
                bucket: "s3:b".into(),
                credential: "c".into(),
                ..Default::default()
            },
            &ResolvedSchedule {
                schedule: "0 3 * * *".into(),
                check_schedule: String::new(),
                time_zone: String::new(),
            },
        );
        assert_eq!(stored_time_zone(Some(&zoneless)), None);
        assert_eq!(stored_time_zone(None), None);
    }

    #[test]
    fn enable_patch_carries_every_retention_key_that_was_set() {
        let p = backup_enable_patch(
            &EnableOpts {
                bucket: "s3:x".into(),
                credential: "c".into(),
                keep_daily: Some(1),
                keep_weekly: Some(2),
                keep_monthly: Some(3),
                enforce: Some("operator".into()),
                ..Default::default()
            },
            &ResolvedSchedule {
                schedule: "0 3 * * *".into(),
                check_schedule: "0 6 * * 0".into(),
                time_zone: "UTC".into(),
            },
        );
        let ret = &p["spec"]["backup"]["retention"];
        assert_eq!(ret["keepDaily"], json!(1));
        assert_eq!(ret["keepWeekly"], json!(2));
        assert_eq!(ret["keepMonthly"], json!(3));
        assert_eq!(ret["enforce"], json!("operator"));
    }

    #[test]
    fn the_enable_success_line_states_the_repo_the_credential_and_the_gitops_caveat() {
        let report = enable_success_report(
            "s3:https://nbg1.example/bucket",
            "apprafter-backup-s3",
            &ResolvedSchedule {
                schedule: "0 3 * * *".into(),
                check_schedule: "0 6 * * 0".into(),
                time_zone: "Europe/Berlin".into(),
            },
        );
        assert!(
            report.contains("s3:https://nbg1.example/bucket"),
            "{report}"
        );
        assert!(report.contains("apprafter-backup-s3"), "{report}");
        assert!(report.contains("backup daily at 03:00"), "{report}");
        assert!(report.contains("Europe/Berlin"), "{report}");
        // A live merge-patch is not durable if the field is git-managed —
        // omitting this is how somebody's backup config silently reverts on
        // the next Argo sync.
        assert!(report.contains("Argo CD"), "{report}");
    }

    // ------------------------------------------------------------------
    // `backup list` — snapshot table rendering
    // ------------------------------------------------------------------

    #[test]
    fn the_snapshot_table_truncates_a_full_id_when_restic_omits_short_id() {
        let full = "0123456789abcdef0123456789abcdef";
        let table = format_snapshot_table(
            "s3:https://h/b",
            &[json!({"id": full, "time": "2026-08-01T03:00:00Z", "tags": ["a", "b"]})],
        );
        assert!(table.contains("01234567"), "{table}");
        assert!(
            !table.contains(full),
            "a 32-hex id in a 12-wide column wrecks the table: {table}"
        );
        assert!(table.contains("a, b"), "tags are joined: {table}");
        assert!(table.contains("2026-08-01T03:00:00Z"), "{table}");
    }

    #[test]
    fn the_snapshot_table_prefers_short_id_and_tolerates_a_bare_snapshot() {
        let table = format_snapshot_table(
            "s3:x",
            &[
                json!({"short_id": "deadbeef", "id": "ffffffffffff"}),
                json!({}),
            ],
        );
        assert!(table.contains("deadbeef"), "{table}");
        assert!(!table.contains("ffffffff"), "short_id wins: {table}");
        // A snapshot with neither id nor time still renders a row rather than
        // aborting the listing.
        assert!(table.contains('?'), "{table}");
    }

    #[test]
    fn an_empty_repo_says_so_instead_of_printing_an_empty_table() {
        let table = format_snapshot_table("s3:https://h/b", &[]);
        assert!(table.contains("No snapshots in s3:https://h/b"), "{table}");
        assert!(
            !table.contains("TAGS"),
            "a header with no rows reads as a broken listing: {table}"
        );
    }

    #[test]
    fn snapshot_json_that_is_not_a_list_is_an_empty_list_but_garbage_is_an_error() {
        assert_eq!(parse_snapshots_json("[]").unwrap().len(), 0);
        assert_eq!(
            parse_snapshots_json(r#"[{"short_id":"a"},{"short_id":"b"}]"#)
                .unwrap()
                .len(),
            2
        );
        // restic printing an object rather than an array is "nothing to show",
        // not a crash.
        assert_eq!(parse_snapshots_json("{}").unwrap().len(), 0);
        // But output that is not JSON at all means restic did something we do
        // not understand, and must not be reported as an empty repo.
        let err = parse_snapshots_json("Fatal: unable to open repo").unwrap_err();
        assert!(format!("{err}").contains("parse restic snapshots JSON"));
    }

    // ------------------------------------------------------------------
    // `backup prune` / `backup status` — the last-prune stamp round-trip
    // ------------------------------------------------------------------

    #[test]
    fn the_last_prune_stamp_is_written_under_the_key_status_reads_back() {
        // prune WRITES `apprafter.io/last-prune`; status READS it through the
        // escaped pointer `apprafter.io~1last-prune`. If those two spellings
        // drift, every pruned cluster reports "Last prune: never" and nothing
        // anywhere errors.
        let body = last_prune_patch_body("2026-08-01T04:00:00Z");
        let doc: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            last_prune_annotation(Some(&doc)).as_deref(),
            Some("2026-08-01T04:00:00Z")
        );
        assert_eq!(last_prune_annotation(None), None);
        assert_eq!(last_prune_annotation(Some(&json!({"metadata": {}}))), None);
    }

    #[test]
    fn the_prune_summary_states_the_policy_that_was_applied() {
        let s = prune_summary(
            "s3:https://h/b",
            &RetentionPolicy {
                keep_daily: 1,
                keep_weekly: 2,
                keep_monthly: 3,
            },
        );
        assert!(s.contains("s3:https://h/b"), "{s}");
        assert!(
            s.contains("keepDaily=1 keepWeekly=2 keepMonthly=3"),
            "each number must sit against its own label: {s}"
        );
    }

    #[test]
    fn status_reports_only_apprafter_backup_jobs() {
        let list = json!({"items": [
            {"metadata": {"name": "apprafter-backup-1"}},
            {"metadata": {"name": "apprafter-backup-check-1"}},
            {"metadata": {"name": "some-other-job"}},
        ]});
        let jobs = backup_jobs_of(Some(&list));
        let names: Vec<&str> = jobs.iter().map(job_metadata_name).collect();
        assert_eq!(
            names,
            vec!["apprafter-backup-1", "apprafter-backup-check-1"]
        );
        // No Jobs listing at all (or no items) is "none", not a failure.
        assert!(backup_jobs_of(None).is_empty());
        assert!(backup_jobs_of(Some(&json!({}))).is_empty());
    }

    #[test]
    fn a_job_that_has_started_but_not_finished_is_neither_succeeded_nor_failed() {
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let running = json!({
            "metadata": {"name": "apprafter-backup-running"},
            "status": {"active": 1}
        });
        let s = format_backup_status(Some(&spec), std::slice::from_ref(&running), None, None);
        assert!(s.contains("Running"), "{s}");

        // A Job with no counters at all must not be reported as a success.
        let bare = json!({"metadata": {"name": "apprafter-backup-bare"}, "status": {}});
        let s = format_backup_status(Some(&spec), std::slice::from_ref(&bare), None, None);
        assert!(s.contains("Unknown"), "{s}");
        assert!(!s.contains("Succeeded"), "{s}");
    }

    // ------------------------------------------------------------------
    // `export` / `backup` — manifest, opts and the summary lines
    // ------------------------------------------------------------------

    #[test]
    fn resource_refs_keeps_the_kind_of_each_source_and_the_claim_type() {
        let ps = json!({"metadata": {"name": "default", "namespace": "apprafter-system"}});
        let claim = json!({
            "metadata": {"name": "shop-pg", "namespace": "prod"},
            "spec": {"type": "pg"}
        });
        let refs = resource_refs(&[("PlatformStack", &ps)], std::slice::from_ref(&claim));
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].kind, "PlatformStack");
        assert_eq!(refs[0].name, "default");
        assert_eq!(refs[0].namespace, "apprafter-system");
        // A config CR has no claim type — a restore keys its data-load path
        // off this field, so a spurious one would send it looking for a dump.
        assert_eq!(refs[0].claim_type, None);
        assert_eq!(refs[1].kind, "ResourceClaim");
        assert_eq!(refs[1].name, "shop-pg");
        assert_eq!(refs[1].claim_type.as_deref(), Some("pg"));

        // Missing metadata degrades to empty strings rather than panicking
        // mid-backup.
        let bare = resource_refs(&[], &[json!({})]);
        assert_eq!(bare[0].name, "");
        assert_eq!(bare[0].namespace, "");
        assert_eq!(bare[0].claim_type, None);
    }

    #[test]
    fn the_export_manifest_records_claims_and_no_config_crs() {
        // `export` is Kind 1: native data only. A config CR listed here would
        // advertise replayable cluster config the export never captured.
        let claims = vec![json!({
            "metadata": {"name": "shop-pg", "namespace": "prod"},
            "spec": {"type": "pg"}
        })];
        let m = export_manifest("prod-cluster", "0.2.58", &["prod".to_string()], &claims);
        assert_eq!(m.cluster_id, "prod-cluster");
        assert_eq!(m.platform_version, "0.2.58");
        assert_eq!(m.namespaces, vec!["prod".to_string()]);
        assert_eq!(
            m.manifest_version,
            backup_core::manifest::MANIFEST_VERSION_CURRENT
        );
        assert_eq!(m.resources.len(), 1);
        assert!(
            m.resources.iter().all(|r| r.kind == "ResourceClaim"),
            "export must not claim to carry config CRs: {:?}",
            m.resources
        );
    }

    #[test]
    fn the_manifest_written_to_disk_reads_back_as_the_manifest() {
        // `restore` parses this file. A serialisation that writes fields the
        // reader cannot find turns every backup into an unrestorable one, and
        // nothing before restore-time would notice.
        let dir = tempfile::tempdir().unwrap();
        let m = export_manifest(
            "c1",
            "0.2.58",
            &["prod".to_string(), "demo".to_string()],
            &[json!({"metadata": {"name": "r", "namespace": "prod"}, "spec": {"type": "redis"}})],
        );
        write_manifest(&m, dir.path()).unwrap();

        let raw = std::fs::read(dir.path().join("manifest.json")).expect("manifest.json written");
        let back: BackupManifest = serde_json::from_slice(&raw).unwrap();
        assert_eq!(back.cluster_id, "c1");
        assert_eq!(back.platform_version, "0.2.58");
        assert_eq!(
            back.namespaces,
            vec!["prod".to_string(), "demo".to_string()]
        );
        assert_eq!(back.resources.len(), 1);
        assert_eq!(back.resources[0].claim_type.as_deref(), Some("redis"));
        assert_eq!(back.manifest_version, m.manifest_version);
    }

    #[test]
    fn the_export_directory_defaults_beside_the_operator_not_inside_the_repo() {
        assert_eq!(
            export_out_dir(Some("/srv/dump")),
            PathBuf::from("/srv/dump")
        );
        let default = export_out_dir(None);
        assert_eq!(
            default.file_name().and_then(|s| s.to_str()),
            Some("apprafter-export")
        );
        assert!(default.is_absolute(), "{default:?}");
    }

    #[test]
    fn the_export_summary_counts_namespaces_claims_and_extractables_separately() {
        let s = export_summary(
            "prod-cluster",
            Path::new("/srv/dump"),
            &["demo".to_string(), "prod".to_string()],
            5,
            2,
        );
        assert!(s.contains("2 namespace(s)"), "{s}");
        assert!(s.contains("demo, prod"), "{s}");
        assert!(s.contains("/srv/dump"), "{s}");
        // "5 claims, 2 of them extractable" — swapping these tells the
        // operator more data was captured than was.
        assert!(s.contains("5 (2 extractable)"), "{s}");
    }

    #[test]
    fn the_local_pull_keeps_the_operator_stations_hostname_as_the_restic_group() {
        // spec §Retention M-r3-1a: only the in-cluster runner pins a fixed
        // host (its pod name is ephemeral). Pinning it here would merge every
        // operator's snapshots into one retention group.
        let opts = local_pull_backup_opts(
            "s3:https://h/b",
            "pw".into(),
            "prod-cluster",
            "0.2.58",
            &["prod".to_string()],
            true,
            Path::new("/staging"),
            "postgres:18-alpine".into(),
            StagingMode::Sequential,
        );
        assert_eq!(opts.backup_host, None);
        assert!(opts.is_subset, "--select must reach the tag decoration");
        assert_eq!(opts.repo, "s3:https://h/b");
        assert_eq!(opts.cluster_id, "prod-cluster");
        assert_eq!(opts.platform_version, "0.2.58");
        assert_eq!(opts.namespaces, vec!["prod".to_string()]);
        assert_eq!(opts.staging_root, PathBuf::from("/staging"));
        assert_eq!(opts.pg_image, "postgres:18-alpine");
        assert!(matches!(opts.staging_mode, StagingMode::Sequential));
        assert!(
            chrono::DateTime::parse_from_rfc3339(&opts.created_at).is_ok(),
            "created_at must be RFC3339 — it is the manifest timestamp and the \
             restic tag: {}",
            opts.created_at
        );
    }

    #[test]
    fn the_backup_summary_omits_the_snapshot_line_when_restic_reported_none() {
        let mut summary = backup_core::engine::BackupSummary {
            snapshot_id: None,
            cr_count: 3,
            secret_count: 4,
            claim_count: 5,
            extracted_count: 2,
            tag: "apprafter/prod-cluster/2026-08-01".into(),
        };
        let none = backup_summary_report(
            "prod-cluster",
            "s3:https://h/b",
            &["prod".to_string()],
            &summary,
        );
        assert!(
            none.contains("3 CR(s), 4 secret(s), 5 claim(s) (2 extracted)"),
            "{none}"
        );
        assert!(none.contains("apprafter/prod-cluster/2026-08-01"), "{none}");
        assert!(
            !none.contains("snapshot:"),
            "an empty snapshot id reads as a stored snapshot that does not exist: {none}"
        );

        summary.snapshot_id = Some("abc123".into());
        let some = backup_summary_report(
            "prod-cluster",
            "s3:https://h/b",
            &["prod".to_string()],
            &summary,
        );
        assert!(some.contains("snapshot:   abc123"), "{some}");
    }

    #[test]
    fn nothing_to_back_up_names_the_action_and_where_the_scope_came_from() {
        // The scope is the app-namespace set, NOT `kubectl get ns`. An
        // operator looking at a cluster full of namespaces has to be told
        // that is deliberate.
        let export = format!("{}", no_applications_error("export"));
        assert!(export.contains("nothing to export"), "{export}");
        let backup = format!("{}", no_applications_error("back up"));
        assert!(backup.contains("nothing to back up"), "{backup}");
        for msg in [&export, &backup] {
            assert!(msg.contains("applications.apprafter.io -A"), "{msg}");
        }
    }

    #[test]
    fn the_default_backup_repo_is_per_target() {
        assert_eq!(
            backup_repo_path(Some("/srv/repo"), "ignored").unwrap(),
            PathBuf::from("/srv/repo")
        );
        let alpha = backup_repo_path(None, "alpha").unwrap();
        let beta = backup_repo_path(None, "beta").unwrap();
        assert_eq!(alpha.file_name().and_then(|s| s.to_str()), Some("alpha"));
        assert_eq!(
            alpha
                .parent()
                .and_then(Path::file_name)
                .and_then(|s| s.to_str()),
            Some("backups")
        );
        // Two targets sharing one repo would interleave their snapshots and
        // each other's retention.
        assert_ne!(alpha, beta);
    }

    // ------------------------------------------------------------------
    // Small pure readers used by the impure fetchers
    // ------------------------------------------------------------------

    #[test]
    fn the_platform_version_falls_back_to_the_literal_unknown() {
        assert_eq!(
            platform_version_of(Some(&json!({"status": {"currentVersion": "0.2.58"}}))),
            "0.2.58"
        );
        // A freshly bootstrapped cluster has no stamped status yet.
        // `restore --reprovision` treats "unknown" as "no version to pin";
        // an empty string would be passed on AS a version.
        assert_eq!(platform_version_of(Some(&json!({}))), "unknown");
        assert_eq!(platform_version_of(None), "unknown");
    }

    #[test]
    fn only_a_missing_kind_is_swallowed_into_an_empty_listing() {
        // `infrastructures` legitimately has no instances at M2, so that one
        // error becomes an empty list. Widening this would turn a connection
        // failure mid-backup into a silently empty, restorable-LOOKING backup.
        assert!(is_missing_resource_kind(&CliError::Other(
            "error: the server doesn't have a resource type \"infrastructures\"".into()
        )));
        assert!(!is_missing_resource_kind(&CliError::Other(
            "The connection to the server 10.0.0.1:6443 was refused".into()
        )));
        assert!(!is_missing_resource_kind(&CliError::Other(
            "Error from server (Forbidden): applications.apprafter.io is forbidden".into()
        )));
    }

    #[test]
    fn items_of_reads_the_list_body_and_tolerates_its_absence() {
        assert_eq!(items_of(&json!({"items": [1, 2, 3]})).len(), 3);
        assert!(items_of(&json!({"items": null})).is_empty());
        assert!(items_of(&json!({})).is_empty());
    }

    #[test]
    fn the_cnpg_scan_always_includes_cnpg_system_exactly_once() {
        // The shared integrated `platform-postgres` Cluster lives ONLY in
        // cnpg-system. An app-ns-only scan structurally misses it, falls back
        // to the default pg major, and every integrated-tier dump dies with
        // `pg_dump: server version mismatch`.
        assert_eq!(
            cnpg_scan_namespaces(&["demo".to_string(), "prod".to_string()]),
            vec!["demo", "prod", "cnpg-system"]
        );
        assert_eq!(cnpg_scan_namespaces(&[]), vec!["cnpg-system"]);
        // Already an app namespace → not scanned twice.
        assert_eq!(
            cnpg_scan_namespaces(&["cnpg-system".to_string()]),
            vec!["cnpg-system"]
        );
    }

    #[test]
    fn a_secret_document_decodes_every_key_and_defaults_its_type() {
        let json = json!({
            "type": "kubernetes.io/tls",
            "data": {"tls.crt": "aGVsbG8=", "tls.key": "d29ybGQ="}
        });
        let (data, kind) = decode_secret_json(&json, "s", "ns").unwrap();
        assert_eq!(kind, "kubernetes.io/tls");
        assert_eq!(data.get("tls.crt").map(Vec::as_slice), Some(&b"hello"[..]));
        assert_eq!(data.get("tls.key").map(Vec::as_slice), Some(&b"world"[..]));

        // A Secret with no explicit type is Opaque — the value the sealing
        // path round-trips.
        let (data, kind) = decode_secret_json(&json!({"data": {}}), "s", "ns").unwrap();
        assert_eq!(kind, "Opaque");
        assert!(data.is_empty());

        // Undecodable material must be an error, not a silently empty value:
        // an empty credential fails far away, against a bucket.
        let err =
            decode_secret_json(&json!({"data": {"k": "!!not base64!!"}}), "s", "ns").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("ns/s"), "names the secret: {msg}");
        assert!(msg.contains('k'), "names the key: {msg}");
    }

    #[test]
    fn an_empty_passphrase_is_refused_as_loudly_as_a_missing_one() {
        // The repository holds DECRYPTED secrets. An empty passphrase is not
        // "no encryption configured", it is a repo anyone can open.
        let msg = format!(
            "{}",
            backup_passphrase_or_error(Some(""), None, false).unwrap_err()
        );
        assert!(msg.contains("empty backup passphrase"), "{msg}");
        assert!(
            format!(
                "{}",
                backup_passphrase_or_error(None, Some(""), false).unwrap_err()
            )
            .contains("empty"),
            "an empty RESTIC_PASSWORD is just as unencrypted"
        );
        // Non-interactive with nothing set names both non-interactive inputs.
        let msg = format!(
            "{}",
            backup_passphrase_or_error(None, None, false).unwrap_err()
        );
        assert!(
            msg.contains("--passphrase") && msg.contains("RESTIC_PASSWORD"),
            "{msg}"
        );
        // The flag beats the environment.
        assert_eq!(
            backup_passphrase_or_error(Some("flag"), Some("env"), false).unwrap(),
            "flag"
        );
    }

    #[test]
    fn a_fully_specified_maintenance_verb_never_reaches_for_a_kubeconfig() {
        // The DR case: the cluster is SUPPOSED to be gone. Asking for a
        // kubeconfig here is what made `backup check` unusable after
        // `apprafter destroy` (v0.2.48). `Ok(None)` is the proof it did not
        // even try — resolving one would fail in this test environment.
        assert!(
            kubeconfig_if_cluster_needed("check", Some("s3:x"), RetentionArgs::NotApplicable)
                .unwrap()
                .is_none()
        );
        assert!(kubeconfig_if_cluster_needed(
            "prune",
            Some("s3:x"),
            prune_keeps(Some(7), Some(4), Some(6))
        )
        .unwrap()
        .is_none());
    }

    // ------------------------------------------------------------------
    // restic invocation: creds on the child, failure text, snapshot id
    // ------------------------------------------------------------------

    #[test]
    fn the_restic_child_gets_aws_names_and_the_explicit_passphrase_wins() {
        let creds: BTreeMap<String, String> = [
            ("S3_ACCESS_KEY_ID", "AKID"),
            ("S3_SECRET_ACCESS_KEY", "SKEY"),
            ("S3_REGION", "eu-central-1"),
            ("RESTIC_PASSWORD", "from-creds"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let r = CredentialedRestic { creds };
        let cmd = r.command(&["check".to_string(), "-r".to_string()], "from-argument");

        let env: BTreeMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert_eq!(
            env.get("AWS_ACCESS_KEY_ID").map(String::as_str),
            Some("AKID")
        );
        assert_eq!(
            env.get("AWS_SECRET_ACCESS_KEY").map(String::as_str),
            Some("SKEY")
        );
        assert_eq!(
            env.get("AWS_DEFAULT_REGION").map(String::as_str),
            Some("eu-central-1")
        );
        // The trait contract passes the passphrase explicitly; it must be
        // applied AFTER the credential map, so the caller's value is the one
        // restic sees.
        assert_eq!(
            env.get("RESTIC_PASSWORD").map(String::as_str),
            Some("from-argument")
        );
        assert_eq!(cmd.get_program(), "restic");
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv, vec!["check".to_string(), "-r".to_string()]);
    }

    #[test]
    fn a_failed_restic_run_names_the_subcommand_and_carries_its_stderr() {
        // restic's stderr is the ONLY place the actual cause is named, and an
        // operator doing disaster recovery has nothing else to go on.
        let e = restic_failure_error(
            &["check".to_string(), "-r".to_string(), "s3:x".to_string()],
            Some(1),
            b"Fatal: wrong password or no key found",
        );
        let msg = format!("{e}");
        assert!(msg.contains("restic check"), "{msg}");
        assert!(msg.contains("wrong password or no key found"), "{msg}");
        assert!(msg.contains('1'), "the exit code is stated: {msg}");
        // An empty argv must not panic on the way to reporting a failure.
        assert!(format!("{}", restic_failure_error(&[], None, b"")).contains("restic ?"));
    }

    #[test]
    fn the_snapshot_id_comes_from_the_summary_line_and_nowhere_else() {
        // restic streams `status` lines during a backup. Taking the first id
        // in the stream reports a snapshot that is not the one just written.
        let stream = concat!(
            r#"{"message_type":"status","percent_done":0.5,"snapshot_id":"WRONG"}"#,
            "\n",
            "not json at all\n",
            r#"{"message_type":"summary","snapshot_id":"RIGHT"}"#,
            "\n"
        );
        assert_eq!(
            snapshot_id_from_backup_json(stream).as_deref(),
            Some("RIGHT")
        );
        // No summary line (restic died mid-run) → no snapshot to report.
        assert_eq!(
            snapshot_id_from_backup_json(r#"{"message_type":"status"}"#),
            None
        );
        assert_eq!(snapshot_id_from_backup_json(""), None);
    }

    #[test]
    fn the_restic_version_gate_fails_low_but_passes_an_unreadable_version() {
        let err = format!("{}", restic_version_gate("restic 0.13.0").unwrap_err());
        assert!(err.contains("0.14"), "names the requirement: {err}");
        assert!(err.contains("0.13.0"), "names what was found: {err}");
        assert!(restic_version_gate("restic 0.16.4 compiled with go1.21.6").is_ok());
        // A future restic that reworks its version line must not make
        // `backup enable` impossible on a perfectly good binary.
        assert!(restic_version_gate("restic (nightly build)").is_ok());
    }

    #[test]
    fn an_unreachable_repo_reports_both_restic_attempts() {
        // `cat config` says "repository does not exist"; `init` says the real
        // obstacle. Dropping either leaves the operator guessing which of the
        // two problems they have.
        let e = repo_unreachable_error(
            "s3:https://h/b",
            b"Fatal: repository does not exist\n",
            b"Fatal: Access Denied\n",
        );
        let msg = format!("{e}");
        assert!(msg.contains("s3:https://h/b"), "{msg}");
        assert!(msg.contains("repository does not exist"), "{msg}");
        assert!(msg.contains("Access Denied"), "{msg}");
    }

    // ------------------------------------------------------------------
    // KubectlExec — driven against a stub binary so the subprocess layer
    // (streams, exit statuses, stderr capture) is actually exercised.
    // ------------------------------------------------------------------

    /// Write an executable `/bin/sh` stub and hand back a [`KubectlExec`]
    /// pointed at it. The `TempDir` must outlive the returned exec.
    ///
    /// The `__probe` prologue plus the retry loop exist only to drain the
    /// `ETXTBSY` window: a sibling test thread that forks while this file is
    /// still open for writing leaves the new process holding a write handle to
    /// it, and `execve` refuses until that handle is gone. Once a probe
    /// succeeds nothing writes this inode again, so every later spawn is safe.
    fn stub_kubectl(dir: &tempfile::TempDir, body: &str) -> KubectlExec {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.path().join("kubectl-stub");
        std::fs::write(
            &path,
            format!("#!/bin/sh\ncase \"$1\" in __probe) exit 0;; esac\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match Command::new(&path).arg("__probe").status() {
                Err(e) if e.raw_os_error() == Some(26) => {
                    thread::sleep(Duration::from_millis(5));
                }
                _ => break,
            }
        }
        KubectlExec {
            kubeconfig: dir.path().join("kubeconfig.yaml"),
            kubectl_bin: path,
        }
    }

    #[test]
    fn exec_stream_to_file_writes_the_pods_stdout_to_the_target_path() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        let k = stub_kubectl(
            &dir,
            &format!(
                "echo \"$@\" > {log}\necho \"KUBECONFIG=$KUBECONFIG\" >> {log}\nprintf 'DUMPBYTES'",
                log = log.display()
            ),
        );
        let out = dir.path().join("dump.sql");
        k.exec_stream_to_file("pg-0", "prod", &["pg_dump", "-Fc"], &out)
            .unwrap();

        assert_eq!(std::fs::read_to_string(&out).unwrap(), "DUMPBYTES");
        let argv = std::fs::read_to_string(&log).unwrap();
        // `--` separates kubectl's own flags from the in-pod command; without
        // it kubectl parses `-Fc` as its own.
        assert!(argv.contains("exec pg-0 -n prod -- pg_dump -Fc"), "{argv}");
        assert!(
            argv.contains(&format!("KUBECONFIG={}", k.kubeconfig.display())),
            "the child must target the caller's cluster: {argv}"
        );
    }

    #[test]
    fn a_failed_exec_surfaces_the_last_stderr_lines_and_never_the_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let k = stub_kubectl(&dir, "echo 'pg_dump: server version mismatch' >&2\nexit 7");
        let err = k
            .exec_stream_to_file("pg-0", "prod", &["pg_dump"], &dir.path().join("out"))
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("exec_stream_to_file"), "{msg}");
        assert!(msg.contains('7'), "the exit status is stated: {msg}");
        assert!(
            msg.contains("pg_dump: server version mismatch"),
            "the pod's own error is the only useful part: {msg}"
        );
    }

    #[test]
    fn exec_stream_from_file_feeds_the_file_on_the_childs_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let sink = dir.path().join("received");
        let log = dir.path().join("argv");
        let k = stub_kubectl(
            &dir,
            &format!(
                "echo \"$@\" > {log}\ncat > {sink}",
                log = log.display(),
                sink = sink.display()
            ),
        );
        let input = dir.path().join("restore.sql");
        std::fs::write(&input, "RESTORE PAYLOAD").unwrap();
        k.exec_stream_from_file("pg-0", "prod", &["psql"], &input)
            .unwrap();

        assert_eq!(std::fs::read_to_string(&sink).unwrap(), "RESTORE PAYLOAD");
        // `-i` is what keeps stdin attached; without it the payload is
        // written into a closed pipe and the load silently restores nothing.
        let argv = std::fs::read_to_string(&log).unwrap();
        assert!(argv.contains("exec -i pg-0 -n prod -- psql"), "{argv}");
    }

    #[test]
    fn a_consumer_that_stops_reading_early_is_not_a_restore_failure() {
        // `psql` legitimately exits 0 on a `\q` before EOF. The resulting
        // EPIPE on our side is not an error — reporting one would fail a
        // restore that actually succeeded.
        let dir = tempfile::tempdir().unwrap();
        let k = stub_kubectl(&dir, "head -c 1 >/dev/null\nexit 0");
        let input = dir.path().join("big.sql");
        std::fs::write(&input, "x".repeat(4 * 1024 * 1024)).unwrap();
        k.exec_stream_from_file("pg-0", "prod", &["psql"], &input)
            .expect("an early-closing consumer that exits 0 is a success");
    }

    #[test]
    fn apply_and_wait_pod_ready_pipes_the_spec_in_and_then_waits_for_ready() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        let seen = dir.path().join("spec.json");
        let k = stub_kubectl(
            &dir,
            &format!(
                "echo \"$@\" >> {log}\nif [ \"$1\" = apply ]; then cat > {seen}; fi\nexit 0",
                log = log.display(),
                seen = seen.display()
            ),
        );
        let spec = json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "helper", "namespace": "prod"},
            "spec": {"containers": [{"name": "c", "image": "postgres:18-alpine"}]}
        });
        k.apply_and_wait_pod_ready(&spec).unwrap();

        // The spec really reached kubectl's stdin, unmodified.
        let piped: Value = serde_json::from_slice(&std::fs::read(&seen).unwrap()).unwrap();
        assert_eq!(piped, spec);

        let argv = std::fs::read_to_string(&log).unwrap();
        assert!(argv.contains("apply -f - -n prod"), "{argv}");
        // Readiness, not existence: a Pod that exists but is not Ready cannot
        // be exec'd into, which is the only reason this helper is created.
        assert!(
            argv.contains("wait --for=condition=Ready pod/helper -n prod --timeout=300s"),
            "{argv}"
        );
    }

    #[test]
    fn a_pod_spec_without_an_identity_is_refused_before_kubectl_is_spawned() {
        let dir = tempfile::tempdir().unwrap();
        // The stub always succeeds — so if these checks were dropped, the
        // calls below would wrongly return Ok.
        let k = stub_kubectl(&dir, "exit 0");
        let no_name = k
            .apply_and_wait_pod_ready(&json!({"metadata": {"namespace": "prod"}}))
            .unwrap_err();
        assert!(format!("{no_name}").contains("metadata.name"), "{no_name}");
        let no_ns = k
            .apply_and_wait_pod_ready(&json!({"metadata": {"name": "helper"}}))
            .unwrap_err();
        assert!(format!("{no_ns}").contains("metadata.namespace"), "{no_ns}");
    }

    #[test]
    fn a_pod_that_never_becomes_ready_is_reported_as_a_timeout_not_an_apply_failure() {
        let dir = tempfile::tempdir().unwrap();
        let spec = json!({"metadata": {"name": "helper", "namespace": "prod"}});

        // apply succeeds, wait fails → the message must be about readiness.
        let waits = stub_kubectl(&dir, "if [ \"$1\" = wait ]; then exit 1; fi\nexit 0");
        let err = waits.apply_and_wait_pod_ready(&spec).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("did not reach Ready within 300s"), "{msg}");
        assert!(msg.contains("helper") && msg.contains("prod"), "{msg}");

        // apply itself fails → the apiserver's own complaint is carried.
        let dir2 = tempfile::tempdir().unwrap();
        let applies = stub_kubectl(
            &dir2,
            "cat >/dev/null\necho 'error: forbidden: pods is forbidden' >&2\nexit 1",
        );
        let err = applies.apply_and_wait_pod_ready(&spec).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("apply_and_wait_pod_ready(apply)"), "{msg}");
        assert!(msg.contains("pods is forbidden"), "{msg}");
    }

    #[test]
    fn get_secret_key_base64_decodes_the_jsonpath_output() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        // `echo` appends a newline, exactly as a shell pipeline would; the
        // decoder must trim it or base64 rejects the whole value.
        let k = stub_kubectl(
            &dir,
            &format!(
                "echo \"$@\" > {log}\necho aGVsbG8gd29ybGQ=",
                log = log.display()
            ),
        );
        assert_eq!(
            k.get_secret_key("pg-app", "prod", "password").unwrap(),
            "hello world"
        );
        let argv = std::fs::read_to_string(&log).unwrap();
        assert!(
            argv.contains("get secret pg-app -n prod -o jsonpath={.data.password}"),
            "{argv}"
        );
    }

    #[test]
    fn get_secret_key_reports_a_non_base64_value_rather_than_returning_junk() {
        let dir = tempfile::tempdir().unwrap();
        let k = stub_kubectl(&dir, "echo 'this is not base64!!'");
        let msg = format!("{}", k.get_secret_key("s", "ns", "k").unwrap_err());
        assert!(msg.contains("not valid base64"), "{msg}");

        let dir2 = tempfile::tempdir().unwrap();
        let failing = stub_kubectl(&dir2, "echo 'Error from server (NotFound)' >&2\nexit 1");
        let msg = format!("{}", failing.get_secret_key("s", "ns", "k").unwrap_err());
        assert!(msg.contains("kubectl get secret s -n ns"), "{msg}");
        assert!(msg.contains("NotFound"), "carries kubectl's stderr: {msg}");
    }

    #[test]
    fn get_json_treats_notfound_as_absence_and_everything_else_as_failure() {
        // The distinction the whole backup sweep rests on: a Secret that does
        // not exist is a skipped item; an unreachable apiserver is a failed
        // backup. Collapsing them yields a backup missing whatever the
        // network dropped.
        let dir = tempfile::tempdir().unwrap();
        let missing = stub_kubectl(
            &dir,
            "echo 'Error from server (NotFound): secrets \"x\" not found' >&2\nexit 1",
        );
        assert_eq!(missing.get_json(&["get", "secret", "x"]).unwrap(), None);

        let dir2 = tempfile::tempdir().unwrap();
        let unreachable = stub_kubectl(
            &dir2,
            "echo 'The connection to the server was refused' >&2\nexit 1",
        );
        let msg = format!(
            "{}",
            unreachable.get_json(&["get", "secret", "x"]).unwrap_err()
        );
        assert!(
            msg.contains("connection to the server was refused"),
            "{msg}"
        );

        let dir3 = tempfile::tempdir().unwrap();
        let ok = stub_kubectl(&dir3, r#"echo '{"items":[{"a":1}]}'"#);
        assert_eq!(
            ok.get_json(&["get", "pods"]).unwrap(),
            Some(json!({"items": [{"a": 1}]}))
        );

        let dir4 = tempfile::tempdir().unwrap();
        let garbage = stub_kubectl(&dir4, "echo 'not json'");
        let msg = format!("{}", garbage.get_json(&["get", "pods"]).unwrap_err());
        assert!(msg.contains("kubectl JSON parse"), "{msg}");
    }

    #[test]
    fn deleting_a_helper_pod_does_not_wait_and_does_not_fail_the_run() {
        // This runs in the cleanup path of a backup that already produced its
        // data. Blocking on termination would add a minute per claim; failing
        // would discard a good backup over a leftover Pod.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        let k = stub_kubectl(
            &dir,
            &format!("echo \"$@\" > {log}\nexit 3", log = log.display()),
        );
        k.delete_pod_best_effort("helper", "prod");
        let argv = std::fs::read_to_string(&log).unwrap();
        assert!(
            argv.contains("delete pod helper -n prod --ignore-not-found --wait=false"),
            "{argv}"
        );
    }

    #[test]
    fn the_stderr_capture_keeps_the_last_lines_and_says_so_when_there_were_none() {
        // A pod that fails after logging thousands of lines must still report
        // the END of its output — the last lines are where the error is.
        let noisy: String = (1..=30).map(|i| format!("line{i}\n")).collect();
        let buf = spawn_capturing_drainer(io::Cursor::new(noisy.into_bytes()));
        let failed = Command::new("/bin/sh")
            .args(["-c", "exit 4"])
            .status()
            .unwrap();
        let msg = format!("{}", format_exec_error("ctx", failed, &buf));
        assert!(msg.contains("ctx"), "{msg}");
        assert!(msg.contains("line30"), "the tail must survive: {msg}");
        assert!(msg.contains("line11"), "{msg}");
        assert!(
            !msg.contains("line10"),
            "the buffer is bounded at {STDERR_CAPTURE_LIMIT} lines: {msg}"
        );

        // No stderr at all is its own diagnosis — "it failed and said nothing"
        // is different from "it failed and we lost the message".
        let empty = spawn_capturing_drainer(io::Cursor::new(Vec::new()));
        let msg = format!("{}", format_exec_error("ctx", failed, &empty));
        assert!(msg.contains("produced no stderr output"), "{msg}");
    }

    #[test]
    fn status_check_job_is_separated_from_backup_job() {
        let backup_job = json!({
            "metadata": {"name": "apprafter-backup-28900000"},
            "status": {"succeeded": 1}
        });
        let check_job = json!({
            "metadata": {"name": "apprafter-backup-check-28900000"},
            "status": {"failed": 1}
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(Some(&spec), &[backup_job, check_job], None, None);
        assert!(s.contains("apprafter-backup-28900000"));
        assert!(s.contains("apprafter-backup-check-28900000"));
        // backup is Succeeded, check is Failed
        assert!(s.contains("Succeeded"));
        assert!(s.contains("Failed"));
    }
}
