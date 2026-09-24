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

use backup_core::cluster::{
    classify_snapshot, identity_read_error, SnapshotOrigin, IDENTITY_NAMESPACE,
};
use backup_core::engine::{resource_refs, BackupOpts};
use backup_core::extract::{claims_without_data_capture, plan_extraction, UncapturedClaim};
use backup_core::prune::{run_prune, RetentionPolicy};
use backup_core::restic::{
    restic_check_argv, restic_dump_argv, restic_ls_argv, restic_stats_argv, restic_unlock_argv,
};
use backup_core::restore::{resolve_latest_snapshot, UnfinishedRun};
use backup_core::{KubeExec, ResticRunner, StagingMode, SubprocessRestic};
use base64::Engine as _;
use cli_core::diagnose::{classify_restic, ResticFailure};
use cli_core::tools::{preflight_tools, KUBECTL, RESTIC};
use cli_core::{CliError, Result};
use cli_providers::backup::extract::run_extraction;
use cli_providers::backup::images::pg_helper_image;
use cli_providers::backup::manifest::BackupManifest;
use cli_providers::backup::restic::restic_snapshots_argv;
use cli_providers::k8s::kubectl::KubectlCli;
use cli_providers::k8s::sealing::{build_sealed_secret, fetch_controller_public_key};
use serde_json::Value;
use tempfile::NamedTempFile;

use crate::commands::helper_interrupt;
use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_apply_server_side, kubectl_delete, kubectl_get_json,
    kubectl_get_json_cluster_wide, kubectl_merge_patch,
};
use crate::commands::state_paths::resolve_state_paths;

mod job_pod;
use job_pod::{grace_for, job_pod, unplaced, JobPod, Unplaced, UnschedulableClock};

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
    /// `--cluster-name` → `spec.backup.clusterName`: the HUMAN label this
    /// cluster's snapshots carry as their restic `--host`. `None` resolves to
    /// the target name ([`resolve_cluster_name`]).
    ///
    /// Legibility, not identity: it is replayed by restore, so a clone
    /// inherits it. Every repository filter keys on the `kube-system` UID,
    /// which a clone cannot inherit.
    pub cluster_name: Option<String>,
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
    // Present on every patch the command builds: `run_backup_enable` resolves
    // this to the target name before calling. `None` reaches here only from a
    // test constructing `EnableOpts` directly, and an omitted `clusterName`
    // then leaves the snapshots under the anonymous `apprafter-backup` host
    // that made two clusters indistinguishable in a listing.
    if let Some(name) = &o.cluster_name {
        backup.insert("clusterName".to_string(), Value::String(name.clone()));
    }

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

/// This cluster's MACHINE key: the `kube-system` namespace UID (E1).
///
/// The de-facto standard cluster identifier — it exists on every cluster
/// including ones older than this code, it needs no generated state, and a
/// restored cluster is a different Kubernetes cluster with a different UID, so
/// a clone cannot inherit it. It leads every restic run tag and is the only
/// thing repository-wide filters key on.
///
/// Hard error rather than a fallback: an unidentified snapshot in a shared
/// repository is precisely the defect, and "the operator's kubeconfig cannot
/// read `kube-system`" is not a condition to paper over. Callers that only
/// WANT the identity (the listing) discard the error; callers that need it to
/// decide something (backup, prune, restore) propagate it.
///
/// Spawned with an explicit `--request-timeout` rather than through
/// [`kubectl_get_json`]. `backup list --local` and `backup list --repo` reached
/// no cluster at all before this read existed, and an unreachable apiserver
/// makes an unbounded `kubectl get` sit on TCP retries for minutes — in exactly
/// the disaster-recovery case where a repository has to be listed with its
/// cluster gone. A bounded wait keeps the attribution and keeps that fast.
pub(crate) fn read_cluster_uid(kubeconfig: &Path) -> Result<String> {
    helper_interrupt::refuse_if_interrupted()?;
    let out = Command::new("kubectl")
        .args([
            "get",
            "namespace",
            IDENTITY_NAMESPACE,
            "-o",
            "json",
            IDENTITY_REQUEST_TIMEOUT_ARG,
        ])
        .env("KUBECONFIG", kubeconfig)
        .output()
        .map_err(|e| CliError::Other(identity_read_error(&format!("spawn kubectl: {e}"))))?;
    if !out.status.success() {
        return Err(CliError::Other(identity_read_error(
            String::from_utf8_lossy(&out.stderr).trim(),
        )));
    }
    let ns: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| CliError::Other(identity_read_error(&format!("kubectl JSON parse: {e}"))))?;
    backup_core::cluster::cluster_uid_of(&ns).ok_or_else(|| {
        CliError::Other(identity_read_error("the namespace carries no metadata.uid"))
    })
}

/// How long the identity read waits on the apiserver. Generous for a live
/// cluster, bounded for a dead one — see [`read_cluster_uid`].
const IDENTITY_REQUEST_TIMEOUT_ARG: &str = "--request-timeout=10s";

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
///
/// Once the process has been interrupted (Ctrl-C or SIGTERM, with the
/// handler of [`helper_interrupt`] installed) every method refuses at once
/// and spawns nothing: the interrupt's own cleanup deletes the helper pods
/// the command created, and it alone does.
pub(crate) struct KubectlExec {
    pub kubeconfig: PathBuf,
    /// The `kubectl` binary to spawn. Always `"kubectl"` in production (see
    /// [`KubectlExec::new`]); a seam so the tests can drive these methods
    /// against a stub binary and actually observe what they do with a child
    /// process's streams and exit status, rather than leaving the whole
    /// subprocess layer unexercised.
    kubectl_bin: PathBuf,
    /// The helper pods applied and not yet deleted, for the interrupt to
    /// delete ([`helper_interrupt::HelperPods`]): the process-wide set in
    /// production, a private one in the tests.
    helpers: helper_interrupt::HelperPods,
}

impl KubectlExec {
    pub(crate) fn new(kubeconfig: PathBuf) -> Self {
        Self {
            kubeconfig,
            kubectl_bin: PathBuf::from(KUBECTL_BIN),
            helpers: helper_interrupt::HelperPods::global(),
        }
    }

    /// Refuse the call once the process has been interrupted (see the type's
    /// docs): the signal handler's flag, set the moment the signal arrives,
    /// or the stop having closed the set of helper pods.
    fn refuse_if_interrupted(&self) -> Result<()> {
        if self.interrupted() {
            return Err(helper_interrupt::interrupted_error());
        }
        Ok(())
    }

    fn interrupted(&self) -> bool {
        helper_interrupt::interrupted() || self.helpers.is_closed()
    }
}

/// How often the Ready wait reads a helper pod.
const POD_READY_POLL: Duration = Duration::from_secs(1);

/// The `kubectl` executable [`KubectlExec`] spawns, resolved through `PATH`
/// (and the interrupt's cleanup, `helper_interrupt`).
pub(crate) const KUBECTL_BIN: &str = "kubectl";

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

/// Copy an exec's stdout to `out`, sending one message on `first_byte` as
/// soon as the first byte has been read (before it is written), and never
/// sending it for a command that wrote nothing.
fn copy_exec_stdout<R: Read>(
    stdout: R,
    mut out: std::fs::File,
    first_byte: Option<std::sync::mpsc::Sender<()>>,
) -> Result<()> {
    let copy_error =
        |e: io::Error| CliError::Other(format!("copy kubectl exec stdout → file: {e}"));
    let mut reader = BufReader::new(stdout);
    if let Some(first_byte) = first_byte {
        let chunk = loop {
            match reader.fill_buf() {
                Ok(chunk) => break chunk.len(),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(copy_error(e)),
            }
        };
        if chunk == 0 {
            return Ok(());
        }
        let _ = first_byte.send(());
    }
    io::copy(&mut reader, &mut out).map_err(copy_error)?;
    Ok(())
}

/// A pod's `metadata.uid`, when it has one.
fn uid_of(pod: &serde_json::Value) -> Option<String> {
    pod.pointer("/metadata/uid")
        .and_then(serde_json::Value::as_str)
        .filter(|uid| !uid.is_empty())
        .map(str::to_string)
}

/// How [`KubectlExec::kubectl_put`] puts a pod in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Put {
    /// `kubectl create`: there is no pod of that name, and the apiserver
    /// refuses (`AlreadyExists`) rather than touch one another run created
    /// since. Its answer is the one that says a pod is this process's.
    Create,
    /// `kubectl apply` over the pod of that name that is there.
    Apply,
}

impl Put {
    fn verb(self) -> &'static str {
        match self {
            Put::Create => "create",
            Put::Apply => "apply",
        }
    }
}

/// What kubectl answered to a [`Put`]: [`KubectlExec::kubectl_put`].
enum ApplyAnswer {
    /// Done: the uid of the pod the apiserver answered with (empty when
    /// kubectl printed none).
    Applied { uid: String },
    /// kubectl ran and refused: its error, and its whole stderr.
    Refused { error: CliError, stderr: String },
}

/// Whether kubectl's stderr is the apiserver refusing a create because an
/// object of that name exists, as `kubectl create` prints it (Kubernetes
/// 1.36): `Error from server (AlreadyExists): error when creating "STDIN":
/// pods "<name>" already exists`.
fn is_already_exists(stderr: &str) -> bool {
    stderr.contains("(AlreadyExists)")
}

impl KubectlExec {
    /// `kubectl create --save-config` or `kubectl apply` of `json_bytes` in
    /// `ns`, printing the uid of the pod the apiserver answered with
    /// (`-o jsonpath={.metadata.uid}`). `--save-config` makes a created pod
    /// the same object an apply would have created.
    ///
    /// A refusal comes with kubectl's WHOLE stderr beside its error. The
    /// error keeps the last lines, which is where a failing command usually
    /// explains itself; the apiserver's refusal of a pod update that cannot
    /// change in place is the FIRST line, above a unified diff of the pod spec
    /// with a hunk per changed field (a changed `sleep` alone is ten lines on
    /// Kubernetes 1.35), which a helper built by another version can run past
    /// the lines the error keeps. The caller needs that first line.
    fn kubectl_put(&self, put: Put, ns: &str, json_bytes: &[u8]) -> Result<ApplyAnswer> {
        let verb = put.verb();
        let mut args = vec![verb];
        if put == Put::Create {
            args.push("--save-config");
        }
        args.extend(["-f", "-", "-n", ns, "-o", "jsonpath={.metadata.uid}"]);
        let mut apply_child = Command::new(&self.kubectl_bin)
            .args(&args)
            .env("KUBECONFIG", &self.kubeconfig)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| CliError::Other(format!("spawn kubectl {verb}: {e}")))?;

        // Both read whole, each on its own thread, so neither pipe can fill
        // and block kubectl — and before the spec is written, so a kubectl
        // that answers before it has read all of it cannot stall either.
        let drain = |pipe: Option<Box<dyn Read + Send>>| {
            thread::spawn(move || {
                let mut text = String::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_string(&mut text);
                }
                text
            })
        };
        let stdout_reader = drain(
            apply_child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let stderr_reader = drain(
            apply_child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );

        // The write result is held rather than propagated here, and the order
        // that follows is the point. A child that dies before reading its
        // stdin gives the parent `EPIPE`, and returning that immediately
        // swallows the only useful thing on the failure path: kubectl's own
        // complaint, already sitting on its stderr. "write pod spec to kubectl
        // apply: Broken pipe (os error 32)", handed to someone whose actual
        // problem is an unreadable kubeconfig or a denied RBAC rule, names the
        // wrong process and tells them nothing.
        //
        // So the child is reaped first and its status is answered first. The
        // pipe error is still reported when the child exited 0 — exit 0 is the
        // tool's claim about what it did with its input, not evidence that the
        // input arrived, and an undelivered manifest must never read as
        // applied.
        let write_result = {
            let mut stdin = apply_child
                .stdin
                .take()
                .ok_or_else(|| CliError::Other(format!("kubectl {verb} has no stdin")))?;
            stdin.write_all(json_bytes)
            // `stdin` drops here, closing the pipe — the child needs that EOF
            // to finish, so it must happen before the `wait()` below.
        };

        let apply_status = apply_child
            .wait()
            .map_err(|e| CliError::Other(format!("wait kubectl {verb}: {e}")))?;
        // kubectl has exited, so its pipes are at EOF (it starts no children).
        let stdout = stdout_reader.join().unwrap_or_default();
        let stderr = stderr_reader.join().unwrap_or_default();
        if !apply_status.success() {
            let tail: Vec<String> = stderr
                .lines()
                .rev()
                .take(STDERR_CAPTURE_LIMIT)
                .map(str::to_string)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let error = format_exec_error(
                &format!("apply_and_wait_pod_ready({verb})"),
                apply_status,
                &Arc::new(Mutex::new(tail)),
            );
            return Ok(ApplyAnswer::Refused { error, stderr });
        }
        write_result
            .map_err(|e| CliError::Other(format!("write pod spec to kubectl {verb}: {e}")))?;
        Ok(ApplyAnswer::Applied {
            uid: stdout.trim().to_string(),
        })
    }

    /// The pod `name` in `ns` as JSON, or `None` when there is none
    /// (`--ignore-not-found`: kubectl then prints nothing and exits 0).
    fn get_pod_if_present(&self, name: &str, ns: &str) -> Result<Option<serde_json::Value>> {
        let out = Command::new(&self.kubectl_bin)
            .args([
                "get",
                "pod",
                name,
                "-n",
                ns,
                "--ignore-not-found",
                "-o",
                "json",
            ])
            .env("KUBECONFIG", &self.kubeconfig)
            .output()
            .map_err(|e| CliError::Other(format!("spawn kubectl get pod: {e}")))?;
        if !out.status.success() {
            return Err(CliError::Other(format!(
                "kubectl get pod {name} -n {ns} failed (exit {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        if out.stdout.iter().all(u8::is_ascii_whitespace) {
            return Ok(None);
        }
        serde_json::from_slice(&out.stdout).map(Some).map_err(|e| {
            CliError::Other(format!("kubectl get pod {name} -n {ns}: JSON parse: {e}"))
        })
    }

    /// [`Self::kubectl_put`] of a pod spec, with a backup helper recorded for
    /// the interrupt first ([`helper_interrupt::HelperPods::begin_apply`]) and
    /// settled by the apiserver's answer before the call stops counting as
    /// under way:
    ///
    /// * a create answered with a uid: [`helper_interrupt::Origin::Created`],
    ///   the one answer that makes a pod this process's;
    /// * an apply answered with the uid of the pod read just before it
    ///   (`seen`): [`helper_interrupt::Origin::Reused`]; answered with any
    ///   other, the pod was replaced in between, and whether this apply
    ///   created the one there now or went over another run's cannot be told
    ///   — it stays unconfirmed;
    /// * a create refused as `AlreadyExists`, or an apply refused as an
    ///   immutable update: nothing was made, and the record is dropped;
    /// * anything else — kubectl killed by the same Ctrl-C, a lost
    ///   connection — stays unconfirmed, and the interrupt leaves that pod.
    ///
    /// Refused once the interrupt has begun.
    fn tracked_put(
        &self,
        spec: &serde_json::Value,
        ns: &str,
        name: &str,
        put: Put,
        seen: Option<&str>,
        json_bytes: &[u8],
    ) -> Result<ApplyAnswer> {
        let in_flight = if backup_core::helper_pod::is_backup_helper(spec) {
            Some(self.helpers.begin_apply(&self.kubeconfig, ns, name)?)
        } else {
            None
        };
        let answer = self.kubectl_put(put, ns, json_bytes)?;
        if let Some(in_flight) = in_flight {
            use helper_interrupt::Origin;
            match (&answer, put) {
                (ApplyAnswer::Applied { uid }, _) if uid.is_empty() => {}
                (ApplyAnswer::Applied { uid }, Put::Create) => {
                    in_flight.answered(Origin::Created(uid.clone()));
                }
                (ApplyAnswer::Applied { uid }, Put::Apply) if seen == Some(uid.as_str()) => {
                    in_flight.answered(Origin::Reused(uid.clone()));
                }
                (ApplyAnswer::Applied { .. }, Put::Apply) => {}
                (ApplyAnswer::Refused { stderr, .. }, Put::Create) if is_already_exists(stderr) => {
                    in_flight.not_created();
                }
                (ApplyAnswer::Refused { stderr, .. }, Put::Apply)
                    if backup_core::helper_pod::is_immutable_pod_update(stderr) =>
                {
                    in_flight.not_created();
                }
                (ApplyAnswer::Refused { .. }, _) => {}
            }
        }
        Ok(answer)
    }

    /// Read pod `name` until it is Running + Ready, for up to `timeout`, every
    /// `poll` — the runner's `KubeRsExec` waits the same way. A container the
    /// kubelet
    /// cannot configure for `grace` without a break (a credential Secret or
    /// key missing: [`backup_core::helper_pod::container_config_error`]) ends
    /// the wait at once with the kubelet's words, rather than after the whole
    /// `timeout` with none. A read settles nothing for the interrupt: only the
    /// apiserver's answer to the create does ([`Self::tracked_put`]).
    fn wait_pod_ready(
        &self,
        name: &str,
        ns: &str,
        timeout: Duration,
        poll: Duration,
        grace: Duration,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        let mut config_error = backup_core::helper_pod::ConfigErrorWatch::default();
        loop {
            self.refuse_if_interrupted()?;
            let pod = self.get_pod_if_present(name, ns)?.ok_or_else(|| {
                CliError::Other(format!(
                    "pod {name} in {ns} is gone: it was deleted while this command waited for \
                     it to be Ready"
                ))
            })?;
            if backup_core::helper_pod::pod_is_ready(&pod) {
                return Ok(());
            }
            if let Some(why) = config_error.observe(&pod, std::time::Instant::now(), grace) {
                return Err(backup_core::helper_pod::container_config_error_message(
                    ns, name, &why,
                ));
            }
            if std::time::Instant::now() >= deadline {
                return Err(CliError::Other(format!(
                    "pod {name} in {ns} did not reach Ready within {}s",
                    timeout.as_secs()
                )));
            }
            thread::sleep(poll);
        }
    }

    /// Put the pod `spec` describes in place under `name`, for
    /// [`KubeExec::apply_and_wait_pod_ready`]: create it when there is none,
    /// apply over one of the same spec that is still running, and replace
    /// any other.
    ///
    /// A pod of this name left from an earlier run is replaced rather than
    /// applied over: an ended one never becomes Ready, one being deleted is
    /// about to go, and a running one hours into its keep-alive would end this
    /// command's work in it early
    /// ([`backup_core::helper_pod::stale_helper_reason`]). So is one whose
    /// spec this one cannot be applied over — an older CLI's or runner's, with
    /// another keep-alive or env.
    ///
    /// Where there is no pod the helper is CREATED, not applied: the
    /// apiserver's answer to a create is what makes it this process's for the
    /// interrupt ([`Self::tracked_put`]), and a create refuses, rather than
    /// takes over, a pod another run created since the read. That pod is then
    /// read and judged like any other, once.
    fn put_helper(
        &self,
        spec: &serde_json::Value,
        ns: &str,
        name: &str,
        json_bytes: &[u8],
    ) -> Result<()> {
        let mut raced = false;
        loop {
            let mut over: Option<Option<String>> = None;
            if let Some(existing) = self.get_pod_if_present(name, ns)? {
                if let Some(why) = backup_core::helper_pod::stale_helper_reason(
                    &existing,
                    spec,
                    chrono::Utc::now(),
                ) {
                    eprintln!(
                        "{}",
                        backup_core::helper_pod::replacing_stale_helper_note(ns, name, &why)
                    );
                    self.delete_and_wait_gone(name, ns)?;
                } else {
                    over = Some(uid_of(&existing));
                }
            }

            if let Some(seen) = over {
                match self.tracked_put(spec, ns, name, Put::Apply, seen.as_deref(), json_bytes)? {
                    ApplyAnswer::Applied { .. } => return Ok(()),
                    ApplyAnswer::Refused { stderr, .. }
                        if backup_core::helper_pod::is_immutable_pod_update(&stderr) =>
                    {
                        eprintln!(
                            "{}",
                            backup_core::helper_pod::replacing_stale_helper_note(
                                ns,
                                name,
                                "was created with a spec this command's cannot be applied over \
                                 (an earlier run of another version, or with another keep-alive)",
                            )
                        );
                        self.delete_and_wait_gone(name, ns)?;
                    }
                    ApplyAnswer::Refused { error, .. } => return Err(error),
                }
            }

            match self.tracked_put(spec, ns, name, Put::Create, None, json_bytes)? {
                ApplyAnswer::Applied { .. } => return Ok(()),
                // Another run created it since the read: judge that pod, once.
                ApplyAnswer::Refused { stderr, .. } if !raced && is_already_exists(&stderr) => {
                    raced = true;
                }
                ApplyAnswer::Refused { error, .. } => return Err(error),
            }
        }
    }

    /// Delete a stale helper pod and wait until it is gone, within
    /// [`backup_core::helper_pod::STALE_POD_GONE_WITHIN`] (`kubectl delete
    /// --wait` watches the pod until it has).
    fn delete_and_wait_gone(&self, name: &str, ns: &str) -> Result<()> {
        let bound = backup_core::helper_pod::STALE_POD_GONE_WITHIN.as_secs();
        let out = Command::new(&self.kubectl_bin)
            .args([
                "delete",
                "pod",
                name,
                "-n",
                ns,
                "--ignore-not-found",
                &format!(
                    "--grace-period={}",
                    backup_core::helper_pod::STALE_POD_DELETE_GRACE_SECONDS
                ),
                "--wait=true",
                &format!("--timeout={bound}s"),
            ])
            .env("KUBECONFIG", &self.kubeconfig)
            .output()
            .map_err(|e| CliError::Other(format!("spawn kubectl delete pod: {e}")))?;
        if out.status.success() {
            return Ok(());
        }
        Err(CliError::Other(format!(
            "the stale helper pod {name} in {ns} was not gone {bound}s after it was deleted \
             (its node may be unreachable); delete it with `kubectl delete pod {name} -n {ns} \
             --force --grace-period=0` and run again. kubectl: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

impl KubeExec for KubectlExec {
    fn apply_and_wait_pod_ready(&self, spec: &serde_json::Value) -> Result<()> {
        self.refuse_if_interrupted()?;
        let name = spec["metadata"]["name"]
            .as_str()
            .ok_or_else(|| CliError::Other("pod spec missing metadata.name".into()))?;
        let ns = spec["metadata"]["namespace"]
            .as_str()
            .ok_or_else(|| CliError::Other("pod spec missing metadata.namespace".into()))?;

        let json_bytes = serde_json::to_vec(spec)
            .map_err(|e| CliError::Other(format!("serialize pod spec: {e}")))?;

        self.put_helper(spec, ns, name, &json_bytes)?;

        self.wait_pod_ready(
            name,
            ns,
            backup_core::helper_pod::POD_READY_TIMEOUT,
            POD_READY_POLL,
            backup_core::helper_pod::CONTAINER_CONFIG_ERROR_GRACE,
        )
    }

    fn exec_stream_to_file(
        &self,
        pod: &str,
        ns: &str,
        argv: &[&str],
        out_path: &Path,
        first_output_within: Option<Duration>,
    ) -> Result<()> {
        self.refuse_if_interrupted()?;
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

        let out_file = std::fs::File::create(out_path).map_err(|e| {
            CliError::Other(format!("create output file {}: {e}", out_path.display()))
        })?;
        match first_output_within {
            None => copy_exec_stdout(stdout, out_file, None)?,
            Some(bound) => {
                // The copy runs on its own thread so this one can stop
                // waiting: the copier reports its first byte, and a command
                // that has written nothing by `bound` has kubectl killed under
                // it. kubectl is one process, so killing it closes the pipe
                // and the copier ends; it is not joined all the same, because
                // anything else holding the pipe open would hold this call.
                let (first_byte, first_byte_seen) = std::sync::mpsc::channel();
                let copier =
                    thread::spawn(move || copy_exec_stdout(stdout, out_file, Some(first_byte)));
                match first_byte_seen.recv_timeout(bound) {
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(backup_core::kube::no_output_error(
                            "exec_stream_to_file",
                            argv,
                            ns,
                            pod,
                            bound,
                        ));
                    }
                    // The first byte arrived, or the copier already finished
                    // (EOF before any output, or a write error): either way
                    // the copy's own result says the rest.
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
                }
                copier.join().map_err(|_| {
                    CliError::Other("copy kubectl exec stdout → file: the copy panicked".into())
                })??;
            }
        }

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
        self.refuse_if_interrupted()?;
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
        // After an interrupt the interrupt's cleanup deletes this command's
        // helpers, by uid; a delete by name from here could take a pod of the
        // same name that this command never created.
        if self.interrupted() {
            return;
        }
        let deleted = Command::new(&self.kubectl_bin)
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
        // Forgotten only when the delete went through: one that failed (or
        // whose kubectl died of the same Ctrl-C) leaves the pod for the
        // interrupt's cleanup.
        if deleted.is_ok_and(|status| status.success()) {
            self.helpers.deleted(ns, name);
        }
    }

    fn get_secret_key(&self, secret: &str, ns: &str, key: &str) -> Result<String> {
        self.refuse_if_interrupted()?;
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
        self.refuse_if_interrupted()?;
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
    // First, so it is dropped last: Ctrl-C deletes the helper pods this
    // command created (`helper_interrupt`).
    let _interruptible = helper_interrupt::install(None);
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
    export_extract(&k, &plan, &out_dir, &pg_image)?;

    let platform_version = read_platform_version(kc.path())?;
    let manifest = export_manifest(&cluster_id, &platform_version, &ns_set, &claims);
    write_manifest(&manifest, &out_dir)?;

    print!(
        "{}",
        export_summary(
            &cluster_id,
            &out_dir,
            &ns_set,
            claims.len(),
            plan.len(),
            &claims_without_data_capture(&claims),
        )
    );
    Ok(())
}

/// Extract an export's data: every helper pod, sized as an interactive
/// command's helpers are.
///
/// No deadline stops an export, so its helper pods' keep-alive is the only
/// limit on one extraction: the cluster's backup deadline, never less than
/// six hours however short a frequent schedule has made it
/// ([`backup_core::engine::read_helper_keep_alive`]). The read is here rather
/// than in [`run_export`] so the tests drive it: a keep-alive sized from the
/// schedule's deadline alone once cut a long restore short, and what guards
/// against it coming back is the pod specs this builds.
fn export_extract(
    k: &dyn KubeExec,
    plan: &[backup_core::extract::ExtractItem],
    out_dir: &Path,
    pg_image: &str,
) -> Result<()> {
    let keep_alive = backup_core::engine::read_helper_keep_alive(k)?;
    run_extraction(k, plan, out_dir, pg_image, keep_alive)
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
        // An export captures native data only — no secrets, so no
        // secret namespaces to record.
        secret_namespaces: Vec::new(),
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
    uncaptured: &[UncapturedClaim],
) -> String {
    let mut out = format!(
        "✓ Exported {} namespace(s) from cluster '{cluster_id}' → {}\n  namespaces: {}\n  claims:     {claim_count} ({extractable_count} extractable)\n",
        namespaces.len(),
        out_dir.display(),
        namespaces.join(", "),
    );
    for line in uncaptured_claims_lines(uncaptured, "export") {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// What a summary says about claims it captured as CONFIGURATION ONLY (A6).
/// One line per claim type, each naming the claims. Pure.
///
/// The whole point of the finding: the claim lands in `manifest.resources` and
/// `backup show` lists it, so the snapshot reads as complete while holding none
/// of that claim's data. Saying it at capture time is what makes it knowable
/// before the restore rather than after — so both `backup create` and
/// `backup show` print this, from the same function, and an `export` does too
/// because it has exactly the same gap.
///
/// Deliberately says nothing about capture arriving later: it is separate,
/// larger work and is not underway, and a summary that hinted otherwise would
/// be inviting an operator to wait for it.
fn uncaptured_claims_lines(uncaptured: &[UncapturedClaim], artifact: &str) -> Vec<String> {
    let mut by_type: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for c in uncaptured {
        by_type
            .entry(c.claim_type.as_str())
            .or_default()
            .push(format!("{}/{}", c.namespace, c.name));
    }
    by_type
        .into_iter()
        .map(|(ty, mut names)| {
            names.sort();
            format!(
                "  ⚠ {ty}: {} claim(s) captured as configuration only — no {ty} data is in this \
                 {artifact}, so a restore brings them back empty: {}",
                names.len(),
                names.join(", ")
            )
        })
        .collect()
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
    // First, so it is dropped last: Ctrl-C deletes the helper pods this
    // command created (`helper_interrupt`).
    let _interruptible = helper_interrupt::install(None);
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

    // The snapshot's identity (E1). A local pull can be aimed at a SHARED
    // repository with `--repo s3:…`, so it must stamp the same machine key the
    // scheduled runner does — otherwise it lands as an unidentified snapshot in
    // a pool two clusters draw from.
    let cluster_uid = read_cluster_uid(kc.path())?;

    let pg_image = pg_helper_image(first_cnpg_image(&ns_set, kc.path()).as_deref());
    let platform_version = read_platform_version(kc.path())?;

    // Stage everything under a tempdir; the engine writes data/ under this root.
    let staging = tempfile::Builder::new()
        .prefix("apprafter-backup-")
        .tempdir()
        .map_err(|e| CliError::Other(format!("create staging dir: {e}")))?;

    let opts = local_pull_backup_opts(
        &k,
        &repo_str,
        pass,
        &cluster_id,
        &cluster_uid,
        &platform_version,
        &ns_set,
        select,
        staging.path(),
        pg_image,
        staging_mode,
    )?;

    let r = RefusingAfterInterrupt(SubprocessRestic);
    let summary = backup_core::engine::run_backup_with_summary(&k, &r, &opts)?;

    print!(
        "{}",
        backup_summary_report(&cluster_id, &repo_str, &ns_set, &summary)
    );
    Ok(())
}

/// A [`ResticRunner`] that starts no restic once the command has been
/// interrupted (`helper_interrupt`), as [`KubectlExec`] starts no kubectl: a
/// SIGTERM sent to the CLI alone leaves the command's own thread running
/// until the interrupt exits, and a `restic backup` begun in that time would
/// write a snapshot after the user stopped the command.
struct RefusingAfterInterrupt<R>(R);

impl<R: ResticRunner> ResticRunner for RefusingAfterInterrupt<R> {
    fn run(&self, argv: &[String], passphrase: &str) -> Result<()> {
        helper_interrupt::refuse_if_interrupted()?;
        self.0.run(argv, passphrase)
    }

    fn run_stdout(&self, argv: &[String], passphrase: &str) -> Result<String> {
        helper_interrupt::refuse_if_interrupted()?;
        self.0.run_stdout(argv, passphrase)
    }

    fn run_backup(&self, argv: &[String], passphrase: &str) -> Result<Option<String>> {
        helper_interrupt::refuse_if_interrupted()?;
        self.0.run_backup(argv, passphrase)
    }

    fn run_capture(&self, argv: &[String], passphrase: &str) -> Result<backup_core::ResticOutput> {
        helper_interrupt::refuse_if_interrupted()?;
        self.0.run_capture(argv, passphrase)
    }
}

/// Assemble the [`BackupOpts`] the CLI local-pull path hands to the engine.
///
/// Extracted from [`run_backup`] and called from both there and the tests.
/// INVARIANT: `backup_host` is `None`. The CLI pull keeps the operator
/// workstation's own hostname as the restic group, which is what makes
/// per-station grouping work; only the in-cluster runner pins
/// `Some("apprafter-backup")` because its pod name is ephemeral (spec
/// §Retention M-r3-1a).
///
/// The one cluster read is the helper pods' keep-alive
/// ([`backup_core::engine::read_helper_keep_alive`]). An interactive backup
/// has no Job deadline — the person running it is the one who stops it — so
/// that keep-alive is the only limit on one extraction: the cluster's backup
/// deadline, never less than six hours however short a frequent schedule has
/// made it. The scheduled runner follows the same rule, so both build one
/// spec for a helper's name. It is read here, not passed in, so the tests
/// drive the read itself: a keep-alive sized from the schedule's deadline
/// alone once cut a long restore short.
#[allow(clippy::too_many_arguments)]
fn local_pull_backup_opts(
    k: &dyn KubeExec,
    repo: &str,
    passphrase: String,
    cluster_id: &str,
    cluster_uid: &str,
    platform_version: &str,
    namespaces: &[String],
    is_subset: bool,
    staging_root: &Path,
    pg_image: String,
    staging_mode: StagingMode,
) -> Result<BackupOpts> {
    Ok(BackupOpts {
        repo: repo.to_string(),
        passphrase,
        cluster_id: cluster_id.to_string(),
        cluster_uid: cluster_uid.to_string(),
        created_at: now_rfc3339(),
        platform_version: platform_version.to_string(),
        namespaces: namespaces.to_vec(),
        is_subset,
        staging_root: staging_root.to_path_buf(),
        pg_image,
        helper_keep_alive: backup_core::engine::read_helper_keep_alive(k)?,
        staging_mode,
        backup_host: None,
    })
}

/// The operator-facing summary `backup` prints on success. Pure — extracted
/// from [`run_backup`], which prints exactly this.
///
/// INVARIANT: the `snapshot:` line is present only when restic reported a
/// snapshot id. Printing an empty one would read as a stored snapshot that
/// does not exist.
///
/// The imported certificate is named only when there IS one. It is the one
/// captured object an operator can check by eye against a `target domain list`
/// — and the one whose absence used to be invisible until a restore produced a
/// Gateway pointing at nothing — so when it is in the snapshot the summary
/// says so, and on the clusters that never connected a domain the line does
/// not appear at all.
fn backup_summary_report(
    cluster_id: &str,
    repo: &str,
    namespaces: &[String],
    summary: &backup_core::engine::BackupSummary,
) -> String {
    let certs = match summary.cert_count {
        0 => String::new(),
        n => format!(", {n} imported cert(s)"),
    };
    let mut out = format!(
        "✓ Backed up cluster '{cluster_id}' → {repo}\n  namespaces: {}\n  captured:   {} CR(s), {} secret(s){certs}, {} claim(s) ({} extracted)\n  tag:        {}\n",
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
    // A6, last so it is the line left on screen: the claims this run captured
    // as configuration only. Absent — and silent — on the clusters that
    // declare none, which is most of them.
    for line in uncaptured_claims_lines(&summary.uncaptured_claims, "backup") {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// `apprafter backup run` — run the scheduled backup NOW
//
// The scheduled backup is a CronJob the platform chart deploys. Triggering it
// means instantiating a Job from that CronJob's own `jobTemplate` — the same
// image, service account, mounts and credentials the 03:00 run uses — which is
// what makes a manual run evidence about the scheduled one. Building an
// equivalent Job here instead would drift from the chart the moment either
// side changed, and then the command that is supposed to prove the backup
// works would be proving something else.
// ---------------------------------------------------------------------------

/// The CronJob the platform chart deploys for scheduled backup.
pub(crate) const BACKUP_CRONJOB_NAME: &str = "apprafter-backup";

/// The CronJob the platform chart deploys for the weekly repository check.
pub(crate) const CHECK_CRONJOB_NAME: &str = "apprafter-backup-check";

/// Name prefix of the Jobs `backup run` creates ([`manual_job_name`]).
const MANUAL_JOB_PREFIX: &str = "apprafter-backup-manual-";

/// Name for a manually triggered backup Job: the CronJob's name, `manual`,
/// and a UTC stamp, which is what makes two runs in the same minute
/// distinguishable and any run identifiable in `kubectl get jobs`.
fn manual_job_name(stamp: &str) -> String {
    format!("{MANUAL_JOB_PREFIX}{stamp}")
}

/// Which of the two runners a Job is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunnerJob {
    /// A backup: the backup CronJob's, or one `backup run` created.
    Backup,
    /// A repository check: the check CronJob's.
    Check,
}

/// Is `job` a backup or check runner, and which? `None` for any other Job.
/// Pure.
///
/// The operator's `BackupHealthy` rule, exactly (`Run::owns` in the
/// platform-stack controller's `backup_health.rs`), so the CLI and the
/// cluster's status count the same Jobs:
///
/// * A Job with a CronJob owner is that CronJob's: `apprafter-backup` or
///   `apprafter-backup-check`, whatever the Job is called. The Job controller
///   sets the owner on every scheduled Job, and `kubectl create job
///   --from=cronjob/<name> <any-name>` sets it too.
/// * A Job with no owner is a backup only when `backup run` created it: its
///   name and its `apprafter.io/manual` label ([`job_from_cronjob`] leaves the
///   owner off on purpose, so the Job outlives its CronJob).
///
/// It used to be the name prefix `apprafter-backup`, while the operator used
/// the owner. A Job made the usual Kubernetes way under another name was
/// counted by the operator and invisible here: `backup status` printed
/// "Last backup Job: none" beside it, and `backup run` started a second
/// runner next to it.
pub(crate) fn runner_job(job: &Value) -> Option<RunnerJob> {
    let cronjob_owner = job
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .and_then(|refs| {
            refs.iter()
                .find(|r| r.get("kind").and_then(Value::as_str) == Some("CronJob"))
        })
        .map(|r| r.get("name").and_then(Value::as_str).unwrap_or(""));
    match cronjob_owner {
        Some(BACKUP_CRONJOB_NAME) => Some(RunnerJob::Backup),
        Some(CHECK_CRONJOB_NAME) => Some(RunnerJob::Check),
        Some(_) => None,
        None => (job_metadata_name(job).starts_with(MANUAL_JOB_PREFIX)
            && job
                .pointer("/metadata/labels/apprafter.io~1manual")
                .and_then(Value::as_str)
                == Some("true"))
        .then_some(RunnerJob::Backup),
    }
}

/// Build a Job manifest from a CronJob's `spec.jobTemplate`.
///
/// Mirrors what `kubectl create job --from=cronjob/<name>` does, including
/// the `cronjob.kubernetes.io/instantiate: manual` annotation, so a Job
/// created here is indistinguishable from one created that way. The extra
/// `apprafter.io/manual` label is ours: it tells an operator reading
/// `kubectl get jobs -n apprafter-system` during an incident which runs were
/// asked for and which were the schedule.
///
/// No `ownerReferences`: a manual Job outliving its CronJob is the point —
/// deleting the schedule must not garbage-collect the evidence that the last
/// manual backup succeeded.
fn job_from_cronjob(cronjob: &Value, job_name: &str) -> Result<Value> {
    let template = cronjob.pointer("/spec/jobTemplate").ok_or_else(|| {
        CliError::Other(format!(
            "CronJob '{BACKUP_CRONJOB_NAME}' has no spec.jobTemplate — the platform chart \
             renders one, so this is either a hand-edited object or a chart version that \
             predates scheduled backup. Re-sync the platform chart and try again."
        ))
    })?;
    let namespace = cronjob
        .pointer("/metadata/namespace")
        .and_then(Value::as_str)
        .unwrap_or(PLATFORMSTACK_NAMESPACE);

    let mut labels = template
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    labels.insert("apprafter.io/manual".to_string(), Value::from("true"));

    let mut annotations = template
        .pointer("/metadata/annotations")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    annotations.insert(
        "cronjob.kubernetes.io/instantiate".to_string(),
        Value::from("manual"),
    );

    Ok(serde_json::json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": job_name,
            "namespace": namespace,
            "labels": Value::Object(labels),
            "annotations": Value::Object(annotations),
        },
        "spec": template.pointer("/spec").cloned().unwrap_or(serde_json::json!({})),
    }))
}

/// Where a Job is in its life, as its status reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JobOutcome {
    /// Not finished — including "not started yet".
    Running,
    /// `Complete` condition is True.
    Succeeded,
    /// `Failed` condition is True; carries reason + message.
    Failed(String),
}

/// Read a Job's outcome from its conditions.
///
/// Only `Complete` and `Failed` with `status: "True"` are terminal. Reading
/// `.status.succeeded`/`.status.failed` counts instead would be wrong in both
/// directions: a Job with a failed pod and retries left reports
/// `failed: 1` while still on its way to success.
fn job_run_outcome(job: &Value) -> JobOutcome {
    let Some(conds) = job.pointer("/status/conditions").and_then(Value::as_array) else {
        return JobOutcome::Running;
    };
    for c in conds {
        if c.pointer("/status").and_then(Value::as_str) != Some("True") {
            continue;
        }
        match c.pointer("/type").and_then(Value::as_str) {
            Some("Complete") => return JobOutcome::Succeeded,
            Some("Failed") => {
                let reason = c
                    .pointer("/reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Failed");
                let message = c.pointer("/message").and_then(Value::as_str).unwrap_or("");
                return JobOutcome::Failed(if message.is_empty() {
                    reason.to_string()
                } else {
                    format!("{reason}: {message}")
                });
            }
            _ => {}
        }
    }
    JobOutcome::Running
}

/// How often the trigger asks the apiserver whether the Job has finished.
const JOB_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The container env var the platform chart renders the repo URL into.
/// Reading it back is how the CLI tells a CronJob that has caught up with a
/// fresh `spec.backup` from one that is still the previous render.
const BACKUP_REPO_ENV: &str = "APPRAFTER_BACKUP_REPO";

/// The repo a deployed CronJob would write to, read out of its runner
/// container's env. `None` when the CronJob does not carry the variable —
/// which must never read as "matches", since acting on it would fire a
/// backup at whatever the previous render pointed at.
fn cronjob_repo(cronjob: &Value) -> Option<String> {
    cronjob
        .pointer("/spec/jobTemplate/spec/template/spec/containers")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(|c| c.pointer("/env").and_then(Value::as_array))
        .flatten()
        .find(|e| e.pointer("/name").and_then(Value::as_str) == Some(BACKUP_REPO_ENV))
        .and_then(|e| e.pointer("/value"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// `apprafter backup run` — instantiate the scheduled backup NOW.
///
/// Runs the platform's own CronJob template as a one-off Job, which is the
/// answer to three separate needs: proving a freshly enabled schedule works
/// without waiting for 03:00, taking a backup before something risky (an
/// upgrade, a migration), and giving `backup list` something to show.
///
/// The Job runs IN the cluster with the cluster's credentials — the operator
/// needs no S3 credentials locally, and a failure here is evidence about the
/// scheduled run rather than about this machine.
pub fn run_backup_trigger(wait: bool, timeout_minutes: u64) -> Result<()> {
    preflight_tools(&[&KUBECTL], "apprafter backup run")?;
    let kc = ensure_kubeconfig_tempfile()?;

    let Some(cronjob) = kubectl_get_json(
        "cronjob",
        Some(BACKUP_CRONJOB_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?
    else {
        return Err(CliError::Other(format!(
            "no CronJob '{BACKUP_CRONJOB_NAME}' in {PLATFORMSTACK_NAMESPACE} — scheduled backup \
             is what this command runs, and nothing has deployed it yet. Run `apprafter backup \
             enable --bucket <name> --endpoint <host> --credential-file <dotenv>` first; if you \
             just ran it, the platform chart has not synced yet — `apprafter backup status` \
             shows when it has."
        )));
    };

    if cronjob
        .pointer("/spec/suspend")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        println!(
            "  note: the schedule is suspended (`apprafter backup disable`) — running it once \
             anyway, which is what you want before an upgrade."
        );
    }

    instantiate_backup_job(&cronjob, wait, timeout_minutes, kc.path())
}

/// A backup or check Job that has not finished, as `backup run` finds it
/// before it starts another ([`active_runner_jobs`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveJob {
    name: String,
    /// What it is doing, as `backup status` prints it (`Running`, `Pending,
    /// cannot be scheduled: …`), or `Stopping (<reason>)` once the Job
    /// controller has begun to fail it.
    state: String,
    /// Its pod ([`job_pod`]).
    pod: JobPod,
    /// The lines `backup status` prints under it ([`job_pod::status_hint`]).
    hint: Option<String>,
}

/// The reason of `job`'s condition of type `kind` when it is True.
fn true_condition_reason(job: &Value, kind: &str) -> Option<String> {
    job.pointer("/status/conditions")
        .and_then(Value::as_array)?
        .iter()
        .find(|c| {
            c.get("type").and_then(Value::as_str) == Some(kind)
                && c.get("status").and_then(Value::as_str) == Some("True")
        })
        .map(|c| {
            c.get("reason")
                .and_then(Value::as_str)
                .unwrap_or(kind)
                .to_string()
        })
}

/// The backup and check Jobs ([`runner_job`]) that have not finished, newest
/// first: no `Complete` or `Failed` condition, nothing succeeded, and not
/// being deleted. Pure.
///
/// A Job the Job controller has begun to fail (`FailureTarget`) counts: its
/// runner is still stopping, and holds its room and its repository lock
/// until it has.
fn active_runner_jobs(jobs: &[Value], pods: &[Value]) -> Vec<ActiveJob> {
    let mut active: Vec<&Value> = jobs
        .iter()
        .filter(|j| runner_job(j).is_some())
        .filter(|j| j.pointer("/metadata/deletionTimestamp").is_none())
        .filter(|j| job_run_outcome(j) == JobOutcome::Running)
        .filter(|j| {
            j.pointer("/status/succeeded")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                == 0
        })
        .collect();
    active.sort_by_key(|j| std::cmp::Reverse(job_start_time(j)));
    active
        .into_iter()
        .map(|j| {
            let pod = job_pod(j, pods);
            let state = match true_condition_reason(j, "FailureTarget") {
                Some(r) => format!("Stopping ({r})"),
                None => job_line_outcome(j, pods),
            };
            ActiveJob {
                name: job_metadata_name(j).to_string(),
                hint: job_pod::status_hint(j, &pod),
                state,
                pod,
            }
        })
        .collect()
}

/// What `backup run` does instead of starting a run beside a backup or
/// check Job that has not finished: the report it prints and the error it
/// exits with. `None` when no such Job is active. Pure.
///
/// One run at a time. Two runs at once do not both finish: two backups need
/// the same helper pods, and a backup and a check each fail on the other's
/// repository lock, since neither waits for one. On a node with room for one
/// runner, the second would not even be scheduled, and `backup run` would
/// give up on it and report the scheduled backup as unable to start while
/// that was the one running. A Job no node takes, or whose container cannot
/// start, may hold on until its deadline, so for those the report also says
/// how to clear it.
fn refusal(jobs: &[Value], pods: &[Value]) -> Option<(String, CliError)> {
    let active = active_runner_jobs(jobs, pods);
    let first = active.first()?;
    let mut out = String::new();
    for a in &active {
        out.push_str(&format!("  ✗ {} has not finished: {}\n", a.name, a.state));
        if let Some(hint) = &a.hint {
            out.push_str(hint);
        }
        if matches!(
            a.pod,
            JobPod::Unschedulable { .. } | JobPod::NotStarted { reason: Some(_) }
        ) {
            out.push_str(&format!(
                "    It may hold on until its deadline stops it. To start a new run sooner, \
                 delete it first:\n      kubectl -n {PLATFORMSTACK_NAMESPACE} delete job {}\n",
                a.name
            ));
        }
    }
    out.push_str(&format!(
        "    A second run beside {} would not finish: two backups need the same helper pods, \
         and a backup and a check each fail on the other's repository lock.\n",
        if active.len() == 1 { "it" } else { "them" }
    ));
    Some((
        out,
        CliError::BackupJobActive {
            job: first.name.clone(),
        },
    ))
}

/// The other backup and check Jobs whose runner is running: each holds room
/// of the size `own`'s runner needs. Pure.
fn room_holders(own: &str, jobs: &[Value], pods: &[Value]) -> Vec<String> {
    active_runner_jobs(jobs, pods)
        .into_iter()
        .filter(|a| a.name != own && a.pod == JobPod::Running)
        .map(|a| a.name)
        .collect()
}

/// Create a one-off Job from `cronjob` and, unless told not to, wait for it.
///
/// Shared by `backup run` and by `backup enable`'s first backup, so both
/// produce the same object and the same reporting — a first backup that
/// differed from a manual one would make neither of them evidence about the
/// other.
///
/// Nothing is created while a backup or check Job has not finished
/// ([`refusal`]), `--no-wait` included.
fn instantiate_backup_job(
    cronjob: &Value,
    wait: bool,
    timeout_minutes: u64,
    kubeconfig: &Path,
) -> Result<()> {
    let jobs = backup_jobs_of(
        kubectl_get_json("jobs", None, Some(PLATFORMSTACK_NAMESPACE), kubeconfig)?.as_ref(),
    );
    let pods = if jobs
        .iter()
        .any(|j| job_run_outcome(j) == JobOutcome::Running)
    {
        kubectl_get_json("pods", None, Some(PLATFORMSTACK_NAMESPACE), kubeconfig)?
            .as_ref()
            .map(items_of)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if let Some((report, err)) = refusal(&jobs, &pods) {
        print!("{report}");
        return Err(err);
    }

    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let name = manual_job_name(&stamp);
    let manifest = job_from_cronjob(cronjob, &name)?;
    let yaml = serde_yaml::to_string(&manifest)
        .map_err(|e| CliError::Other(format!("serialize Job manifest: {e}")))?;
    kubectl_apply_server_side(&yaml, "apprafter-cli", kubeconfig)?;
    println!("  → Job {name} created in {PLATFORMSTACK_NAMESPACE}");

    if !wait {
        println!(
            "  not waiting (--no-wait). Follow it with:\n    \
             kubectl -n {PLATFORMSTACK_NAMESPACE} logs -f job/{name}\n  \
             or check the outcome later with `apprafter backup status`."
        );
        return Ok(());
    }

    wait_for_backup_job(&name, timeout_minutes, kubeconfig)
}

/// Default wall-clock ceiling for waiting on a backup Job, shared by
/// `backup run --timeout` and the first backup `enable` takes.
pub(crate) const DEFAULT_BACKUP_JOB_TIMEOUT_MINUTES: u64 = 60;

/// How long `backup enable` waits for Argo CD to render the CronJob from the
/// `spec.backup` it just patched, before giving up on the first backup.
///
/// Argo CD's default reconciliation is three minutes, so a shorter wait would
/// report "not synced" on a perfectly healthy cluster most of the time.
const CRONJOB_SYNC_WAIT_MINUTES: u64 = 5;

/// Wait until the deployed CronJob writes to `repo` — i.e. until the platform
/// chart has caught up with the `spec.backup` just written. `Ok(None)` on
/// timeout: a chart that has not synced yet is a wait, not a failure, and the
/// `enable` it follows has already succeeded.
fn wait_for_synced_cronjob(
    repo: &str,
    timeout_minutes: u64,
    kubeconfig: &Path,
) -> Result<Option<Value>> {
    let deadline = Duration::from_secs(timeout_minutes * 60);
    let started = std::time::Instant::now();
    let mut announced = false;
    loop {
        let cj = kubectl_get_json(
            "cronjob",
            Some(BACKUP_CRONJOB_NAME),
            Some(PLATFORMSTACK_NAMESPACE),
            kubeconfig,
        )?;
        if let Some(cj) = cj {
            if cronjob_repo(&cj).as_deref() == Some(repo) {
                return Ok(Some(cj));
            }
        }
        if started.elapsed() >= deadline {
            return Ok(None);
        }
        if !announced {
            println!(
                "  waiting for the platform chart to deploy the schedule (Argo CD reconciles \
                 every few minutes)…"
            );
            announced = true;
        }
        thread::sleep(JOB_POLL_INTERVAL);
    }
}

/// What the wait does after one look at the Job and its pods.
#[derive(Debug, PartialEq, Eq)]
enum WaitStep {
    /// The Job's `Complete` condition is True.
    Succeeded,
    /// The Job's `Failed` condition is True: its reason and message.
    Failed(String),
    /// The Job is gone.
    Vanished,
    /// Its pod has been unschedulable for `for_`, past [`grace_for`] its
    /// reason: the scheduler's message and what it says, what the runner asks
    /// for ([`job_pod::runner_requests`]), and the attempts that failed before
    /// this one with the newest one's reason.
    Unschedulable {
        message: String,
        cause: Unplaced,
        requests: Option<String>,
        for_: Duration,
        failed: u64,
        last_failure: Option<String>,
    },
    /// The caller's `--timeout` is up, with the pod in this state.
    TimedOut(JobPod),
    /// Keep waiting: the pod's state, and how long it has been unschedulable
    /// when it is.
    Wait(JobPod, Option<Duration>),
    /// Keep waiting, with the clock stopped: no room for the pod, but these
    /// pods are stopping, and the room they give back may be what it needs.
    RoomReturning { pod: JobPod, stopping: Vec<String> },
}

/// Decide the wait's next step from one observation. Pure: the loop in
/// [`wait_for_backup_job`] fetches, and `now`/`waited` come in as values, so
/// the order of the checks is pinned by tests instead of by a cluster.
///
/// The order matters. The Job's own verdict comes first: a Job that finished
/// has finished, whatever its pods look like. A pod no node can take comes
/// before the timeout, because a known reason beats "no longer waiting".
/// The timeout comes last.
///
/// `stopping` is [`job_pod::stopping_pods`] of the whole cluster, read only
/// while the pod has no room: while it is not empty, the room those pods
/// give back may be what the runner waits for, and the clock does not run.
/// It applies to a lack of room only; a node condition or another rule of
/// the nodes does not change as pods leave.
fn wait_step(
    job: Option<&Value>,
    pods: &[Value],
    stopping: &[String],
    clock: &mut UnschedulableClock,
    now: std::time::Instant,
    waited: Duration,
    timeout: Duration,
) -> WaitStep {
    let Some(job) = job else {
        return WaitStep::Vanished;
    };
    match job_run_outcome(job) {
        JobOutcome::Succeeded => return WaitStep::Succeeded,
        JobOutcome::Failed(why) => return WaitStep::Failed(why),
        JobOutcome::Running => {}
    }
    let pod = job_pod(job, pods);
    let cause = match &pod {
        JobPod::Unschedulable { message, .. } => Some(unplaced(message)),
        _ => None,
    };
    if cause == Some(Unplaced::NoRoom) && !stopping.is_empty() {
        clock.reset();
        if waited >= timeout {
            return WaitStep::TimedOut(pod);
        }
        return WaitStep::RoomReturning {
            pod,
            stopping: stopping.to_vec(),
        };
    }
    let unschedulable_for = clock.observe(&pod, now);
    if let (JobPod::Unschedulable { message, .. }, Some(cause), Some(for_)) =
        (&pod, cause, unschedulable_for)
    {
        if for_ >= grace_for(&cause) {
            return WaitStep::Unschedulable {
                message: message.clone(),
                cause,
                requests: job_pod::runner_requests(job),
                for_,
                failed: job
                    .pointer("/status/failed")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                last_failure: job_pod::last_failed_attempt(job, pods),
            };
        }
    }
    if waited >= timeout {
        return WaitStep::TimedOut(pod);
    }
    WaitStep::Wait(pod, unschedulable_for)
}

/// Poll a backup Job to its terminal state, reporting progress while it runs.
///
/// A timeout is NOT a failure of the backup: the Job keeps running in the
/// cluster, and saying otherwise would send an operator to clean up after a
/// backup that is still in progress. The message says so and hands over the
/// two commands that follow it. It exits 0 only while no attempt of the Job
/// has failed: once one has, the timeout is an error that says why the last
/// one failed ([`job_pod::timed_out_failing`]), because a Job whose attempts
/// fail has taken no backup, and each attempt can take many minutes to fail.
/// Each failed attempt is also said as soon as it is seen, with its reason
/// ([`job_pod::failed_attempt_note`]).
///
/// A pod no node takes is different: it is not a backup in progress. Once it
/// has been unschedulable for [`grace_for`] its reason ([`job_pod::UNSCHEDULABLE_GRACE`]
/// for a lack of room, longer for a condition of the node that lifts by
/// itself), this deletes the Job and fails with the scheduler's reason. Time
/// in which pods elsewhere are stopping does not count toward a lack of room.
/// Deleting the Job matters. Left in place, it would start on its own
/// whenever a node took it, at a time nobody chose and possibly beside the
/// scheduled backup, where two runs that need the same helper pod do not
/// both finish. On a chart whose Job has no deadline it would also never go
/// away.
fn wait_for_backup_job(name: &str, timeout_minutes: u64, kubeconfig: &Path) -> Result<()> {
    let deadline = Duration::from_secs(timeout_minutes * 60);
    let started = std::time::Instant::now();
    let mut last_note = std::time::Instant::now();
    let mut clock = UnschedulableClock::default();
    let mut noted_stopping = false;
    let mut noted_failures: u64 = 0;

    println!(
        "  waiting for it to finish (up to {timeout_minutes}m; Ctrl-C is safe — the Job \
              keeps running)"
    );
    loop {
        let job = kubectl_get_json("job", Some(name), Some(PLATFORMSTACK_NAMESPACE), kubeconfig)?;
        // The pods are read only while the Job has not finished: they are
        // what tells a runner that is working from one that never started.
        let pods = match &job {
            Some(j) if job_run_outcome(j) == JobOutcome::Running => {
                kubectl_get_json("pods", None, Some(PLATFORMSTACK_NAMESPACE), kubeconfig)?
                    .as_ref()
                    .map(items_of)
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        // Pods stopping anywhere in the cluster, read only while the pod has
        // no room: that is when the room they give back matters. Best-effort:
        // without the listing the wait gives up at the usual time.
        let stopping = match &job {
            Some(j)
                if matches!(
                    &job_pod(j, &pods),
                    JobPod::Unschedulable { message, .. } if unplaced(message) == Unplaced::NoRoom
                ) =>
            {
                kubectl_get_json_cluster_wide("pods", None, kubeconfig)
                    .ok()
                    .flatten()
                    .as_ref()
                    .map(|l| job_pod::stopping_pods(&items_of(l)))
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let now = std::time::Instant::now();
        let step = wait_step(
            job.as_ref(),
            &pods,
            &stopping,
            &mut clock,
            now,
            started.elapsed(),
            deadline,
        );
        let was_stopping = std::mem::replace(
            &mut noted_stopping,
            matches!(step, WaitStep::RoomReturning { .. }),
        );
        // A failed attempt of a Job that has not finished: said once, with
        // its reason. A finished Job reports its own ending below.
        let failures = unfinished_failures(job.as_ref(), &step);
        let why = match &job {
            Some(j)
                if failures > 0
                    && (failures > noted_failures || matches!(step, WaitStep::TimedOut(_))) =>
            {
                failed_attempt_reason(j, &pods, kubeconfig)
            }
            _ => None,
        };
        if failures > noted_failures {
            if let Some(j) = &job {
                let attempts = j
                    .pointer("/spec/backoffLimit")
                    .and_then(Value::as_u64)
                    .unwrap_or(6)
                    + 1;
                println!(
                    "{}",
                    job_pod::failed_attempt_note(failures, attempts, why.as_deref())
                );
            }
            noted_failures = failures;
        }
        match step {
            WaitStep::Succeeded => {
                println!(
                    "✓ Backup complete in {}.",
                    format_elapsed(started.elapsed().as_secs())
                );
                println!("  `apprafter backup list` shows the new snapshot.");
                return Ok(());
            }
            WaitStep::Failed(why) => {
                print_job_log_tail(name, kubeconfig);
                return Err(CliError::Other(format!(
                    "backup Job {name} failed after {}: {why}\n  \
                     Full log: kubectl -n {PLATFORMSTACK_NAMESPACE} logs job/{name}",
                    format_elapsed(started.elapsed().as_secs())
                )));
            }
            // A Job that vanished mid-wait was deleted by someone else;
            // reporting success or failure would both be guesses.
            WaitStep::Vanished => {
                return Err(CliError::Other(format!(
                    "backup Job {name} disappeared while waiting for it — someone or something \
                     deleted it. `apprafter backup status` shows what the cluster has now."
                )));
            }
            WaitStep::Unschedulable {
                message,
                cause,
                requests,
                for_,
                failed,
                last_failure,
            } => {
                // Another runner that is running holds room of the same size,
                // and may be the scheduled backup itself: the report names it
                // rather than say the scheduled backup cannot start.
                // Best-effort: without the listing the report is the general one.
                let holders = if cause == Unplaced::NoRoom {
                    kubectl_get_json("jobs", None, Some(PLATFORMSTACK_NAMESPACE), kubeconfig)
                        .ok()
                        .flatten()
                        .map(|l| room_holders(name, &backup_jobs_of(Some(&l)), &pods))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let give_up = job_pod::GiveUp {
                    namespace: PLATFORMSTACK_NAMESPACE,
                    name,
                    message: &message,
                    cause: &cause,
                    requests: requests.as_deref(),
                    holders: &holders,
                    failed,
                    last_failure: last_failure.as_deref(),
                };
                let deleted = kubectl_delete("job", name, PLATFORMSTACK_NAMESPACE, kubeconfig)
                    .map_err(|e| e.to_string());
                print!("{}", job_pod::unschedulable_report(&give_up, deleted));
                let (what, help) = job_pod::give_up_error(&give_up, for_);
                return Err(CliError::BackupRunnerUnschedulable {
                    job: name.to_string(),
                    what,
                    help,
                });
            }
            WaitStep::TimedOut(pod) => {
                println!(
                    "{}",
                    job_pod::timeout_note(&pod, timeout_minutes, PLATFORMSTACK_NAMESPACE, name)
                );
                return timed_out(name, timeout_minutes, failures, why.as_deref());
            }
            WaitStep::Wait(pod, unschedulable_for) => {
                // A new streak of "cannot be scheduled" is said at once, not
                // up to 30 s later: it is the one state with a countdown.
                if last_note.elapsed() >= Duration::from_secs(30)
                    || unschedulable_for == Some(Duration::ZERO)
                {
                    println!(
                        "{}",
                        job_pod::progress_note(&pod, started.elapsed(), unschedulable_for)
                    );
                    last_note = now;
                }
            }
            WaitStep::RoomReturning { stopping, .. } => {
                if last_note.elapsed() >= Duration::from_secs(30) || !was_stopping {
                    println!("{}", job_pod::stopping_note(&stopping, started.elapsed()));
                    last_note = now;
                }
            }
        }
        thread::sleep(JOB_POLL_INTERVAL);
    }
}

/// How many attempts of `job` have failed while it has not finished: its
/// `status.failed` on a step that goes on waiting or ends the wait at the
/// timeout, and 0 on any other, where the Job's own ending (or its pod no
/// node takes) is the report. Pure.
fn unfinished_failures(job: Option<&Value>, step: &WaitStep) -> u64 {
    match (job, step) {
        (Some(j), WaitStep::Wait(..) | WaitStep::RoomReturning { .. } | WaitStep::TimedOut(_)) => j
            .pointer("/status/failed")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        _ => 0,
    }
}

/// How the wait ends at its `--timeout`: `Ok` while no attempt has failed,
/// an error naming the last one's reason once `failures` have
/// ([`job_pod::timed_out_failing`]). Pure.
fn timed_out(name: &str, timeout_minutes: u64, failures: u64, why: Option<&str>) -> Result<()> {
    if failures == 0 {
        return Ok(());
    }
    Err(CliError::Other(job_pod::timed_out_failing(
        name,
        timeout_minutes,
        failures,
        why,
    )))
}

/// Why the newest failed attempt of `job` failed: the runner's own
/// `lastError` when it recorded one during that attempt
/// ([`job_pod::runner_error_during`]), else what its pod says
/// ([`job_pod::last_failed_attempt`]). Best-effort: a status ConfigMap that
/// cannot be read leaves the pod's reason.
fn failed_attempt_reason(job: &Value, pods: &[Value], kubeconfig: &Path) -> Option<String> {
    let record = kubectl_get_json(
        "configmap",
        Some("apprafter-backup-status"),
        Some(PLATFORMSTACK_NAMESPACE),
        kubeconfig,
    )
    .ok()
    .flatten();
    job_pod::runner_error_during(job, pods, record.as_ref())
        .or_else(|| job_pod::last_failed_attempt(job, pods))
}

/// `Xm Ys`, or `Ys` under a minute. Pure.
fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// Print the tail of a failed Job's log, best-effort.
///
/// Best-effort on purpose: the Job's failure is the finding, and a pod that
/// was already garbage-collected must not turn a clear "the backup failed"
/// into an error about fetching logs.
fn print_job_log_tail(name: &str, kubeconfig: &Path) {
    let out = Command::new("kubectl")
        .args([
            "logs",
            &format!("job/{name}"),
            "-n",
            PLATFORMSTACK_NAMESPACE,
            "--tail=30",
        ])
        .env("KUBECONFIG", kubeconfig)
        .output();
    if let Ok(out) = out {
        let text = String::from_utf8_lossy(&out.stdout);
        if !text.trim().is_empty() {
            println!("  --- last 30 log lines ---");
            for line in text.lines() {
                println!("  | {line}");
            }
        }
    }
}

/// Which repository `apprafter backup list` reads.
///
/// The default follows the cluster: once a schedule is writing snapshots
/// off-site, those ARE the cluster's backups, and listing the local
/// repository instead answers a question nobody asked — the one that made a
/// freshly-enabled schedule look like it had not worked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListRepo {
    /// `--repo <url|path>`, verbatim.
    Explicit(String),
    /// The local repository `backup create` writes by default.
    Local,
    /// The off-site repository `spec.backup.bucket` names.
    OffSite(String),
}

/// Decide which repository to list. Pure — the impure caller supplies
/// `spec_backup` (`None` when there is no cluster to read it from, which is
/// the disaster-recovery case and must stay usable).
fn choose_list_repo(
    repo_override: Option<&str>,
    local: bool,
    spec_backup: Option<&Value>,
) -> ListRepo {
    if let Some(r) = repo_override {
        return ListRepo::Explicit(r.to_string());
    }
    if local {
        return ListRepo::Local;
    }
    let enabled = spec_backup
        .and_then(|s| s.pointer("/enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let bucket = spec_backup
        .and_then(|s| s.pointer("/bucket"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match (enabled, bucket) {
        (true, Some(b)) => ListRepo::OffSite(b.to_string()),
        _ => ListRepo::Local,
    }
}

/// How a listing attributes each snapshot to a cluster (E2).
///
/// A restic repository can legitimately be shared, so a listing that shows
/// everything without saying whose it is answers a different question than the
/// one asked. `this` is the reader's own `kube-system` UID — `None` when there
/// was no cluster to ask, in which case nothing can be narrowed and the output
/// says so rather than pretending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClusterView<'a> {
    pub this: Option<&'a str>,
    /// `--all-clusters`: show every cluster's snapshots, not just this one's.
    pub all: bool,
}

/// The snapshots a listing shows, and how many it withheld.
#[derive(Debug)]
pub(crate) struct ListingScope<'a> {
    pub shown: Vec<&'a Value>,
    /// Snapshots belonging to another identified cluster, not displayed.
    pub hidden: usize,
}

/// Sort a listing newest-first, in place.
///
/// restic returns its snapshots oldest-first, and every listing passed that
/// order straight through — so the snapshot an operator wants first, the
/// most recent one, was the one furthest from the prompt, and on a
/// repository with months of history it was off the screen entirely. The
/// question a listing answers is almost always "what is the latest?".
///
/// Times are parsed rather than compared as strings: restic writes an
/// offset, and `2026-09-11T03:00:00+02:00` sorts before
/// `2026-09-11T02:00:00Z` lexically while being the later instant.
///
/// A snapshot whose `time` will not parse keeps its relative order and goes
/// last — it cannot be placed, and dropping or hoisting it would both be
/// worse than showing it at the end.
fn sort_newest_first(shown: &mut [&Value]) {
    shown.sort_by(|a, b| {
        let key = |s: &Value| {
            s.pointer("/time")
                .and_then(Value::as_str)
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(|d| d.with_timezone(&chrono::Utc))
        };
        match (key(a), key(b)) {
            (Some(x), Some(y)) => y.cmp(&x),
            // `None` is "unplaceable", which sorts after everything placeable.
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
}

/// Narrow a repository listing to what this cluster owns, newest first. Pure.
///
/// INVARIANT: `hidden` counts ONLY snapshots that carry another cluster's UID.
/// Legacy snapshots (no UID at all) are shown and marked, never hidden — an
/// operator choosing a snapshot to restore has to be able to see the ones being
/// attributed to them by assumption.
pub(crate) fn narrow_to_cluster<'a>(
    snapshots: &'a [Value],
    view: ClusterView<'_>,
) -> ListingScope<'a> {
    let Some(uid) = view.this.filter(|_| !view.all) else {
        let mut shown: Vec<&Value> = snapshots.iter().collect();
        sort_newest_first(&mut shown);
        return ListingScope { shown, hidden: 0 };
    };
    let mut shown = Vec::new();
    let mut hidden = 0;
    for s in snapshots {
        if classify_snapshot(&tags_of_snapshot(s), uid) == SnapshotOrigin::OtherCluster {
            hidden += 1;
        } else {
            shown.push(s);
        }
    }
    sort_newest_first(&mut shown);
    ListingScope { shown, hidden }
}

/// The tag list restic reports for a snapshot document.
fn tags_of_snapshot(s: &Value) -> Vec<String> {
    s.pointer("/tags")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The CLUSTER cell for one snapshot: its restic host (the human cluster name
/// `backup enable` set) plus a marker when the attribution is not certain.
///
/// `(legacy)` is the load-bearing one. Those snapshots carry no identity, are
/// treated as this cluster's by a stated assumption, and the operator has to be
/// able to see WHICH rows that assumption is being applied to — otherwise the
/// widening is silent, which is the thing it must not be.
pub(crate) fn cluster_cell(s: &Value, view: ClusterView<'_>) -> String {
    let host = s
        .pointer("/hostname")
        .and_then(Value::as_str)
        .filter(|h| !h.is_empty())
        .unwrap_or("?");
    match view.this {
        None => host.to_string(),
        Some(uid) => match classify_snapshot(&tags_of_snapshot(s), uid) {
            SnapshotOrigin::ThisCluster => host.to_string(),
            SnapshotOrigin::Legacy => format!("{host} (legacy)"),
            SnapshotOrigin::OtherCluster => format!("{host} (other)"),
        },
    }
}

/// Render a tag for a column: a leading cluster UID is abbreviated to its first
/// eight characters, and a trailing RFC3339 stamp is rendered the way the TIME
/// column renders one. The whole UUID is 36 characters of noise in a table whose
/// job is comparison, and nothing an operator types takes a tag — restic works
/// on snapshot ids.
///
/// Both halves of a tag were machine text. `<uuid>-2026-09-11T03:00:00Z` put a
/// second, differently-formatted timestamp on the same row as the TIME column,
/// in the one format a reader has to decode — so one line reported one moment
/// twice and disagreed with itself about how a moment looks.
///
/// The stamp is reformatted rather than dropped: it is the backup run's own
/// stamp, not restic's write time, and the two can differ on a slow run.
fn short_tag<Tz>(tag: &str, tz: &Tz) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    let rendered = render_trailing_timestamp(tag, tz);
    match backup_core::cluster::tag_cluster_uid(&rendered) {
        Some(uid) => format!("{}…{}", &uid[..8], &rendered[uid.len()..]),
        None => rendered,
    }
}

/// Reformat an RFC3339 stamp embedded at the end of `text`, leaving the rest
/// untouched. Returns `text` unchanged when there is none.
///
/// Scans `-` boundaries left to right and takes the first suffix that parses.
/// Left to right is correct even though a date is full of hyphens: the first
/// one that parses is the start of the date, and anything earlier fails on the
/// prefix before it.
fn render_trailing_timestamp<Tz>(text: &str, tz: &Tz) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    for (i, _) in text.match_indices('-') {
        let suffix = &text[i + 1..];
        if chrono::DateTime::parse_from_rfc3339(suffix).is_ok() {
            return format!("{}-{}", &text[..i], format_timestamp(suffix, tz));
        }
    }
    text.to_string()
}

/// The lines printed under a listing to explain what it did and did not show.
/// Pure — extracted so the explanation is pinned by a test rather than a walk.
pub(crate) fn listing_footnotes(
    scope: &ListingScope<'_>,
    view: ClusterView<'_>,
    any_legacy: bool,
) -> Vec<String> {
    let mut out = Vec::new();
    if scope.hidden > 0 {
        out.push(format!(
            "  {} snapshot(s) belong to another cluster and are not shown — `--all-clusters` \
             lists them.",
            scope.hidden
        ));
    }
    if view.this.is_none() {
        out.push(
            "  No cluster to compare against, so every snapshot in the repository is listed."
                .to_string(),
        );
    }
    // With `--all-clusters` the listing is the whole repository, so this is
    // the one place that can answer "which clusters are in here" in full. The
    // TAGS column abbreviates each UID to eight characters (36 characters of
    // UUID in a comparison table is noise), and `backup prune --cluster-uid`
    // needs the whole thing — this is where an operator reads it off.
    if view.all {
        let uids = backup_core::cluster::cluster_uids_in(scope.shown.iter().copied());
        if !uids.is_empty() {
            out.push(format!(
                "  cluster identities in this repository: {}.",
                uids.join(", ")
            ));
        }
    }
    if any_legacy {
        out.push(
            "  (legacy) — written before snapshots carried a cluster identity; treated as this \
             cluster's by assumption."
                .to_string(),
        );
    }
    out
}

/// `apprafter backup list` — list the snapshots in a restic repo.
///
/// With no flags it lists whatever the cluster's schedule writes; `--local`
/// lists the repository `backup create` writes on this machine, and `--repo`
/// names one directly. A cluster that cannot be reached is not an error —
/// listing falls back to the local repository and says so, because verifying
/// a repo when the cluster is gone is the case this command matters in.
///
/// The listing is narrowed to THIS cluster's snapshots (E2): a shared
/// repository holds runs an operator must not mistake for their own.
/// `--all-clusters` shows the rest, which is how you find the id to pass to
/// `restore --snapshot` when you genuinely mean a different cluster's run.
pub fn run_backup_list(
    repo: Option<&str>,
    passphrase: Option<&str>,
    local: bool,
    credential_file: Option<&Path>,
    details: bool,
    all_clusters: bool,
) -> Result<()> {
    // D11 / 2.22a: the external binaries this command spawns, checked
    // BEFORE any prompt, kubeconfig or provider call. The reported bug
    // was a passphrase typed into a command that could not have worked.
    preflight_tools(&[&RESTIC], "apprafter backup list")?;

    // Best-effort: no cluster, no target, an unreachable apiserver — all
    // mean "no schedule to follow", never a failure to list. Attempted even
    // for `--repo` / `--local`, which do not NEED a cluster, because the
    // cluster is also where the identity that narrows the listing comes from.
    let kc = ensure_kubeconfig_tempfile().ok();
    let spec_backup = match (repo.is_some() || local, kc.as_ref()) {
        (false, Some(kc)) => spec_backup_from_cluster(Some(kc.path())).unwrap_or(None),
        _ => None,
    };
    // Best-effort too: a cluster that cannot name itself lists everything and
    // says so. Refusing to list would take the command away exactly when a
    // repository has to be inspected without its cluster.
    let this_uid = kc.as_ref().and_then(|kc| read_cluster_uid(kc.path()).ok());
    let view = ClusterView {
        this: this_uid.as_deref(),
        all: all_clusters,
    };

    let zone = readers_zone();

    match choose_list_repo(repo, local, spec_backup.as_ref()) {
        ListRepo::OffSite(repo_url) => {
            let creds = resolve_verb_creds(
                credential_file,
                kc.as_ref().map(|f| f.path()),
                spec_backup.as_ref(),
            )?;
            let pass = creds["RESTIC_PASSWORD"].clone();
            let runner = CredentialedRestic { creds };
            let json = runner.run_stdout(&restic_snapshots_argv(&repo_url), &pass)?;
            let snapshots = parse_snapshots_json(&json)?;
            let scope = narrow_to_cluster(&snapshots, view);
            if details {
                let rows = collect_snapshot_details(&runner, &repo_url, &pass, &scope.shown, view);
                print!(
                    "{}",
                    format_detail_table(&repo_url, &rows, &chrono::Local, zone.as_deref())
                );
            } else {
                print!(
                    "{}",
                    format_snapshot_table(
                        &repo_url,
                        &scope.shown,
                        &chrono::Local,
                        zone.as_deref(),
                        view
                    )
                );
            }
            print_listing_footnotes(&scope, view);
        }
        chosen => {
            let repo_str = match &chosen {
                ListRepo::Explicit(r) => r.clone(),
                _ => {
                    let resolved = resolve_state_paths(None)?;
                    backup_repo_path(None, &resolved.target_name)?
                        .to_string_lossy()
                        .to_string()
                }
            };
            let env_pass = std::env::var("RESTIC_PASSWORD").ok();
            let is_tty = std::io::stdin().is_terminal();
            let pass = backup_passphrase_or_error(passphrase, env_pass.as_deref(), is_tty)?;

            let r = SubprocessRestic;
            let json = r.run_stdout(&restic_snapshots_argv(&repo_str), &pass)?;
            let snapshots = parse_snapshots_json(&json)?;
            let scope = narrow_to_cluster(&snapshots, view);
            print!(
                "{}",
                format_snapshot_table(
                    &repo_str,
                    &scope.shown,
                    &chrono::Local,
                    zone.as_deref(),
                    view
                )
            );
            print_listing_footnotes(&scope, view);
        }
    }
    Ok(())
}

/// Print what [`listing_footnotes`] computed for the rows just rendered.
fn print_listing_footnotes(scope: &ListingScope<'_>, view: ClusterView<'_>) {
    let any_legacy = view.this.is_some_and(|uid| {
        scope
            .shown
            .iter()
            .any(|s| classify_snapshot(&tags_of_snapshot(s), uid) == SnapshotOrigin::Legacy)
    });
    for line in listing_footnotes(scope, view, any_legacy) {
        println!("{line}");
    }
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

// ---------------------------------------------------------------------------
// Repository size + snapshot contents
//
// "Is the backup working" and "what is in the backup" are different
// questions, and until now the CLI could only answer the first. A snapshot
// id and a timestamp say a run happened; they do not say whether it captured
// the four applications the cluster actually has.
// ---------------------------------------------------------------------------

/// Bytes at the scale a reader thinks in. Binary units, because that is what
/// restic counts in and what an object store bills against.
fn human_size(bytes: u64) -> String {
    const UNITS: &[(&str, u64)] = &[
        ("GiB", 1024 * 1024 * 1024),
        ("MiB", 1024 * 1024),
        ("KiB", 1024),
    ];
    for (unit, scale) in UNITS {
        if bytes >= *scale {
            return format!("{:.1} {unit}", bytes as f64 / *scale as f64);
        }
    }
    format!("{bytes} B")
}

/// What `restic stats --json --mode raw-data` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResticStats {
    /// Bytes actually stored, after dedup and compression.
    pub total_size: u64,
    /// Snapshots counted — absent when stats was asked about ONE snapshot.
    pub snapshots_count: Option<u64>,
}

/// Parse a `restic stats --json` document. `None` when it is not JSON or
/// carries no size — a stats call that failed to say anything useful must
/// not render as a repository of zero bytes.
fn parse_stats_json(raw: &str) -> Option<ResticStats> {
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    Some(ResticStats {
        total_size: v.pointer("/total_size").and_then(Value::as_u64)?,
        snapshots_count: v.pointer("/snapshots_count").and_then(Value::as_u64),
    })
}

/// Find `manifest.json` in a `restic ls --json` stream.
///
/// The runner snapshots a temp staging directory whose name changes every
/// run, so the manifest has no fixed path — it can only be found by name.
/// `restic ls --json` emits one object per line: the snapshot first, then
/// each node.
fn manifest_path_in_snapshot(ls_output: &str) -> Option<String> {
    ls_output
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|n| n.pointer("/name").and_then(Value::as_str) == Some("manifest.json"))
        .and_then(|n| {
            n.pointer("/path")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

/// Gather per-snapshot size and content counts.
///
/// Three restic calls per snapshot (`stats`, `ls`, `dump`), which is why
/// this is behind a flag rather than the default. A snapshot whose stats or
/// manifest cannot be read still gets a row with dashes: one unreadable
/// snapshot in a listing of ten must not take the other nine with it.
fn collect_snapshot_details(
    runner: &CredentialedRestic,
    repo: &str,
    pass: &str,
    snapshots: &[&Value],
    view: ClusterView<'_>,
) -> Vec<SnapshotDetail> {
    snapshots
        .iter()
        .map(|s| {
            let id = s
                .pointer("/short_id")
                .or_else(|| s.pointer("/id"))
                .and_then(Value::as_str)
                .map(|i| i.chars().take(8).collect::<String>())
                .unwrap_or_else(|| "?".to_string());
            let time = s
                .pointer("/time")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            let size = repo_stats(runner, repo, pass, Some(&id)).map(|st| st.total_size);
            let counts = read_snapshot_insides(runner, repo, pass, &id)
                .ok()
                .map(|i| content_counts(&i.manifest, i.secret_files));
            SnapshotDetail {
                id,
                time,
                cluster: cluster_cell(s, view),
                size,
                counts,
            }
        })
        .collect()
}

/// One row of `backup list --details`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotDetail {
    pub id: String,
    pub time: String,
    /// The CLUSTER cell — see [`cluster_cell`].
    pub cluster: String,
    /// `None` when restic could not be asked — rendered as a dash, never
    /// as a zero, because "unknown" and "none" are different answers.
    pub size: Option<u64>,
    pub counts: Option<ContentCounts>,
}

/// The three counts an operator scans a listing for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContentCounts {
    pub apps: u64,
    pub secrets: u64,
    pub claims: u64,
}

/// Count Applications, Secrets and ResourceClaims. Pure.
///
/// `secret_files` comes from the snapshot TREE rather than the manifest,
/// which never lists secrets — counting them there reported 0 for every
/// backup ever taken.
fn content_counts(manifest: &Value, secret_files: u64) -> ContentCounts {
    let mut c = ContentCounts {
        apps: 0,
        secrets: secret_files,
        claims: 0,
    };
    for r in manifest
        .pointer("/resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match r.pointer("/kind").and_then(Value::as_str) {
            Some("Application") => c.apps += 1,
            Some("ResourceClaim") => c.claims += 1,
            _ => {}
        }
    }
    c
}

/// Render `backup list --details`: a row per snapshot with size and the
/// counts, so two runs can be compared down the columns.
fn format_detail_table<Tz>(
    repo: &str,
    rows: &[SnapshotDetail],
    tz: &Tz,
    zone_label: Option<&str>,
) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    if rows.is_empty() {
        return format!("No snapshots in {repo}.\n");
    }
    let time_header = format!("TIME ({})", zone_label.unwrap_or("local"));

    /// One row, already rendered to strings — a named struct rather than
    /// a six-tuple so the column-width helper below stays readable.
    struct Rendered {
        id: String,
        time: String,
        cluster: String,
        size: String,
        apps: String,
        secrets: String,
        claims: String,
    }

    let rendered: Vec<Rendered> = rows
        .iter()
        .map(|r| {
            let (apps, secrets, claims) = match r.counts {
                Some(c) => (
                    c.apps.to_string(),
                    c.secrets.to_string(),
                    c.claims.to_string(),
                ),
                // A dash, never a zero: "unknown" and "none" are
                // different answers, and a listing read for change must
                // not invent the second when it means the first.
                None => ("—".into(), "—".into(), "—".into()),
            };
            Rendered {
                id: r.id.clone(),
                time: format_timestamp(&r.time, tz),
                cluster: r.cluster.clone(),
                size: r.size.map(human_size).unwrap_or_else(|| "—".into()),
                apps,
                secrets,
                claims,
            }
        })
        .collect();

    let width = |header: &str, pick: &dyn Fn(&Rendered) -> &String| {
        rendered
            .iter()
            .map(|r| pick(r).chars().count())
            .chain(std::iter::once(header.chars().count()))
            .max()
            .unwrap_or(header.len())
    };
    let id_w = width("ID", &|r| &r.id);
    let time_w = width(&time_header, &|r| &r.time);
    let cluster_w = width("CLUSTER", &|r| &r.cluster);
    let size_w = width("SIZE", &|r| &r.size);
    let apps_w = width("APPS", &|r| &r.apps);
    let sec_w = width("SECRETS", &|r| &r.secrets);

    let mut out = format!(
        "Snapshots in {repo}:\n{:<id_w$}  {:<time_w$}  {:<cluster_w$}  {:>size_w$}  {:>apps_w$}  \
         {:>sec_w$}  CLAIMS\n",
        "ID", time_header, "CLUSTER", "SIZE", "APPS", "SECRETS"
    );
    for r in rendered {
        out.push_str(&format!(
            "{:<id_w$}  {:<time_w$}  {:<cluster_w$}  {:>size_w$}  {:>apps_w$}  {:>sec_w$}  {}\n",
            r.id, r.time, r.cluster, r.size, r.apps, r.secrets, r.claims
        ));
    }
    out
}

/// Count the secret files a snapshot carries.
///
/// The manifest lists CRs and claims and NOTHING else — `resource_refs`
/// never adds secrets — so a count taken from it reports zero for every
/// backup ever made. The secrets are in the tree, as
/// `secrets/<namespace>/<name>.json` plus `secrets/sourcecred/<name>.json`,
/// and that is where the number has to come from.
fn count_secret_files(ls_output: &str) -> u64 {
    ls_output
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|n| n.pointer("/type").and_then(Value::as_str) == Some("file"))
        .filter(|n| {
            n.pointer("/path")
                .and_then(Value::as_str)
                .is_some_and(|p| p.contains("/secrets/"))
        })
        .count() as u64
}

/// Render what a snapshot contains, from its manifest.
///
/// Counts by `kind`, and breaks `ResourceClaim` down by `claimType` —
/// "ResourceClaims 3" does not answer "which databases are in there", which
/// is the question that gets asked. An unknown kind is counted under its own
/// name rather than dropped: a listing that silently omits resources is
/// worse than one naming something the reader has to look up.
fn format_snapshot_contents<Tz>(
    snapshot_id: &str,
    time: Option<&str>,
    size: Option<u64>,
    manifest: &Value,
    secret_files: Option<u64>,
    tz: &Tz,
    zone_label: Option<&str>,
) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    let mut out = match time {
        Some(t) => format!(
            "Snapshot {snapshot_id} — {}\n",
            format_timestamp_with_zone(t, tz, zone_label)
        ),
        None => format!("Snapshot {snapshot_id}\n"),
    };

    let field = |k: &str| manifest.pointer(k).and_then(Value::as_str).unwrap_or("?");
    out.push_str(&format!("  cluster:        {}\n", field("/clusterId")));
    out.push_str(&format!(
        "  platform-stack: {}\n",
        field("/platformVersion")
    ));
    if let Some(b) = size {
        out.push_str(&format!("  size:           {} (raw)\n", human_size(b)));
    }
    let namespaces: Vec<&str> = manifest
        .pointer("/namespaces")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !namespaces.is_empty() {
        out.push_str(&format!("  namespaces:     {}\n", namespaces.join(", ")));
    }
    // Secrets follow the SealedSecrets, so they can come from namespaces
    // that hold no Application. Printed only when it differs — a second
    // line repeating the first teaches the reader to skip both.
    let secret_namespaces: Vec<&str> = manifest
        .pointer("/secretNamespaces")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !secret_namespaces.is_empty() && secret_namespaces != namespaces {
        out.push_str(&format!(
            "  secrets from:   {}\n",
            secret_namespaces.join(", ")
        ));
    }

    let resources = manifest
        .pointer("/resources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if resources.is_empty() && secret_files.unwrap_or(0) == 0 {
        out.push_str("  (no resources recorded in this snapshot's manifest)\n");
        return out;
    }

    // BTreeMap so the order is stable run to run — a listing meant for
    // spotting a change must not reorder itself between two runs.
    let mut by_kind: BTreeMap<&str, u64> = BTreeMap::new();
    // Secrets come from the tree, never from `resources` — see
    // `count_secret_files`. Counted first so a manifest that ever starts
    // listing them cannot double them.
    if let Some(n) = secret_files {
        by_kind.insert("Secret", n);
    }
    let mut claims_by_type: BTreeMap<&str, u64> = BTreeMap::new();
    for r in &resources {
        let kind = r.pointer("/kind").and_then(Value::as_str).unwrap_or("?");
        if kind == "Secret" && secret_files.is_some() {
            continue;
        }
        *by_kind.entry(kind).or_default() += 1;
        if kind == "ResourceClaim" {
            // `ResourceRef` carries no `rename_all`, so the manifest
            // spells this `claim_type`. The camelCase form is accepted
            // too: if the struct ever gains a rename, this keeps
            // reading rather than quietly reporting "unspecified".
            let t = r
                .pointer("/claim_type")
                .or_else(|| r.pointer("/claimType"))
                .and_then(Value::as_str)
                .unwrap_or("unspecified");
            *claims_by_type.entry(t).or_default() += 1;
        }
    }

    out.push_str("  contents:\n");
    let width = by_kind.keys().map(|k| k.len()).max().unwrap_or(4);
    for (kind, n) in &by_kind {
        if *kind == "ResourceClaim" && !claims_by_type.is_empty() {
            let breakdown: Vec<String> = claims_by_type
                .iter()
                .map(|(t, c)| format!("{t} {c}"))
                .collect();
            out.push_str(&format!(
                "    {kind:<width$}  {n}  ({})\n",
                breakdown.join(", ")
            ));
        } else {
            out.push_str(&format!("    {kind:<width$}  {n}\n"));
        }
    }
    // A6: the claim counts above say a claim is in the snapshot; they cannot
    // say whether its DATA is. For the types that have no capture path, this
    // is the difference between a snapshot that reads complete and one that is
    // — so it is stated here, under the listing it qualifies.
    let manifest_version = manifest
        .pointer("/manifestVersion")
        .and_then(Value::as_u64)
        // Absent means the format that predates the field, which is v1 — the
        // same reading `manifest::default_manifest_version` makes, and for the
        // same reason.
        .unwrap_or(1) as u32;
    for line in uncaptured_claims_lines(
        &uncaptured_claims_of(&resources, manifest_version),
        "snapshot",
    ) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The claims a snapshot's manifest lists as carrying no data of their own
/// (A6). Pure.
///
/// Reads the manifest's own `no_data` marker first — a snapshot should be able
/// to describe itself — and falls back to what this build knows about a
/// snapshot of THAT FORMAT VERSION, so one taken before the marker existed
/// still gets an honest answer instead of reading as complete.
///
/// The version is what makes the fallback right in both directions. A v1
/// snapshot holds no jetstream data, whatever this build can capture now; a v2
/// one holds it unless the marker says otherwise. Without the version, the day
/// jetstream capture shipped every older snapshot would have gone silent about
/// exactly the claims A6 was about.
fn uncaptured_claims_of(resources: &[Value], manifest_version: u32) -> Vec<UncapturedClaim> {
    resources
        .iter()
        .filter(|r| r.pointer("/kind").and_then(Value::as_str) == Some("ResourceClaim"))
        .filter_map(|r| {
            let claim_type = r
                .pointer("/claim_type")
                .or_else(|| r.pointer("/claimType"))
                .and_then(Value::as_str)?;
            let marked = r
                .pointer("/no_data")
                .or_else(|| r.pointer("/noData"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !marked
                && !backup_core::extract::claim_type_has_no_data_in_manifest(
                    claim_type,
                    manifest_version,
                )
            {
                return None;
            }
            Some(UncapturedClaim {
                namespace: r
                    .pointer("/namespace")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name: r
                    .pointer("/name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                claim_type: claim_type.to_string(),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// `apprafter backup set` — change ONE field of a configured backup
//
// `backup enable` composes `spec.backup` wholesale: every CRD-required field
// is written on every run, from flags or from the platform default. That is
// right for configuring backup and wrong for changing it — an operator who
// re-runs `enable` to move the hour also silently resets the verification
// depth, the retention, and anything set outside the CLI. `set` writes one
// key, so what it changes is what it says.
// ---------------------------------------------------------------------------

/// The settable keys, in the order the error lists them.
const BACKUP_SET_KEYS: &[&str] = &[
    "enabled",
    "at",
    "check",
    "cluster-name",
    "check-depth",
    "timezone",
    "keep-daily",
    "keep-weekly",
    "keep-monthly",
    "enforce",
    "staging-mode",
    "failure-webhook",
    "deadline",
    "check-deadline",
];

/// The shortest Job deadline `backup set` writes, and the CRD's own minimum.
const MIN_JOB_DEADLINE_SECS: u64 = 600;

/// Parse a Job deadline written as a whole number of hours, minutes or
/// seconds — `6h`, `90m`, `43200s` — into seconds.
///
/// One unit, no fractions and no bare numbers: a bare `6` is exactly the
/// ambiguity (hours? seconds?) a deadline must not have, since the wrong
/// reading either stops every run or never stops a stuck one.
fn parse_job_deadline(key: &str, value: &str) -> Result<u64> {
    let refuse = |why: &str| {
        CliError::Other(format!(
            "{key} takes a duration like `6h`, `90m` or `43200s` — got `{value}`: {why}. \
             It must stay shorter than the interval between two runs of its schedule, and \
             longer than the slowest run expected to succeed."
        ))
    };
    let per_unit = match value.chars().last() {
        Some('h') => 3600,
        Some('m') => 60,
        Some('s') => 1,
        _ => return Err(refuse("the unit must be h, m or s")),
    };
    // The unit is one ASCII byte, so this slice is on a char boundary.
    let digits = &value[..value.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(refuse("expected a whole number before the unit"));
    }
    let secs = digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(per_unit))
        .filter(|s| i64::try_from(*s).is_ok())
        .ok_or_else(|| refuse("too large"))?;
    if secs < MIN_JOB_DEADLINE_SECS {
        return Err(refuse("the minimum is 10m"));
    }
    Ok(secs)
}

/// Is this a value restic's `--read-data-subset` would accept?
///
/// `x%` / `x.y%`, `n/t`, or a byte size with a k/K/m/M/g/G/t/T suffix.
/// Checked HERE rather than left to restic because the only place restic
/// would report it is inside the weekly Job — 06:00 on a Sunday, as a red
/// Job nobody is watching, with the previous depth still in force.
fn is_read_data_subset(v: &str) -> bool {
    if let Some(pct) = v.strip_suffix('%') {
        return !pct.is_empty()
            && pct.parse::<f64>().is_ok_and(|n| n > 0.0 && n <= 100.0)
            && !pct.starts_with('-');
    }
    if let Some((n, t)) = v.split_once('/') {
        return matches!((n.parse::<u64>(), t.parse::<u64>()), (Ok(n), Ok(t)) if t > 0 && n >= 1 && n <= t);
    }
    // Split before the last CHARACTER, not the last byte: a byte index lands
    // inside a multi-byte character (`5é`) and `split_at` panics there.
    let Some((last, _)) = v.char_indices().next_back() else {
        return false;
    };
    let (digits, suffix) = v.split_at(last);
    matches!(suffix, "k" | "K" | "m" | "M" | "g" | "G" | "t" | "T")
        && !digits.is_empty()
        && digits.parse::<u64>().is_ok_and(|n| n > 0)
}

/// Build the merge-patch for `apprafter backup set <key> <value>`. Pure.
///
/// Returns the full `{"spec":{"backup":{…}}}` body so the caller can hand it
/// straight to a JSON merge-patch — which, unlike SSA, leaves every key it
/// does not mention exactly as it was.
fn backup_set_patch(key: &str, value: &str) -> Result<Value> {
    let mut field = serde_json::Map::new();
    match key {
        // The switch, on its own. `backup enable` composes the WHOLE block
        // from its flags, so it cannot flip this without also resetting
        // schedule, timezone, retention and staging mode to whatever the
        // command line and the platform defaults say. Two paths leave a
        // cluster holding a complete, correct, switched-off configuration —
        // `backup disable`, and a restore, which replays the source's block
        // disabled — and both of them need a way back on that changes exactly
        // this one field.
        "enabled" => {
            let on = match value {
                "true" | "on" | "yes" => true,
                "false" | "off" | "no" => false,
                _ => {
                    return Err(CliError::Other(format!(
                        "enabled takes `true` or `false` — got `{value}`"
                    )))
                }
            };
            field.insert("enabled".into(), Value::Bool(on));
        }
        "at" => {
            let (h, m) = parse_at(value)?;
            field.insert("schedule".into(), Value::String(compose_daily(h, m)));
        }
        "check" => {
            // Empty is not missing: it is the value that omits the check
            // CronJob from the render, and the only way to say "no check".
            if value.eq_ignore_ascii_case("off") {
                field.insert("checkSchedule".into(), Value::String(String::new()));
            } else {
                let (h, m) = parse_at(value).map_err(|e| {
                    CliError::Other(format!("{e}").replace("--at", "check") + " (or `off`)")
                })?;
                field.insert(
                    "checkSchedule".into(),
                    Value::String(compose_weekly_sunday(h, m)),
                );
            }
        }
        "check-depth" => {
            // Both fields, every time. Writing only the one that changed
            // would leave the result depending on what was there before.
            let (full, subset) = match value {
                "structure" | "off" | "none" => (false, String::new()),
                "full" | "all" => (true, String::new()),
                v if is_read_data_subset(v) => (false, v.to_string()),
                _ => {
                    return Err(CliError::Other(format!(
                        "check-depth takes `structure`, `full`, or a subset restic understands \
                         (`10%`, `2.5%`, `1/12`, `500M`) — got `{value}`.\n  \
                         structure: metadata only, reads none of the data it certifies.\n  \
                         a subset:  re-hashes that share of the packs each week (10% covers the \
                         repository in ten weeks).\n  \
                         full:      re-hashes everything, every week."
                    )))
                }
            };
            field.insert("checkReadData".into(), Value::Bool(full));
            field.insert("checkReadDataSubset".into(), Value::String(subset));
        }
        "cluster-name" => {
            // The label every snapshot is listed under. Changing it does NOT
            // change this cluster's identity — that is the kube-system UID in
            // the tag — so it is safe to rename at any time, and it is the
            // answer a restored clone needs when it finds it inherited the
            // source's name.
            field.insert(
                "clusterName".into(),
                Value::String(resolve_cluster_name(Some(value), value)?),
            );
        }
        "timezone" => {
            validate_zone_shape(value)?;
            field.insert("timeZone".into(), Value::String(value.to_string()));
        }
        "keep-daily" | "keep-weekly" | "keep-monthly" => {
            let n: u32 = value.parse().map_err(|_| {
                CliError::Other(format!(
                    "{key} takes a positive whole number — got `{value}`"
                ))
            })?;
            if n == 0 {
                return Err(CliError::Other(format!(
                    "{key} must be greater than 0 — a zero would keep nothing at that tier. \
                     To stop keeping a tier at all, leave it unset."
                )));
            }
            let cr_key = match key {
                "keep-daily" => "keepDaily",
                "keep-weekly" => "keepWeekly",
                _ => "keepMonthly",
            };
            let mut retention = serde_json::Map::new();
            retention.insert(cr_key.into(), Value::from(n));
            field.insert("retention".into(), Value::Object(retention));
        }
        "enforce" => {
            if !matches!(value, "check" | "cluster" | "operator") {
                return Err(CliError::Other(format!(
                    "enforce takes `check`, `cluster` or `operator` — got `{value}`.\n  \
                     check:    the weekly check Job prunes after a check that passed, as far \
                     as the cluster's key may delete (the default).\n  \
                     cluster:  the backup Job prunes after every backup; needs a key that may \
                     delete.\n  \
                     operator: nothing in the cluster prunes; run `apprafter backup prune`."
                )));
            }
            let mut retention = serde_json::Map::new();
            retention.insert("enforce".into(), Value::String(value.to_string()));
            field.insert("retention".into(), Value::Object(retention));
        }
        "staging-mode" => {
            if !matches!(value, "monolithic" | "sequential") {
                return Err(CliError::Other(format!(
                    "staging-mode takes `monolithic` or `sequential` — got `{value}`"
                )));
            }
            field.insert("stagingMode".into(), Value::String(value.to_string()));
        }
        "failure-webhook" => {
            field.insert("failureWebhook".into(), Value::String(value.to_string()));
        }
        // How long one Job may run before Kubernetes stops it; see
        // `activeDeadlineSeconds` in the PlatformStack schema. Operator
        // v0.2.52 is the first CRD to define these, and the readback in
        // `run_backup_set` reports an older CRD pruning them.
        "deadline" | "check-deadline" => {
            let secs = parse_job_deadline(key, value)?;
            let cr_key = if key == "deadline" {
                "activeDeadlineSeconds"
            } else {
                "checkActiveDeadlineSeconds"
            };
            field.insert(cr_key.into(), Value::from(secs));
        }
        _ => {
            return Err(CliError::Other(format!(
                "unknown key `{key}`. Settable keys: {}.\n  \
                 The repository and its credential are not among them: pointing an existing \
                 schedule at a different bucket is a new repository, with its own init and its \
                 own first backup, so it goes through `apprafter backup enable`.",
                BACKUP_SET_KEYS.join(", ")
            )))
        }
    }
    Ok(serde_json::json!({"spec": {"backup": Value::Object(field)}}))
}

/// `apprafter backup show [<snapshot>]` — what a snapshot contains.
///
/// The question `list` cannot answer: an id and a timestamp say a run
/// happened, not whether it captured the applications the cluster has. Reads
/// the manifest the runner wrote into the snapshot, so the answer comes from
/// the backup itself rather than from the cluster it was taken from.
///
/// ## The default is not restic's `latest` (E2)
///
/// It used to be, and in a shared repository that displayed whichever cluster
/// wrote last. Read-only, but this is what an operator reads before deciding
/// what to restore, so a foreign answer here becomes a foreign restore. The
/// default now resolves through [`resolve_latest_snapshot`] — the same rule
/// `restore` uses, deliberately the same function — so `show` and `restore`
/// cannot disagree about which snapshot `latest` is.
///
/// That rule also makes `latest` the newest COMPLETE run: a newer sequential
/// run stopped before its commit snapshot has no manifest to show, and is
/// named above the contents instead ([`passed_over_lines`]).
pub fn run_backup_show(
    snapshot: Option<&str>,
    repo_override: Option<&str>,
    credential_file: Option<&Path>,
) -> Result<()> {
    preflight_tools(&[&RESTIC], "apprafter backup show")?;

    let source = cred_source(
        credential_file.is_some(),
        env_creds_complete(&|k| std::env::var(k).ok()),
    );
    let kc = kubeconfig_if_cluster_needed(
        "show",
        repo_override,
        RetentionArgs::NotApplicable,
        source,
        None,
    )?;
    let kc_path = kc.as_ref().map(|f| f.path());
    let spec_backup = spec_backup_from_cluster(kc_path)?;
    let creds = resolve_verb_creds(credential_file, kc_path, spec_backup.as_ref())?;
    let pass = creds["RESTIC_PASSWORD"].clone();
    let repo = repo_from_spec_backup(repo_override, spec_backup.as_ref())?;

    // The identity that narrows `latest`, acquired even on the paths that
    // needed no cluster for anything else (`--repo` + local credentials) —
    // exactly as `backup list` does, and best-effort for the same reason: a
    // repository must stay inspectable when its cluster is gone. With no
    // identity, `resolve_latest_snapshot` falls back to the single-cluster /
    // refuse-if-ambiguous rule, which is the half that still refuses to guess.
    let kc = match kc {
        Some(kc) => Some(kc),
        None => ensure_kubeconfig_tempfile().ok(),
    };
    let this_uid = kc.as_ref().and_then(|kc| read_cluster_uid(kc.path()).ok());

    let runner = CredentialedRestic { creds };
    // The repository listing is fetched ONLY for the default; naming a
    // snapshot must not cost a `restic snapshots` call, and must keep working
    // when the listing is what is broken.
    let listing = match snapshot {
        Some(_) => None,
        None => Some(runner.run_stdout(&restic_snapshots_argv(&repo), &pass)?),
    };
    let (id, passed_over) = snapshot_to_show(snapshot, listing.as_deref(), this_uid.as_deref())?;
    let id = id.as_str();
    let inside = read_snapshot_insides(&runner, &repo, &pass, id)?;

    // The id and time come from `snapshots`, not from the manifest: the
    // manifest records when the RUN started, and an operator matching this
    // against `backup list` needs the snapshot's own id and timestamp.
    let (resolved_id, time) = snapshot_identity(&runner, &repo, &pass, id);
    let size = repo_stats(&runner, &repo, &pass, Some(id)).map(|s| s.total_size);

    let zone = readers_zone();
    for line in passed_over_lines(
        &passed_over,
        &format!("{resolved_id}, shown below"),
        &chrono::Local,
        zone.as_deref(),
    ) {
        println!("{line}");
    }
    print!(
        "{}",
        format_snapshot_contents(
            &resolved_id,
            time.as_deref(),
            size,
            &inside.manifest,
            Some(inside.secret_files),
            &chrono::Local,
            readers_zone().as_deref(),
        )
    );
    Ok(())
}

/// Which snapshot `backup show` inspects: the one the operator named, or —
/// for the default — `latest` resolved inside THIS cluster's history, with
/// the unfinished runs newer than it that `latest` passed over. Pure.
///
/// The seam exists so the default is pinned by a test rather than by a walk
/// against a shared repository, which is the one shape that shows the defect
/// and the one nobody has lying around. `listing` is `None` exactly when a
/// snapshot was named, because then no listing is fetched at all.
fn snapshot_to_show(
    requested: Option<&str>,
    listing: Option<&str>,
    this_cluster_uid: Option<&str>,
) -> Result<(String, Vec<UnfinishedRun>)> {
    if let Some(id) = requested {
        return Ok((id.to_string(), Vec::new()));
    }
    let listing = listing.ok_or_else(|| {
        CliError::Other(
            "internal: `backup show` needs the repository listing to resolve `latest`".into(),
        )
    })?;
    let latest = resolve_latest_snapshot(listing, this_cluster_uid).map_err(CliError::Other)?;
    Ok((latest.id, latest.passed_over))
}

/// What `backup show` and `restore` say when `latest` passed over newer runs
/// that did not finish: the newest backup is not the one being shown or
/// restored, and an operator in the middle of a recovery must not have to
/// find that out from `backup list`. Empty — and silent — when nothing was
/// passed over. `chosen` names the run `latest` resolved to, and what
/// happens to it. Pure.
pub(crate) fn passed_over_lines<Tz>(
    passed_over: &[UnfinishedRun],
    chosen: &str,
    tz: &Tz,
    zone_label: Option<&str>,
) -> Vec<String>
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    if passed_over.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = passed_over
        .iter()
        .map(|run| {
            format!(
                "  ⚠ a newer backup run did not finish: {} snapshot(s) of run {}, the last \
                 written {}. None carries manifest.json — the run was interrupted before its \
                 last snapshot, or is still being written.",
                run.snapshots,
                short_tag(&run.tag, tz),
                format_timestamp_with_zone(&run.newest, tz, zone_label)
            )
        })
        .collect();
    out.push(format!(
        "  `latest` is the newest COMPLETE run: snapshot {chosen}."
    ));
    out
}

/// The short id and timestamp restic itself reports for a snapshot.
///
/// Falls back to the caller's own reference when `snapshots` cannot be read
/// — the contents are the answer here, and losing the header would be a
/// worse outcome than losing the exact id.
///
/// `snapshot` is always a concrete id: `run_backup_show` resolves `latest`
/// itself, through this cluster's own history (E2). There is deliberately no
/// `latest` branch here — restic's alias means "newest in the repository",
/// which in a shared one is whoever wrote last.
fn snapshot_identity(
    runner: &CredentialedRestic,
    repo: &str,
    pass: &str,
    snapshot: &str,
) -> (String, Option<String>) {
    let fallback = (snapshot.to_string(), None);
    let Ok(json) = runner.run_stdout(&restic_snapshots_argv(repo), pass) else {
        return fallback;
    };
    let Ok(list) = parse_snapshots_json(&json) else {
        return fallback;
    };
    let found = list.iter().find(|s| {
        [s.pointer("/short_id"), s.pointer("/id")]
            .iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .any(|v| v.starts_with(snapshot) || snapshot.starts_with(v))
    });
    match found {
        Some(s) => (
            s.pointer("/short_id")
                .or_else(|| s.pointer("/id"))
                .and_then(Value::as_str)
                .map(|i| i.chars().take(8).collect())
                .unwrap_or_else(|| snapshot.to_string()),
            s.pointer("/time")
                .and_then(Value::as_str)
                .map(str::to_string),
        ),
        None => fallback,
    }
}

/// Compare what `set` wrote against what the cluster stored.
///
/// The apiserver PRUNES fields a structural CRD does not define: the patch
/// is accepted, 200 comes back, and the value is gone. `checkReadDataSubset`
/// arrived with operator v0.2.48, so on an older cluster `set check-depth`
/// would print a tick and change nothing — the same silent half-success the
/// `timeZone` read-back exists for.
///
/// `None` when everything the patch wrote is present, or when there was no
/// CR to read back: the write already succeeded, and a follow-up read that
/// does not answer is not evidence that it failed.
fn set_readback_error(key: &str, written: &Value, stored: Option<&Value>) -> Option<CliError> {
    let stored = stored?;
    let written = written.as_object()?;
    let dropped: Vec<&str> = written
        .iter()
        .filter(|(k, v)| stored.get(k.as_str()) != Some(*v))
        .map(|(k, _)| k.as_str())
        .collect();
    if dropped.is_empty() {
        return None;
    }
    Some(CliError::Other(format!(
        "the cluster did not store {} — the write was accepted and the field(s) discarded.\n\n         This cluster's PlatformStack CRD predates them, so the apiserver pruned what it does \
         not define. `{key}` was NOT changed. Upgrade the platform \
         (`apprafter platform upgrade`), then re-run this command.",
        dropped.join(", ")
    )))
}

/// `apprafter backup set <key> <value>` — change one field of a configured
/// backup, leaving every other field exactly as it was.
///
/// Refuses when backup has never been configured: a merge-patch into an
/// absent `spec.backup` would create a half-object the CRD rejects, and the
/// operator's real next step is `enable`, which composes the whole block.
pub fn run_backup_set(key: &str, value: &str) -> Result<()> {
    preflight_tools(&[&KUBECTL], "apprafter backup set")?;
    let patch = backup_set_patch(key, value)?;

    let kc = ensure_kubeconfig_tempfile()?;
    let spec_backup = spec_backup_from_cluster(Some(kc.path()))?;
    if spec_backup.is_none() {
        return Err(CliError::Other(
            "backup is not configured on this cluster, so there is no field to change. \
             `apprafter backup enable --bucket <name> --endpoint <host> --credential-file \
             <dotenv>` configures it."
                .into(),
        ));
    }

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

    // Read back before claiming success — see `set_readback_error`.
    let stored = spec_backup_from_cluster(Some(kc.path())).unwrap_or(None);
    if let Some(e) = set_readback_error(key, &patch["spec"]["backup"], stored.as_ref()) {
        return Err(e);
    }

    println!("✓ {key} set to {value}.");
    println!("  {BACKUP_GITOPS_ADVISORY}");
    println!(
        "  The change reaches the CronJob on the platform chart's next sync — \
              `apprafter backup status` shows what the cluster has."
    );
    Ok(())
}

/// The reader's zone NAME, best-effort: `$TZ`, else what the OS reports.
///
/// Cosmetic — it labels a time, it does not convert one. The conversion
/// goes through [`chrono::Local`], which applies the OS rules for each
/// timestamp's own date; this only answers "what do we call that zone".
/// `None` when neither source gives an IANA name, which prints an
/// unlabelled time rather than a wrong label.
pub(crate) fn readers_zone() -> Option<String> {
    resolve_time_zone(
        None,
        std::env::var("TZ").ok().as_deref(),
        iana_time_zone::get_timezone().ok().as_deref(),
    )
    .ok()
    .map(|(z, _)| z)
}

/// Render a stored RFC3339 timestamp in the reader's zone.
///
/// restic writes RFC3339 UTC to the nanosecond. Every other time this CLI
/// prints — the schedule above all — is in the operator's own zone, and one
/// raw UTC value among them reads as a different event than the one they
/// just caused. A value that does not parse is printed VERBATIM: it is the
/// only information there is about that snapshot, and a formatted guess
/// would be worse than the original.
///
/// Generic over the zone so production can pass [`chrono::Local`] — which
/// applies the OS rules for the snapshot's OWN date, not today's offset —
/// while tests pass a fixed offset and assert against a constant.
fn format_timestamp<Tz>(raw: &str, tz: &Tz) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    match chrono::DateTime::parse_from_rfc3339(raw) {
        Ok(t) => t.with_timezone(tz).format("%Y-%m-%d %H:%M:%S").to_string(),
        Err(_) => raw.to_string(),
    }
}

/// [`format_timestamp`] plus the zone name, for lines that carry a single
/// timestamp rather than a column under a header. Matches how the schedule
/// line already reads ("daily at 03:00 Europe/Lisbon") — one screen should
/// not report one time with its zone and another without.
///
/// A value that does not parse keeps its original text and gains no zone
/// label: labelling a string whose zone is unknown would be a claim.
fn format_timestamp_with_zone<Tz>(raw: &str, tz: &Tz, zone_label: Option<&str>) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    let rendered = format_timestamp(raw, tz);
    match zone_label {
        Some(z) if rendered != raw => format!("{rendered} {z}"),
        _ => rendered,
    }
}

/// Render the `backup list` snapshot table. Pure — extracted from
/// [`run_backup_list`], which prints exactly this.
///
/// INVARIANT: an absent `short_id` falls back to the full `id` TRUNCATED to 8
/// characters. `restic` takes either, and printing a full 64-hex id would
/// wreck the table it is supposed to line up.
///
/// Column widths are measured from the rows rather than fixed. The fixed
/// 25 the header used to reserve for TIME was narrower than the 30-character
/// timestamp restic writes, so TAGS began in a different place on every line
/// — the table lined up only for values nobody had.
fn format_snapshot_table<Tz>(
    repo: &str,
    snapshots: &[&Value],
    tz: &Tz,
    zone_label: Option<&str>,
    view: ClusterView<'_>,
) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    if snapshots.is_empty() {
        return format!("No snapshots in {repo}.\n");
    }
    let time_header = format!("TIME ({})", zone_label.unwrap_or("local"));

    let rows: Vec<(String, String, String, String)> = snapshots
        .iter()
        .map(|s| {
            let id = s
                .pointer("/short_id")
                .or_else(|| s.pointer("/id"))
                .and_then(Value::as_str)
                .map(|i| i.chars().take(8).collect::<String>())
                .unwrap_or_else(|| "?".to_string());
            let time = s
                .pointer("/time")
                .and_then(Value::as_str)
                .map(|t| format_timestamp(t, tz))
                .unwrap_or_else(|| "?".to_string());
            let tags = tags_of_snapshot(s)
                .iter()
                .map(|t| short_tag(t, tz))
                .collect::<Vec<_>>()
                .join(", ");
            (id, time, cluster_cell(s, view), tags)
        })
        .collect();

    let id_w = rows
        .iter()
        .map(|(id, _, _, _)| id.chars().count())
        .chain(std::iter::once("ID".len()))
        .max()
        .unwrap_or(2);
    let time_w = rows
        .iter()
        .map(|(_, t, _, _)| t.chars().count())
        .chain(std::iter::once(time_header.chars().count()))
        .max()
        .unwrap_or(4);
    let cluster_w = rows
        .iter()
        .map(|(_, _, c, _)| c.chars().count())
        .chain(std::iter::once("CLUSTER".len()))
        .max()
        .unwrap_or(7);

    let mut out = format!(
        "Snapshots in {repo}:\n{:<id_w$}  {:<time_w$}  {:<cluster_w$}  TAGS\n",
        "ID", time_header, "CLUSTER"
    );
    for (id, time, cluster, tags) in rows {
        out.push_str(&format!(
            "{id:<id_w$}  {time:<time_w$}  {cluster:<cluster_w$}  {tags}\n"
        ));
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

    fn run_capture(&self, argv: &[String], pass: &str) -> Result<backup_core::ResticOutput> {
        let out = self
            .command(argv, pass)
            .output()
            .map_err(|e| CliError::Other(format!("spawn restic: {e}")))?;
        if !out.status.success() {
            return Err(restic_failure_error(argv, out.status.code(), &out.stderr));
        }
        Ok(backup_core::ResticOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
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
    /// One of the unresolved inputs is the cluster's own IDENTITY (prune).
    /// `--cluster-uid` substitutes for it, but unlike a repo URL or a retention
    /// count it is a claim about whose data may be deleted, so the hint spells
    /// out what is being claimed.
    identity: bool,
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
        let mut msg = format!(
            "`apprafter backup {verb}` needs {}, which it reads from the cluster — so it needs a \
             reachable one. If the cluster no longer exists (disaster recovery — verifying an \
             off-site repo before restoring into a new cluster, or reclaiming what a destroyed \
             cluster left in one), pass {} and the command runs entirely off the cluster.",
            self.reasons.join(" and "),
            self.flags.join(" ")
        );
        if self.identity {
            msg.push_str(
                "\n\n`--cluster-uid` is not a convenience: it is a claim about WHOSE snapshots \
                 may be forgotten. A prune deletes by explicit snapshot id and one repository \
                 can hold several clusters' runs, so the wrong UID reclaims the wrong history. \
                 It is the gone cluster's `kube-system` namespace UID, which leads every restic \
                 tag its snapshots carry — `apprafter backup list --repo <repo> --all-clusters` \
                 names the identities a repository holds, and a prune against one it has never \
                 seen refuses instead of falling back to everything.",
            );
        }
        msg
    }
}

/// Where an operator verb's S3 credentials come from on this invocation.
///
/// Ordered by precedence: an explicit file, else a complete set in the
/// environment, else the Secret the cluster is already holding — the one
/// `backup enable` sealed. The cluster fallback is what lets `apprafter
/// backup check` run with no flags at all on a configured cluster; before
/// it, every maintenance verb asked the operator to hand back credentials
/// the platform already had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredSource {
    /// `--credential-file <dotenv>`.
    File,
    /// A complete canonical/alias set in the process environment.
    Env,
    /// `spec.backup.credentialRef` → a Secret in `apprafter-system`.
    Cluster,
}

/// Pick the credential source from what is available locally. Pure.
pub(crate) fn cred_source(has_file: bool, env_complete: bool) -> CredSource {
    if has_file {
        CredSource::File
    } else if env_complete {
        CredSource::Env
    } else {
        CredSource::Cluster
    }
}

/// Is the process environment carrying a COMPLETE credential set?
///
/// Partial is not enough and must not count: a stray `RESTIC_PASSWORD`
/// left over from an earlier command would otherwise beat the cluster's
/// own Secret and fail on the missing key pair.
fn env_creds_complete(env_lookup: &dyn Fn(&str) -> Option<String>) -> bool {
    let raw: BTreeMap<String, String> = ALL_S3_ENV_KEYS
        .iter()
        .filter_map(|&k| env_lookup(k).map(|v| (k.to_string(), v)))
        .collect();
    validate_required_cred_keys(&normalize_s3_creds(raw)).is_ok()
}

/// The Secret holding the off-site credentials: `spec.backup.credentialRef.name`
/// when the CR names one, else the name `backup enable` seals by default. Pure.
fn credential_secret_name(spec_backup: Option<&Value>) -> String {
    spec_backup
        .and_then(|s| s.pointer("/credentialRef/name"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_BACKUP_CREDENTIAL_NAME)
        .to_string()
}

/// Turn a credential Secret's decoded `.data` into the canonical `S3_*` map.
///
/// Accepts either spelling for the same reason the dotenv path does — the
/// Secret may have been sealed by `backup enable` (canonical) or by hand
/// (restic's own `AWS_*`). An incomplete Secret names ITSELF in the error:
/// the operator has to know which object to go fix, and "missing
/// RESTIC_PASSWORD" without a name reads like a flag they forgot.
fn creds_from_secret_bytes(
    data: BTreeMap<String, Vec<u8>>,
    secret_name: &str,
) -> Result<BTreeMap<String, String>> {
    let canonical = normalize_s3_creds(secret_bytes_to_strings(data));
    validate_required_cred_keys(&canonical).map_err(|e| {
        CliError::Other(format!(
            "credential Secret '{secret_name}' in {PLATFORMSTACK_NAMESPACE} is incomplete: {e}"
        ))
    })?;
    Ok(canonical)
}

/// State the rule ONCE: which inputs of a maintenance verb cannot be resolved
/// from the command line alone. Pure — table-tested without a cluster.
pub(crate) fn cluster_need(
    repo_override: Option<&str>,
    retention: RetentionArgs,
    creds: CredSource,
    cluster_uid_override: Option<&str>,
) -> ClusterNeed {
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
    // A prune DELETES, and a repository can be shared, so it must know whose
    // snapshots it is allowed to forget (E3/E4). The cluster answers that with
    // its own `kube-system` UID; with the cluster gone, `--cluster-uid` is the
    // operator answering it EXPLICITLY, which is the only other honest form —
    // the offline path this replaced simply planned across every snapshot in
    // the bucket, which is the defect and not the capability.
    if matches!(retention, RetentionArgs::Prune { .. }) && cluster_uid_override.is_none() {
        need.reasons.push(
            "an identity for the snapshots it may forget (this cluster's kube-system namespace \
             UID)",
        );
        need.flags.push("--cluster-uid <uid>".to_string());
        need.identity = true;
    }
    if creds == CredSource::Cluster {
        need.reasons
            .push("the S3 credentials (spec.backup.credentialRef)");
        need.flags.push("--credential-file <dotenv>".to_string());
    }
    need
}

/// Does this invocation of `backup check` / `prune` / `unlock` need to reach the
/// cluster?
///
/// `--repo` removes the repo lookup; for prune, explicit retention removes the
/// second reason; a local credential source removes the third. Anything still
/// unresolved must come from the PlatformStack CR or the Secret it names.
pub(crate) fn backup_verb_needs_cluster(
    repo_override: Option<&str>,
    retention: RetentionArgs,
    creds: CredSource,
    cluster_uid_override: Option<&str>,
) -> bool {
    cluster_need(repo_override, retention, creds, cluster_uid_override).is_needed()
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
    creds: CredSource,
    cluster_uid_override: Option<&str>,
) -> Result<Option<NamedTempFile>> {
    if !backup_verb_needs_cluster(repo_override, retention, creds, cluster_uid_override) {
        return Ok(None);
    }
    match ensure_kubeconfig_tempfile() {
        Ok(kc) => Ok(Some(kc)),
        Err(e) => Err(CliError::Other(format!(
            "{e}\n{}",
            cluster_need(repo_override, retention, creds, cluster_uid_override).hint(verb)
        ))),
    }
}

/// Resolve an operator verb's S3 credentials from the first source that has
/// them: `--credential-file`, else a complete env set, else the Secret the
/// cluster holds.
///
/// The cluster read is what makes `apprafter backup check` work with no
/// flags on a configured cluster. It is deliberately LAST: an operator who
/// passed a file or exported the variables meant those, and a maintenance
/// verb must never quietly prefer a different credential to the one they
/// named.
///
/// `spec_backup` is passed in rather than fetched so the caller — which has
/// already read the CR for the repo URL — does not read it twice.
fn resolve_verb_creds(
    credential_file: Option<&Path>,
    kubeconfig: Option<&Path>,
    spec_backup: Option<&Value>,
) -> Result<BTreeMap<String, String>> {
    let env_lookup = |k: &str| std::env::var(k).ok();
    match cred_source(credential_file.is_some(), env_creds_complete(&env_lookup)) {
        CredSource::File | CredSource::Env => {
            resolve_operator_s3_creds(credential_file, &env_lookup)
        }
        CredSource::Cluster => {
            let Some(kc) = kubeconfig else {
                return Err(CliError::Other(format!(
                    "no S3 credentials — none given locally and no cluster to read them from.\n\n\
                     {CRED_KEYS_HELP}"
                )));
            };
            let name = credential_secret_name(spec_backup);
            let Some((raw, _)) = read_secret_data(&name, PLATFORMSTACK_NAMESPACE, kc)? else {
                return Err(CliError::Other(format!(
                    "credential Secret '{name}' not found in {PLATFORMSTACK_NAMESPACE} — the CR \
                     names it but the object is not there. Re-seal it with `apprafter backup \
                     enable --credential-file <dotenv>`, or pass --credential-file to this \
                     command.\n\n{CRED_KEYS_HELP}"
                )));
            };
            let creds = creds_from_secret_bytes(raw, &name)?;
            println!("  using credentials from Secret '{name}' in {PLATFORMSTACK_NAMESPACE}");
            Ok(creds)
        }
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

/// Read `PlatformStack/default.spec.backup` once, or `None` when there is no
/// cluster to read it from.
///
/// Every maintenance verb needs the same block for up to three different
/// inputs — the repo URL, the retention policy, and the name of the credential
/// Secret — and fetching it once is what keeps a verb to a single CR read.
/// Returned owned rather than borrowed so callers can hold it across the
/// credential resolution that follows.
fn spec_backup_from_cluster(kubeconfig: Option<&Path>) -> Result<Option<Value>> {
    let Some(kc) = kubeconfig else {
        return Ok(None);
    };
    let ps = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc,
    )?;
    Ok(ps.and_then(|p| p.pointer("/spec/backup").cloned()))
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
/// ## Prune needs an identity, from the cluster or from the operator (E3/E4)
///
/// It used to be lazy: `--repo` plus all three `--keep-*` flags let it run with
/// no cluster at all. That form is gone, and the reason is the point of this
/// command's blast radius. A restic repository can legitimately be shared by
/// two clusters — the documented "move to a bigger machine" runbook has both
/// alive at once — and the planner deletes by explicit snapshot id. Without a
/// cluster's `kube-system` UID there is nothing to tell one cluster's runs from
/// the other's, so an "offline" prune planned across the whole bucket and
/// forgot the neighbour's history.
///
/// What the identity may NOT be is implicit. A prune with a live cluster reads
/// the UID off `kube-system`; a prune whose cluster is gone — the real offline
/// need, where the repository outlived the machine and its snapshots should be
/// reclaimable — takes `--cluster-uid <uid>`, the operator saying WHOSE history
/// this is. The repository checks that claim before anything is forgotten: a
/// UID it has never seen is refused, naming the ones it holds, rather than
/// falling through to the pre-identity snapshots.
pub fn run_backup_prune(
    repo_override: Option<&str>,
    credential_file: Option<&Path>,
    keep_daily: Option<u32>,
    keep_weekly: Option<u32>,
    keep_monthly: Option<u32>,
    cluster_uid_override: Option<&str>,
) -> Result<()> {
    // D11 / 2.22a: the external binaries this command spawns, checked
    // BEFORE any prompt, kubeconfig or provider call. The reported bug
    // was a passphrase typed into a command that could not have worked.
    preflight_tools(&[&RESTIC], "apprafter backup prune")?;

    // A typo here selects a different cluster's history, so it is rejected at
    // the flag rather than at the planner: every identity in a repository is a
    // Kubernetes namespace UID, and nothing else can ever match one.
    if let Some(uid) = cluster_uid_override {
        if !backup_core::cluster::is_uuid(uid) {
            return Err(CliError::Other(format!(
                "--cluster-uid '{uid}' is not a Kubernetes namespace UID. It must be the gone \
                 cluster's `kube-system` UID in canonical UUID form \
                 (8-4-4-4-12 hex), which is what leads every restic tag its snapshots carry."
            )));
        }
    }

    let retention = RetentionArgs::Prune {
        keep_daily,
        keep_weekly,
        keep_monthly,
    };
    let source = cred_source(
        credential_file.is_some(),
        env_creds_complete(&|k| std::env::var(k).ok()),
    );
    let kc = kubeconfig_if_cluster_needed(
        "prune",
        repo_override,
        retention,
        source,
        cluster_uid_override,
    )?;
    let kc_path = kc.as_ref().map(|f| f.path());

    // Fetch the CR once (when we have a cluster at all): repo fallback
    // (spec.backup.bucket), retention defaults (spec.backup.retention) and the
    // credential Secret's name (spec.backup.credentialRef) all read from it.
    let spec_backup = spec_backup_from_cluster(kc_path)?;
    let spec_backup = spec_backup.as_ref();

    let creds = resolve_verb_creds(credential_file, kc_path, spec_backup)?;
    let pass = creds["RESTIC_PASSWORD"].clone();

    let repo = repo_from_spec_backup(repo_override, spec_backup)?;
    let policy = retention_from_spec_backup(spec_backup, keep_daily, keep_weekly, keep_monthly);

    let runner = CredentialedRestic { creds };

    // Whose snapshots this prune may forget: the operator's explicit claim, or
    // the cluster's own UID. `cluster_need` guarantees one of the two is
    // available — with no `--cluster-uid` the kubeconfig is unconditional.
    let cluster_uid = match cluster_uid_override {
        Some(uid) => {
            // Check the claim against the repository before deleting anything.
            // This costs a second `restic snapshots` (run_prune lists again),
            // which is the right trade for a rare, deliberate, destructive
            // command that cannot ask a cluster to confirm its own identity.
            let json = runner.run_stdout(&restic_snapshots_argv(&repo), &pass)?;
            let snapshots = parse_snapshots_json(&json)?;
            println!("  {}", offline_prune_scope(&snapshots, uid)?);
            uid.to_string()
        }
        None => {
            let kc_path = kc_path.ok_or_else(|| {
                CliError::Other(identity_read_error(
                    "prune resolved no kubeconfig, which `cluster_need` should have made \
                     impossible",
                ))
            })?;
            read_cluster_uid(kc_path)?
        }
    };

    // A run with no manifest is left alone while a backup may still be
    // writing it: the scheduled backup, or a `backup create` into the same
    // repository, is not stopped by this command. How long that is follows
    // the cluster's backup deadline — the default six hours with no cluster.
    let run_deadline = backup_core::helper_pod::run_deadline_of_spec_backup(spec_backup);
    let outcome = run_prune(
        &runner,
        &repo,
        &pass,
        &policy,
        &cluster_uid,
        chrono::Utc::now(),
        run_deadline,
    )?;
    refuse_an_unenforced_prune(&repo, &outcome, credential_file.is_some())?;

    print!("{}", prune_summary(&repo, &policy, &outcome));

    // Stamp last-prune so `backup status` can report it. Best-effort ordering:
    // the prune already succeeded, so a merge-patch failure here surfaces as an
    // error (the annotation is the audit trail — we don't want to swallow it).
    // Only a prune of THIS cluster's history is stamped on it
    // ([`last_prune_stamp`]); any other says why it stamps nothing rather
    // than failing — an offline prune's cluster being gone is the whole
    // premise of that path.
    let own_uid = match (kc_path, cluster_uid_override) {
        (Some(_), None) => Ok(cluster_uid.clone()),
        (Some(kc), Some(_)) => read_cluster_uid(kc).map_err(|e| e.to_string()),
        (None, _) => Err("no cluster".to_string()),
    };
    let configured_repo = spec_backup
        .and_then(|s| s.pointer("/bucket"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    if let Err(why) = last_prune_stamp(
        kc_path.is_some(),
        &repo,
        configured_repo,
        &cluster_uid,
        own_uid.as_deref().map_err(String::as_str),
    ) {
        println!("  (`apprafter.io/last-prune` not stamped: {why})");
        return Ok(());
    }
    let Some(kc_path) = kc_path else {
        return Ok(());
    };
    let ts = chrono::Utc::now().to_rfc3339();
    let body = last_prune_patch_body(&ts);
    kubectl_merge_patch(
        "platformstack",
        PLATFORMSTACK_NAME,
        Some(PLATFORMSTACK_NAMESPACE),
        None,
        &body,
        kc_path,
    )?;
    println!("  last-prune stamped: {ts}");
    Ok(())
}

/// Check an offline prune's `--cluster-uid` against the repository it is about
/// to forget snapshots in, and describe what it will plan over. Pure.
///
/// With no cluster to read an identity from, the REPOSITORY is the only thing
/// that can check the operator's claim — and it must, because a mistyped UID
/// does not fail loudly. It matches nothing identified, every identified
/// snapshot becomes another cluster's and is spared, and the planner is left
/// holding only the pre-identity ones, which it would forget by policy. That
/// is a silent delete of the wrong history, so a UID this repository has never
/// seen is refused with the ones it has.
///
/// A repository holding ONLY pre-identity snapshots is not that mistake: there
/// is no identity in it to match, every snapshot is attributed by the stated
/// assumption, and reclaiming such a repository after its cluster is gone is
/// exactly what this flag restores. Allowed, and named so it is not a surprise.
fn offline_prune_scope(snapshots: &[Value], uid: &str) -> Result<String> {
    let present = backup_core::cluster::cluster_uids_in(snapshots);
    if present.iter().any(|p| p == uid) {
        return Ok(format!(
            "pruning the snapshots of cluster {uid} ({} cluster(s) in this repository)",
            present.len()
        ));
    }
    if present.is_empty() {
        return Ok(format!(
            "no snapshot in this repository carries a cluster identity — they predate it, and \
             are pruned as {uid}'s by the stated assumption"
        ));
    }
    Err(CliError::Other(format!(
        "--cluster-uid {uid} has never written a snapshot to this repository, which holds \
         {}: {}.\n\nRefusing rather than pruning: a UID that matches nothing would spare every \
         identified snapshot and forget only the ones written before cluster identity existed. \
         Pass one of the identities above, or `apprafter backup list --repo <repo> \
         --all-clusters` to see the runs behind them.",
        if present.len() == 1 {
            "one cluster".to_string()
        } else {
            format!("{} clusters", present.len())
        },
        present.join(", ")
    )))
}

/// What `backup prune` prints after a successful prune. Pure — extracted from
/// [`run_backup_prune`], which prints exactly this.
fn prune_summary(
    repo: &str,
    policy: &RetentionPolicy,
    outcome: &backup_core::prune::PruneOutcome,
) -> String {
    format!(
        "✓ Pruned {repo}: {}\n  retention: keepDaily={} keepWeekly={} keepMonthly={}\n",
        outcome.describe(),
        policy.keep_daily,
        policy.keep_weekly,
        policy.keep_monthly
    )
}

/// A prune the credential was not permitted to run is an error — nothing was
/// deleted — and must not read as `✓ Pruned` or stamp `last-prune`. Pure.
fn refuse_an_unenforced_prune(
    repo: &str,
    outcome: &backup_core::prune::PruneOutcome,
    had_credential_file: bool,
) -> Result<()> {
    match outcome {
        backup_core::prune::PruneOutcome::NotPermitted { .. } => Err(prune_not_permitted_error(
            repo,
            outcome,
            had_credential_file,
        )),
        _ => Ok(()),
    }
}

/// The error `backup prune` ends with when the credential it ran with may
/// not delete. Pure.
///
/// The usual cause is the one ADR 0050 recommends: with no credential file,
/// and none in the environment, the command falls back to the cluster's own
/// Secret, whose key is scoped so that a compromised cluster cannot erase
/// history — and so cannot prune either. Nothing was deleted: the prune
/// stopped at the first refused delete.
fn prune_not_permitted_error(
    repo: &str,
    outcome: &backup_core::prune::PruneOutcome,
    had_credential_file: bool,
) -> CliError {
    let which = if had_credential_file {
        "The credential file this command read holds a key that may not delete from this \
         repository."
    } else {
        "With no --credential-file, this command used the credentials in the environment or, \
         failing those, the cluster's own backup Secret — whose key is usually scoped so that \
         the cluster cannot delete history (ADR 0050), and so cannot prune it either."
    };
    CliError::Other(format!(
        "retention was not enforced on {repo}: {}.\n\n{which} Run it again with the \
         operator's full credentials: `apprafter backup prune --credential-file \
         <full-credentials.env>` (S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY, RESTIC_PASSWORD).",
        outcome.describe()
    ))
}

/// Does `backup prune` stamp `apprafter.io/last-prune` on the cluster it
/// resolved? `Ok` to stamp, else why not. Pure.
///
/// Only when what was pruned is that cluster's history: `repo` is the
/// repository its `spec.backup.bucket` names, and `pruned_uid` — the
/// identity whose snapshots the prune could forget — is its own
/// `kube-system` UID (`own_uid`, or why it could not be read). The operator's
/// `BackupRetention` quotes the stamp as "`apprafter backup prune` last ran
/// against this cluster", and `backup status` shows it as the last prune.
///
/// It used to stamp whenever a cluster had been resolved at all. `backup
/// prune --repo <a rehearsal repository> --cluster-uid <another cluster>`
/// with no `--keep-*` flags resolves the ACTIVE cluster only to read its
/// retention policy, and stamped it although neither the repository nor the
/// identity was its own.
fn last_prune_stamp(
    have_cluster: bool,
    repo: &str,
    configured_repo: Option<&str>,
    pruned_uid: &str,
    own_uid: std::result::Result<&str, &str>,
) -> std::result::Result<(), String> {
    if !have_cluster {
        return Err("no cluster to stamp it on — an offline prune by --cluster-uid".into());
    }
    let same_repo = |a: &str, b: &str| a.trim_end_matches('/') == b.trim_end_matches('/');
    match configured_repo {
        None => {
            return Err(format!(
                "this cluster has no backup repository configured, so {repo} is not its \
                 repository"
            ))
        }
        Some(configured) if !same_repo(repo, configured) => {
            return Err(format!(
                "{repo} is not this cluster's backup repository, {configured}"
            ))
        }
        Some(_) => {}
    }
    match own_uid {
        Ok(own) if own == pruned_uid => Ok(()),
        Ok(own) => Err(format!(
            "the history pruned is cluster {pruned_uid}'s, and this cluster is {own}"
        )),
        Err(e) => Err(format!(
            "this cluster's own identity could not be read to check it is {pruned_uid}: {e}"
        )),
    }
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

    let source = cred_source(
        credential_file.is_some(),
        env_creds_complete(&|k| std::env::var(k).ok()),
    );
    let kc = kubeconfig_if_cluster_needed(
        "check",
        repo_override,
        RetentionArgs::NotApplicable,
        source,
        None,
    )?;
    let kc_path = kc.as_ref().map(|f| f.path());
    let spec_backup = spec_backup_from_cluster(kc_path)?;
    let creds = resolve_verb_creds(credential_file, kc_path, spec_backup.as_ref())?;
    let pass = creds["RESTIC_PASSWORD"].clone();
    let repo = repo_from_spec_backup(repo_override, spec_backup.as_ref())?;

    let runner = CredentialedRestic { creds };
    runner.run(&restic_check_argv(&repo, read_data), &pass)?;

    if read_data {
        println!("✓ Repository check passed (deep --read-data verify).");
    } else {
        println!("✓ Repository check passed.");
    }

    // Size and snapshot count, best-effort: the check has already passed
    // and that is the answer. A stats call that fails must not turn a
    // verified repository into a failed command.
    if let Some(stats) = repo_stats(&runner, &repo, &pass, None) {
        let count = stats
            .snapshots_count
            .map(|n| format!("{n} snapshot(s), "))
            .unwrap_or_default();
        println!(
            "  {count}{} stored (raw, after dedup and compression)",
            human_size(stats.total_size)
        );
    }
    Ok(())
}

/// `restic stats` for a repo or one snapshot, or `None` when the call or the
/// parse fails. Best-effort by construction: every caller has already
/// answered the question it was asked, and a size line is an extra.
fn repo_stats(
    runner: &CredentialedRestic,
    repo: &str,
    pass: &str,
    snapshot: Option<&str>,
) -> Option<ResticStats> {
    let out = runner
        .run_stdout(&restic_stats_argv(repo, snapshot), pass)
        .ok()?;
    parse_stats_json(&out)
}

/// What one `restic ls` of a snapshot tells us: the manifest it carries and
/// the secrets it holds, which live in the tree rather than in the manifest.
pub(crate) struct SnapshotInsides {
    pub manifest: Value,
    pub secret_files: u64,
}

/// Read a snapshot's `manifest.json` and count its secret files — two restic
/// calls: `ls` to walk the tree (the staging directory's name changes every
/// run, so the manifest can only be found by name), `dump` to read it.
fn read_snapshot_insides(
    runner: &CredentialedRestic,
    repo: &str,
    pass: &str,
    snapshot: &str,
) -> Result<SnapshotInsides> {
    let ls = runner.run_stdout(&restic_ls_argv(repo, snapshot), pass)?;
    let secret_files = count_secret_files(&ls);
    let path = manifest_path_in_snapshot(&ls).ok_or_else(|| {
        CliError::Other(format!(
            "snapshot {snapshot} carries no manifest.json, so it was not written by \
             `apprafter backup` — restic repositories can hold anything, and this one \
             holds something else."
        ))
    })?;
    let raw = runner.run_stdout(&restic_dump_argv(repo, snapshot, &path), pass)?;
    let manifest = serde_json::from_str(&raw).map_err(|e| {
        CliError::Other(format!("parse manifest.json from snapshot {snapshot}: {e}"))
    })?;
    Ok(SnapshotInsides {
        manifest,
        secret_files,
    })
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

    let source = cred_source(
        credential_file.is_some(),
        env_creds_complete(&|k| std::env::var(k).ok()),
    );
    let kc = kubeconfig_if_cluster_needed(
        "unlock",
        repo_override,
        RetentionArgs::NotApplicable,
        source,
        None,
    )?;
    let kc_path = kc.as_ref().map(|f| f.path());
    let spec_backup = spec_backup_from_cluster(kc_path)?;
    let creds = resolve_verb_creds(credential_file, kc_path, spec_backup.as_ref())?;
    let pass = creds["RESTIC_PASSWORD"].clone();
    let repo = repo_from_spec_backup(repo_override, spec_backup.as_ref())?;

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
    initial_backup: bool,
) -> Result<()> {
    // 0. Build the canonical restic repo URL from bucket + optional endpoint/prefix.
    opts.bucket = construct_repo_url(&opts.bucket, endpoint, prefix)?;

    // 1. Validate enum-valued options before touching the cluster.
    validate_enable_enums(&opts)?;

    // 1a. The human cluster label the snapshots are grouped under. Defaults to
    //     the target name, which is the name the operator already thinks of
    //     this cluster by — the alternative was an anonymous shared host, and
    //     a repository where every row reads `apprafter-backup` cannot be read.
    opts.cluster_name = Some(resolve_cluster_name(
        opts.cluster_name.as_deref(),
        &resolve_state_paths(None)?.target_name,
    )?);

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
        enable_success_report(
            &opts.bucket,
            &opts.credential,
            opts.cluster_name.as_deref().unwrap_or(""),
            &resolved
        )
    );

    // 9. Run the first backup, unless told not to.
    //
    //    A schedule that has never run is indistinguishable from one that
    //    does not work: `backup status` shows no Jobs, `backup list` shows
    //    no snapshots, and the operator has hours to wait before learning
    //    which of the two they have. Running it once here closes that gap
    //    and exercises the parts a local preflight cannot — the cluster's
    //    own credentials, the runner's RBAC, and egress from the cluster to
    //    the bucket.
    if !initial_backup {
        println!(
            "  first backup skipped (--no-initial-backup) — it runs at the scheduled time, or \
             now with `apprafter backup run`."
        );
        return Ok(());
    }
    //
    //    By then the configuration is applied. A first backup that did not
    //    complete does not undo that, and the command says so before it
    //    exits ([`first_backup_outcome`]).
    match wait_for_synced_cronjob(&opts.bucket, CRONJOB_SYNC_WAIT_MINUTES, kc.path())? {
        Some(cronjob) => {
            println!("  → running the first backup now");
            take_first_backup(|| {
                instantiate_backup_job(
                    &cronjob,
                    true,
                    DEFAULT_BACKUP_JOB_TIMEOUT_MINUTES,
                    kc.path(),
                )
            })?;
        }
        None => {
            // The `enable` itself succeeded. A chart that has not synced
            // within the window is a slow cluster or a paused Argo CD, and
            // failing here would report a configured backup as broken.
            println!(
                "  the platform chart has not deployed the new schedule yet, so no first backup \
                 was run. Backup IS enabled; `apprafter backup status` shows when the schedule \
                 lands, and `apprafter backup run` takes the first one then."
            );
        }
    }
    Ok(())
}

/// Run `backup enable`'s first backup with `run`, and end the way
/// [`first_backup_outcome`] says when it does not complete.
fn take_first_backup(run: impl FnOnce() -> Result<()>) -> Result<()> {
    let Err(e) = run() else {
        return Ok(());
    };
    let (line, exit) = first_backup_outcome(e);
    println!("{line}");
    exit.map_or(Ok(()), Err)
}

/// How `backup enable` ends when its first backup did not complete: the line
/// it prints, and the error it exits with.
///
/// The PlatformStack patch is applied before the first backup starts, so
/// backup IS enabled whatever happens to that backup, and a bare error reads
/// as an `enable` that failed, which people answer by running it again. The
/// line says it is enabled; for a runner no node would take, so does the
/// error's help, ahead of the advice `backup run` gives.
///
/// The exit stays non-zero on purpose. The first backup is the proof
/// `enable` offers that backups work, and one no node would take means the
/// scheduled backup, which asks for the same, will not run either. A script
/// that checks the exit code must not read that as working backups. A first
/// backup that is not attempted at all (the chart has not synced) exits 0:
/// nothing failed.
///
/// Beside a backup or check Job that has not finished, no first backup is
/// started ([`refusal`]). That exits 0 too: nothing failed, and the Job
/// already there is the one to watch.
fn first_backup_outcome(e: CliError) -> (String, Option<CliError>) {
    match e {
        CliError::BackupJobActive { job } => (
            format!(
                "  Backup IS enabled. Its first backup was not started beside {job}: `apprafter \
                 backup status` shows that Job's result, and `apprafter backup run` takes a \
                 backup once it has finished."
            ),
            None,
        ),
        CliError::BackupRunnerUnschedulable { job, what, help } => (
            "  Backup IS enabled: the configuration above is applied. Its first backup could \
             not start."
                .to_string(),
            Some(CliError::BackupRunnerUnschedulable {
                job,
                what,
                help: format!(
                    "Backup IS enabled, so do not run `apprafter backup enable` again; only its \
                     first backup could not start. {help}"
                ),
            }),
        ),
        other => (
            "  Backup IS enabled: the configuration above is applied. Its first backup did not \
             complete:"
                .to_string(),
            Some(other),
        ),
    }
}

/// The cluster label to store: `--cluster-name` when given, else the target
/// name. Pure — extracted from [`run_backup_enable`].
///
/// Validated as a restic host: it becomes `--host` on every snapshot, and a
/// value with whitespace or a comma in it would make a listing unreadable and
/// a `--host` filter unusable. Deliberately permissive otherwise — this is a
/// human label, not a DNS name.
pub(crate) fn resolve_cluster_name(explicit: Option<&str>, target_name: &str) -> Result<String> {
    let name = explicit.unwrap_or(target_name).trim().to_string();
    if name.is_empty() {
        return Err(CliError::Other(
            "--cluster-name cannot be empty — it is the label every snapshot of this cluster is \
             listed under."
                .into(),
        ));
    }
    if name.chars().any(|c| c.is_whitespace() || c == ',') {
        return Err(CliError::Other(format!(
            "--cluster-name '{name}' cannot contain whitespace or commas — it becomes the restic \
             `--host` on every snapshot."
        )));
    }
    Ok(name)
}

/// Refuse the two enum-valued `enable` flags before anything is touched.
///
/// Pure — extracted from [`run_backup_enable`] and called from both there and
/// the tests. It runs BEFORE the kubeconfig, the credential seal and the DR
/// prompt precisely so a typo costs nothing.
fn validate_enable_enums(o: &EnableOpts) -> Result<()> {
    if let Some(enforce) = &o.enforce {
        if !matches!(enforce.as_str(), "check" | "cluster" | "operator") {
            return Err(CliError::Other(format!(
                "invalid --enforce '{enforce}': expected 'check', 'cluster' or 'operator'"
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
fn enable_success_report(
    bucket: &str,
    credential: &str,
    cluster_name: &str,
    s: &ResolvedSchedule,
) -> String {
    format!(
        "✓ Scheduled off-site backup enabled → {bucket} (credential Secret '{credential}').\n  schedule: {} {}\n  cluster:  {cluster_name} (the name this cluster's snapshots are listed under; \
         `apprafter backup set cluster-name <name>` changes it)\n{BACKUP_GITOPS_ADVISORY}\n",
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
    // Names `set enabled true` and not `enable`: the config IS retained, and
    // `backup enable` would recompose the whole block from its flags, quietly
    // resetting the schedule, timezone, retention and staging mode this
    // command just promised to keep.
    println!(
        "✓ Scheduled backup disabled (config retained; re-enable with \
         `apprafter backup set enabled true`)."
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
    let cat = String::from_utf8_lossy(cat_stderr).trim().to_string();
    let init = String::from_utf8_lossy(init_stderr).trim().to_string();
    let hint = repo_probe_hint(&cat, &init);
    CliError::BackupRepoProbe {
        repo: bucket.to_string(),
        cat_stderr: cat,
        init_stderr: init,
        hint,
    }
}

/// Fallback guidance when neither stderr matches a known shape. Never the
/// catch-all's "the message above is the only context": a probe that fails
/// has a fixed, short list of things it can be, and naming them beats
/// handing back two lines of restic and wishing the reader luck.
const REPO_PROBE_GENERIC_HINT: &str =
    "Neither restic call recognised its own failure, so both stderrs above are the evidence. \
     The probe touches four things and nothing else: the endpoint (`--endpoint`), the bucket \
     name (`--bucket`), the S3 key pair, and the passphrase — all read from the credential \
     file. Reproduce it outside the CLI to narrow it down: `restic -r <repo> cat config` with \
     `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `RESTIC_PASSWORD` exported does exactly \
     what `enable` just did.";

/// Pick the remedy for a failed repo probe.
///
/// Which stderr holds the diagnosis depends on the failure. Normally it is
/// `init`'s — `cat config` only ever reports that no repository is there,
/// which is the premise for trying `init` rather than a finding. The
/// exception is a wrong passphrase: there the config exists and opens for
/// nobody, `cat config` says so, and `init` can only answer "already
/// initialized" — classifying on `init` would report an intact repository
/// as a broken one, so the passphrase reading wins.
fn repo_probe_hint(cat_stderr: &str, init_stderr: &str) -> String {
    if classify_restic(cat_stderr) == ResticFailure::WrongPassphrase {
        return ResticFailure::WrongPassphrase
            .hint()
            .unwrap_or(REPO_PROBE_GENERIC_HINT)
            .to_string();
    }
    let mut hint = classify_restic(init_stderr)
        .hint()
        .unwrap_or(REPO_PROBE_GENERIC_HINT)
        .to_string();
    if let Some(note) = init_leftover_note(init_stderr) {
        hint.push_str("\n\n");
        hint.push_str(note);
    }
    hint
}

/// Whether the failed `init` left a key file in the repository, read off
/// the object it was saving when it gave up.
///
/// restic writes the master key first and the config second, and its
/// stderr names the handle: `Save(<key/…>)` failed on the very first
/// object it writes, so the location is untouched; `Save(<config/…>)`
/// failed after the key was already stored, so a key file is sitting
/// there now — and that key is what makes the *next* `enable` fail with
/// "repository already contains keys" instead of the real obstacle.
///
/// Reading the object out of the stderr is deliberate. The alternative
/// — running `init` a second time and taking "already contains keys" as
/// proof — costs another round trip and is not free of consequences: an
/// init that failed transiently *before* writing its key can succeed at
/// writing one on the retry, so the probe would sometimes create the
/// very leftover it set out to detect. `None` when the stderr names no
/// object, because then there is nothing to conclude.
fn init_leftover_note(init_stderr: &str) -> Option<&'static str> {
    let s = init_stderr.to_lowercase();
    if s.contains("save(<config/") {
        Some(
            "This run left a key file behind. restic reported the failure while saving \
             `config`, which it writes after the master key, so the key is already stored. \
             Delete the `keys/` prefix under the repository path before retrying — otherwise \
             the next run reports 'repository already contains keys' and hides the problem \
             above.",
        )
    } else if s.contains("save(<key/") {
        Some(
            "Nothing was left behind: the failure came while saving the master key itself, \
             which is the first object restic writes, so the location is as it was.",
        )
    } else {
        None
    }
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

/// A Job's outcome as `backup status` prints it: the Job's own terminal
/// condition when it has one — with the reason and message for a failure, so
/// a run stopped at its deadline reads `Failed: DeadlineExceeded: Job was
/// active longer than specified deadline` rather than a bare `Failed` — and
/// the pod counts ([`job_outcome`]) while it has none.
///
/// The condition is what the Job controller decided; the pod counts are only
/// what its pods did, and they cannot tell a deadline from a crash.
///
/// While there is no condition, the Job's pod says more than its counts:
/// `status.active` counts a pod the scheduler could not place exactly like
/// one that is running a backup, so a runner that never started read
/// `Running`. With the pod in `pods` the line says `Pending, cannot be
/// scheduled: <the scheduler's reason>` instead ([`job_pod`]). Between a
/// failed attempt and its retry there is no live pod, and the counts
/// (`active: 0`, `failed: 1`) read `Failed` for a Job that has attempts left:
/// the line says `Retrying after 1 failed attempt (7 attempts at most)`
/// instead. Without the pods, or for a shape that is not recognised, the
/// counts are what is left.
fn job_line_outcome(j: &serde_json::Value, pods: &[serde_json::Value]) -> String {
    match job_run_outcome(j) {
        JobOutcome::Succeeded => "Succeeded".to_string(),
        JobOutcome::Failed(why) => format!("Failed: {why}"),
        JobOutcome::Running => {
            job_pod::status_outcome(&job_pod(j, pods)).unwrap_or_else(|| job_outcome(j).to_string())
        }
    }
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
/// Jobs are told apart by [`runner_job`] — the CronJob that owns them, or the
/// marks `backup run` leaves on its own — as the operator's `BackupHealthy`
/// tells them apart. For each of the two the most-recent Job (by
/// `.status.startTime`) is shown.
///
/// `pods` is any listing that holds those Jobs' pods (the caller reads
/// `apprafter-system`'s). It is what tells an unfinished Job whose runner
/// works from one whose pod no node has room for; empty, the Job lines fall
/// back to the Jobs' own pod counts.
pub(crate) fn format_backup_status<Tz>(
    spec_backup: Option<&serde_json::Value>,
    jobs: &[serde_json::Value],
    pods: &[serde_json::Value],
    status_cm: Option<&serde_json::Value>,
    last_prune: Option<&str>,
    tz: &Tz,
    zone_label: Option<&str>,
) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
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
    // Retention sub-block. Who prunes is always said: unset, it is the
    // platform's default, which the operator's retention verdict below
    // names (`check` since WI-389; `operator` before it).
    out.push_str("  retention:\n");
    let ret = spec.get("retention");
    for key in ["keepDaily", "keepWeekly", "keepMonthly"] {
        if let Some(n) = ret.and_then(|r| r.get(key)) {
            out.push_str(&format!("    {key}: {n}\n"));
        }
    }
    match ret
        .and_then(|r| r.get("enforce"))
        .and_then(serde_json::Value::as_str)
    {
        Some(e) => out.push_str(&format!("    enforce: {e}\n")),
        None => out.push_str("    enforce: not set (the platform's default)\n"),
    }

    // --- Job outcomes ---
    // Partition into backup Jobs and check Jobs by what runs them.
    let backup_jobs: Vec<&serde_json::Value> = jobs
        .iter()
        .filter(|j| runner_job(j) == Some(RunnerJob::Backup))
        .collect();
    let check_jobs: Vec<&serde_json::Value> = jobs
        .iter()
        .filter(|j| runner_job(j) == Some(RunnerJob::Check))
        .collect();

    // A Job line that says only WHETHER it succeeded leaves the question the
    // operator opened this screen with — is the backup current? — unanswered:
    // last week's success and this morning's read identically.
    //
    // A Job whose pod cannot be scheduled gets the reason on its line and,
    // under it, where to look: that Job is not running, and the scheduled
    // backup asks for the same room.
    let job_line = |j: &serde_json::Value| -> String {
        let when = job_start_time(j);
        let outcome = job_line_outcome(j, pods);
        let mut line = if when.is_empty() {
            format!("{} — {outcome}\n", job_metadata_name(j))
        } else {
            format!(
                "{} — {outcome} ({})\n",
                job_metadata_name(j),
                format_timestamp_with_zone(when, tz, zone_label)
            )
        };
        if job_run_outcome(j) == JobOutcome::Running {
            if let Some(hint) = job_pod::status_hint(j, &job_pod(j, pods)) {
                line.push_str(&hint);
            }
        }
        line
    };

    out.push_str("\nJobs:\n");
    match most_recent_job(&backup_jobs) {
        Some(j) => out.push_str(&format!("  Last backup Job: {}", job_line(j))),
        None => out.push_str("  Last backup Job: none\n"),
    }
    match most_recent_job(&check_jobs) {
        Some(j) => out.push_str(&format!("  Last check Job:  {}", job_line(j))),
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
            out.push_str(&format!(
                "  lastSuccess:    {}\n",
                format_timestamp_with_zone(last_success, tz, zone_label)
            ));
        } else {
            out.push_str("  lastSuccess:    never\n");
        }
        if !last_failure.is_empty() {
            out.push_str(&format!(
                "  lastFailure:    {}\n",
                format_timestamp_with_zone(last_failure, tz, zone_label)
            ));
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

    // --- Last prune from outside the cluster ---
    out.push_str(&format!(
        "\nLast prune: {} (by `apprafter backup prune`, from outside the cluster)\n",
        last_prune
            .map(|p| format_timestamp_with_zone(p, tz, zone_label))
            .unwrap_or_else(|| "never".to_string())
    ));

    out
}

/// How many lines of a failed check's output `backup status` quotes.
const CHECK_ERROR_LINES: usize = 3;

/// The repository block of `backup status`: the runner's record of its last
/// check, its last prune and the repository's figures (WI-389), then what
/// the operator makes of retention.
///
/// Pure. `status_cm` is the runner's ConfigMap (the object or its `data`);
/// `stack` is `PlatformStack/default`, whose `BackupRetention` condition is
/// the verdict. With an operator that writes no such condition, a prune the
/// key did not permit is still said, from the record alone: that warning is
/// the point of this block.
pub(crate) fn format_repository_status<Tz>(
    status_cm: Option<&Value>,
    stack: Option<&Value>,
    tz: &Tz,
    zone_label: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    let data = status_cm.map(|cm| cm.get("data").filter(|d| d.is_object()).unwrap_or(cm));
    let get = |key: &str| -> Option<&str> {
        data.and_then(|d| d.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let num = |key: &str| get(key).and_then(|v| v.parse::<i64>().ok());
    let when = |t: &str| format_timestamp_with_zone(t, tz, zone_label);
    let mut out = String::from("\nRepository (recorded by the weekly check):\n");

    match get("lastCheck") {
        Some(t) => {
            let result = match get("lastCheckResult") {
                Some("passed") => "passed".to_string(),
                Some("failed") => "FAILED".to_string(),
                other => other.unwrap_or("no result recorded").to_string(),
            };
            out.push_str(&format!("  last check:  {} — {result}\n", when(t)));
            // restic's output runs long; the lines that name the damage come
            // first, and the check pod's log has the rest.
            if let Some(error) = get("lastCheckError") {
                let lines: Vec<&str> = error
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect();
                for line in lines.iter().take(CHECK_ERROR_LINES) {
                    out.push_str(&format!("               {line}\n"));
                }
                if lines.len() > CHECK_ERROR_LINES {
                    out.push_str(&format!(
                        "               … {} more line(s): the check pod's log, or `apprafter \
                         backup check`, has all of restic's output\n",
                        lines.len() - CHECK_ERROR_LINES
                    ));
                }
            }
        }
        None => out.push_str("  last check:  none recorded yet\n"),
    }
    match get("lastPrune") {
        Some(t) => {
            let after = match get("lastPruneBy") {
                Some("backup") => "after a backup",
                _ => "after the weekly check",
            };
            let result = match get("lastPruneResult") {
                Some("not-permitted") => "NOT PERMITTED",
                Some("failed") => "FAILED",
                Some("pruned") => "pruned",
                Some("nothing-to-prune") => "nothing to prune",
                other => other.unwrap_or("no result recorded"),
            };
            out.push_str(&format!("  last prune:  {} {after} — {result}\n", when(t)));
            if let Some(detail) = get("lastPruneDetail") {
                out.push_str(&format!("               {detail}\n"));
            }
        }
        None => out.push_str("  last prune:  none in the cluster yet\n"),
    }
    match (get("repoStatsAt"), num("repoBytes")) {
        (Some(at), Some(bytes)) => {
            let mut counts = Vec::new();
            if let Some(n) = num("repoSnapshots") {
                counts.push(format!("{n} snapshots"));
            }
            if let Some(n) = num("repoBlobs") {
                counts.push(format!("{n} blobs"));
            }
            let counts = if counts.is_empty() {
                String::new()
            } else {
                format!(" in {}", counts.join(" and "))
            };
            out.push_str(&format!(
                "  size:        {}{counts} ({})\n",
                human_size(bytes.max(0) as u64),
                when(at)
            ));
            if let (Some(prev_at), Some(prev)) = (get("repoPrevStatsAt"), num("repoPrevBytes")) {
                let delta = bytes - prev;
                let mut moved = vec![format!(
                    "{}{}",
                    if delta < 0 { "-" } else { "+" },
                    human_size(delta.unsigned_abs())
                )];
                if let (Some(n), Some(p)) = (num("repoSnapshots"), num("repoPrevSnapshots")) {
                    moved.push(format!("{:+} snapshots", n - p));
                }
                if let (Some(n), Some(p)) = (num("repoBlobs"), num("repoPrevBlobs")) {
                    moved.push(format!("{:+} blobs", n - p));
                }
                out.push_str(&format!(
                    "  growth:      {} since {}\n",
                    moved.join(", "),
                    when(prev_at)
                ));
            }
        }
        _ => out.push_str("  size:        not measured yet (the weekly check measures it)\n"),
    }

    let verdict = stack
        .map(|s| crate::commands::platform::backup_retention_lines(s, now))
        .unwrap_or_default();
    if !verdict.is_empty() {
        out.push('\n');
        for line in verdict {
            out.push_str(&line);
            out.push('\n');
        }
    } else if get("lastPruneResult") == Some("not-permitted") {
        out.push_str(&format!(
            "\n{}\n  Next: `apprafter backup prune --credential-file <full-credentials.env>` \
             prunes with the operator's full credentials.\n",
            cli_core::style::warn(
                "Retention: NOT ENFORCED — the cluster's key may not delete, so the last prune \
                 deleted nothing and the repository keeps growing."
            )
        ));
    }
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

/// The Jobs `backup status` and `backup run` report on: `.items[]` of a Jobs
/// listing, narrowed to the backup and check runners ([`runner_job`]).
///
/// Pure — extracted from [`run_backup_status`] and called from both there and
/// the tests. INVARIANT: the filter is applied here, so an unrelated Job in
/// `apprafter-system` never gets reported as somebody's backup, and a runner
/// Job is found whatever it is called.
fn backup_jobs_of(jobs_list: Option<&Value>) -> Vec<Value> {
    jobs_list
        .map(items_of)
        .unwrap_or_default()
        .into_iter()
        .filter(|j| runner_job(j).is_some())
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

    // 2. List Jobs in apprafter-system and keep the runners.
    let jobs_list = kubectl_get_json("jobs", None, Some(PLATFORMSTACK_NAMESPACE), kc.path())?;
    let jobs = backup_jobs_of(jobs_list.as_ref());

    // 2b. The pods, only when a Job has not finished: a finished Job's
    //     conditions say everything, and an unfinished one's `active` count
    //     cannot tell a working runner from one no node has room for.
    let pods = if jobs
        .iter()
        .any(|j| job_run_outcome(j) == JobOutcome::Running)
    {
        kubectl_get_json("pods", None, Some(PLATFORMSTACK_NAMESPACE), kc.path())?
            .as_ref()
            .map(items_of)
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // 3. Fetch the runner status ConfigMap.
    let status_cm = kubectl_get_json(
        "configmap",
        Some("apprafter-backup-status"),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    )?;

    let zone = readers_zone();
    println!(
        "{}",
        format_backup_status(
            spec_backup.as_ref(),
            &jobs,
            &pods,
            status_cm.as_ref(),
            last_prune.as_deref(),
            &chrono::Local,
            zone.as_deref(),
        )
    );
    // The repository and retention (WI-389), for an enabled schedule.
    if spec_backup
        .as_ref()
        .and_then(|s| s.get("enabled"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        println!(
            "{}",
            format_repository_status(
                status_cm.as_ref(),
                ps.as_ref(),
                &chrono::Local,
                zone.as_deref(),
                chrono::Utc::now(),
            )
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (pure helpers — the tested core)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use job_pod::UNSCHEDULABLE_GRACE;
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

    // ------------------------------------------------------------------
    // Repository size + snapshot contents
    // ------------------------------------------------------------------

    #[test]
    fn a_size_is_rendered_at_the_scale_a_reader_thinks_in() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(432 * 1024 * 1024), "432.0 MiB");
        assert_eq!(
            human_size(3 * 1024 * 1024 * 1024 + 512 * 1024 * 1024),
            "3.5 GiB"
        );
    }

    #[test]
    fn repository_stats_come_out_of_resticss_own_json() {
        // Captured from `restic 0.18.1 stats --json --mode raw-data`.
        let json = r#"{"total_size":307,"total_uncompressed_size":442,
                       "compression_ratio":1.43,"total_blob_count":2,"snapshots_count":1}"#;
        let s = parse_stats_json(json).expect("parsed");
        assert_eq!(s.total_size, 307);
        assert_eq!(s.snapshots_count, Some(1));
        // A single-snapshot stats call omits snapshots_count; the size is
        // still the answer and must not be discarded with it.
        let one = parse_stats_json(r#"{"total_size":4096}"#).expect("parsed");
        assert_eq!(one.total_size, 4096);
        assert_eq!(one.snapshots_count, None);
        assert!(parse_stats_json("not json").is_none());
    }

    #[test]
    fn the_manifest_is_found_by_its_path_in_the_snapshot() {
        // `restic ls --json` emits one object per line: the snapshot
        // first, then every node. The staging directory the runner
        // snapshots is a temp path that changes every run, so the
        // manifest can only be found by its NAME, never by a fixed path.
        let ls = r#"{"time":"2026-09-10T22:11:39Z","paths":["/staging/x"],"struct_type":"snapshot"}
{"name":"crs","type":"dir","path":"/staging/ar-9f2/crs","struct_type":"node"}
{"name":"manifest.json","type":"file","path":"/staging/ar-9f2/manifest.json","struct_type":"node"}
{"name":"web.json","type":"file","path":"/staging/ar-9f2/crs/web.json","struct_type":"node"}"#;
        assert_eq!(
            manifest_path_in_snapshot(ls).as_deref(),
            Some("/staging/ar-9f2/manifest.json")
        );
        // A snapshot that carries no manifest is not an AppRafter backup,
        // and saying so beats dumping a path that does not exist.
        assert_eq!(
            manifest_path_in_snapshot(r#"{"name":"f","path":"/f"}"#),
            None
        );
    }

    #[test]
    fn a_claim_type_is_read_from_the_key_the_manifest_actually_uses() {
        // Live output read `ResourceClaim 1 (unspecified 1)` against a
        // cluster whose claim has a type. `ResourceRef` carries no
        // `rename_all`, so it serialises SNAKE_CASE — `claim_type`, not
        // `claimType`. Reading the camelCase key found nothing and the
        // breakdown said "unspecified" for every claim there will ever be.
        let manifest = json!({
            "clusterId": "c", "createdAt": "t", "platformVersion": "v",
            "namespaces": ["demo"],
            "resources": [
                {"namespace": "demo", "kind": "ResourceClaim", "name": "db", "claim_type": "pg"}
            ]
        });
        let s = format_snapshot_contents("id", None, None, &manifest, None, &tokyo(), None);
        assert!(s.contains("pg 1"), "{s}");
        assert!(!s.contains("unspecified"), "{s}");
    }

    #[test]
    fn secrets_are_counted_from_the_snapshot_tree_not_the_manifest() {
        // The manifest lists CRs and claims only — `resource_refs` never
        // adds secrets — so counting `kind == "Secret"` there reported 0
        // against a cluster whose backup held nine of them. They are in
        // the snapshot as `secrets/<ns>/<name>.json`, which is where the
        // count has to come from.
        let ls = r#"{"struct_type":"snapshot","paths":["/staging/x"]}
{"name":"secrets","type":"dir","path":"/staging/x/secrets","struct_type":"node"}
{"name":"api-ai.json","type":"file","path":"/staging/x/secrets/shop/api-ai.json","struct_type":"node"}
{"name":"api-s3.json","type":"file","path":"/staging/x/secrets/shop/api-s3.json","struct_type":"node"}
{"name":"srccred-x.json","type":"file","path":"/staging/x/secrets/sourcecred/srccred-x.json","struct_type":"node"}
{"name":"web.json","type":"file","path":"/staging/x/crs/web.json","struct_type":"node"}"#;
        assert_eq!(count_secret_files(ls), 3);
        // A snapshot with no secrets dir is a real zero, not an unknown.
        assert_eq!(
            count_secret_files(r#"{"name":"web.json","type":"file","path":"/s/crs/web.json"}"#),
            0
        );
    }

    #[test]
    fn show_says_where_the_secrets_came_from_when_it_is_wider() {
        // The capture follows SealedSecrets, so a backup can hold secrets
        // from a namespace that has no Application — and a header listing
        // only the app namespaces would read as if those were missing.
        let manifest = json!({
            "clusterId": "c", "createdAt": "t", "platformVersion": "v",
            "namespaces": ["apprafter", "procvue"],
            "secretNamespaces": ["apprafter", "apprafter-system", "laundry-assistant"],
            "resources": []
        });
        let s = format_snapshot_contents("id", None, None, &manifest, Some(4), &tokyo(), None);
        assert!(s.contains("apprafter, procvue"), "{s}");
        assert!(s.contains("laundry-assistant"), "names the wider set: {s}");

        // Same set on both: one line, not two saying the same thing.
        let same = json!({
            "clusterId": "c", "createdAt": "t", "platformVersion": "v",
            "namespaces": ["shop"], "secretNamespaces": ["shop"], "resources": []
        });
        let t = format_snapshot_contents("id", None, None, &same, Some(1), &tokyo(), None);
        assert_eq!(t.matches("shop").count(), 1, "{t}");
    }

    #[test]
    fn the_secret_count_reaches_both_surfaces() {
        let manifest = json!({
            "clusterId": "c", "createdAt": "t", "platformVersion": "v",
            "namespaces": ["demo"], "resources": []
        });
        let s = format_snapshot_contents("id", None, None, &manifest, Some(9), &tokyo(), None);
        assert!(s.contains("Secret") && s.contains('9'), "{s}");
    }

    #[test]
    fn the_contents_summary_counts_what_an_operator_asks_about() {
        let manifest = json!({
            "manifestVersion": 1,
            "clusterId": "prod",
            "createdAt": "2026-09-10T22:11:34Z",
            "platformVersion": "0.2.67",
            "namespaces": ["shop", "blog"],
            "resources": [
                {"namespace": "shop", "kind": "Application", "name": "web"},
                {"namespace": "shop", "kind": "Application", "name": "api"},
                {"namespace": "blog", "kind": "Application", "name": "blog"},
                {"namespace": "shop", "kind": "ResourceClaim", "name": "db", "claimType": "pg"},
                {"namespace": "blog", "kind": "ResourceClaim", "name": "db2", "claimType": "pg"},
                {"namespace": "shop", "kind": "ResourceClaim", "name": "cache", "claimType": "redis"},
                {"namespace": "shop", "kind": "SharedVolume", "name": "uploads"}
            ]
        });
        let s = format_snapshot_contents(
            "354fb34e",
            Some("2026-09-10T22:11:39Z"),
            Some(1_234_567),
            &manifest,
            // Secrets never appear in `resources`; they are counted from
            // the snapshot tree and passed in.
            Some(11),
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("354fb34e"), "{s}");
        assert!(s.contains("2026-09-11 07:11:39 Asia/Tokyo"), "{s}");
        assert!(s.contains("prod"), "names the cluster: {s}");
        assert!(s.contains("0.2.67"), "names the platform version: {s}");
        assert!(s.contains("1.2 MiB"), "{s}");
        assert!(s.contains("shop, blog"), "{s}");
        assert!(s.contains("Application") && s.contains('3'), "{s}");
        assert!(s.contains("Secret"), "{s}");
        // The claim breakdown is the "which databases" question, and a
        // bare "ResourceClaim 3" does not answer it.
        assert!(s.contains("pg 2"), "{s}");
        assert!(s.contains("redis 1"), "{s}");
        assert!(s.contains("SharedVolume"), "{s}");
    }

    #[test]
    fn the_detail_columns_show_where_the_contents_changed() {
        // What `--details` is for: two snapshots side by side, and the
        // row where a count moves is the run where something entered or
        // left the cluster.
        let rows = vec![
            SnapshotDetail {
                id: "354fb34e".into(),
                time: "2026-09-10T22:11:39Z".into(),
                cluster: "prod".into(),
                size: Some(432 * 1024 * 1024),
                counts: Some(ContentCounts {
                    apps: 4,
                    secrets: 11,
                    claims: 3,
                }),
            },
            SnapshotDetail {
                id: "9c1d0a77".into(),
                time: "2026-09-11T02:00:04Z".into(),
                cluster: "prod".into(),
                size: Some(433 * 1024 * 1024),
                counts: Some(ContentCounts {
                    apps: 5,
                    secrets: 11,
                    claims: 3,
                }),
            },
        ];
        let table = format_detail_table("s3:x", &rows, &tokyo(), Some("Asia/Tokyo"));
        assert!(table.contains("432.0 MiB"), "{table}");
        assert!(table.contains("SIZE") && table.contains("APPS"), "{table}");
        assert!(table.contains("2026-09-11 07:11:39"), "{table}");
        // Columns line up — this table exists to be read DOWN, and a
        // count that moves between rows is the whole signal. Measured on
        // the last column boundary, which only holds if every column
        // before it holds too.
        let claims_column: Vec<usize> = table
            .lines()
            .filter(|l| l.contains("CLAIMS") || l.contains("354fb34e") || l.contains("9c1d0a77"))
            .map(|l| l.rfind("  ").map(|i| i + 2).expect("column gap"))
            .collect();
        assert_eq!(claims_column.len(), 3, "{table}");
        assert!(
            claims_column.windows(2).all(|w| w[0] == w[1]),
            "CLAIMS must start at one column on every line: {table}"
        );
    }

    #[test]
    fn a_snapshot_whose_manifest_could_not_be_read_still_gets_a_row() {
        // One unreadable snapshot in a listing of ten must not take the
        // other nine down with it — and a dash says "not known" where a
        // zero would say "none", which is a different claim entirely.
        let rows = vec![SnapshotDetail {
            id: "deadbeef".into(),
            time: "2026-09-10T22:11:39Z".into(),
            cluster: "prod".into(),
            size: None,
            counts: None,
        }];
        let table = format_detail_table("s3:x", &rows, &tokyo(), None);
        assert!(table.contains("deadbeef"), "{table}");
        assert!(table.contains('—') || table.contains('-'), "{table}");
        assert!(!table.contains(" 0 "), "a dash is not a zero: {table}");
    }

    #[test]
    fn a_manifest_from_a_future_cli_still_renders_what_it_can() {
        // Unknown kinds are counted under their own name rather than
        // dropped: a listing that silently omits resources is worse than
        // one that names something the reader has to look up.
        let manifest = json!({
            "clusterId": "c", "createdAt": "t", "platformVersion": "9.9.9",
            "namespaces": [],
            "resources": [{"namespace": "n", "kind": "SomethingNew", "name": "x"}]
        });
        let s = format_snapshot_contents("id", None, None, &manifest, None, &tokyo(), None);
        assert!(s.contains("SomethingNew"), "{s}");
    }

    // ------------------------------------------------------------------
    // `backup set` — change one field of a configured backup
    // ------------------------------------------------------------------

    #[test]
    fn a_field_the_crd_dropped_is_reported_rather_than_celebrated() {
        // Structural pruning: the apiserver takes a merge-patch carrying
        // a field its CRD does not define, answers 200, and silently
        // drops it. `spec.backup.checkReadDataSubset` arrived with
        // operator v0.2.48, so on any older cluster `set check-depth 10%`
        // would print a tick and change nothing — the same failure the
        // timeZone read-back was added for.
        let patched = json!({"checkReadData": false, "checkReadDataSubset": "10%"});
        assert!(set_readback_error("check-depth", &patched, Some(&patched)).is_none());

        let pruned = json!({"checkReadData": false});
        let err = set_readback_error("check-depth", &patched, Some(&pruned))
            .expect("a dropped field must be reported");
        let msg = err.to_string();
        assert!(
            msg.contains("checkReadDataSubset"),
            "names the field: {msg}"
        );
        assert!(msg.contains("upgrade") || msg.contains("Upgrade"), "{msg}");
    }

    #[test]
    fn a_readback_that_cannot_be_taken_does_not_invent_a_failure() {
        // No CR came back — the write already succeeded, and reporting a
        // failure because the follow-up read did not answer would be a
        // wrong answer about a change that landed.
        let patched = json!({"schedule": "30 4 * * *"});
        assert!(set_readback_error("at", &patched, None).is_none());
    }

    #[test]
    fn set_check_depth_writes_the_three_shapes_of_verification() {
        // Structure-only, a subset, and the whole repository. The pair of
        // fields is written TOGETHER every time: leaving the other one at
        // its previous value would make the result depend on what was
        // there before, which is exactly the surprise `set` exists to
        // remove.
        let structure = backup_set_patch("check-depth", "structure").unwrap();
        assert_eq!(structure["spec"]["backup"]["checkReadData"], json!(false));
        assert_eq!(
            structure["spec"]["backup"]["checkReadDataSubset"],
            json!("")
        );

        let subset = backup_set_patch("check-depth", "10%").unwrap();
        assert_eq!(subset["spec"]["backup"]["checkReadData"], json!(false));
        assert_eq!(
            subset["spec"]["backup"]["checkReadDataSubset"],
            json!("10%")
        );

        let full = backup_set_patch("check-depth", "full").unwrap();
        assert_eq!(full["spec"]["backup"]["checkReadData"], json!(true));
        // Cleared, not left dangling: `--read-data` wins in the chart, so
        // a stale subset would sit in the CR meaning nothing.
        assert_eq!(full["spec"]["backup"]["checkReadDataSubset"], json!(""));
    }

    #[test]
    fn set_check_depth_takes_restics_own_subset_grammar() {
        for ok in ["10%", "2.5%", "1/12", "500M", "2G"] {
            assert!(backup_set_patch("check-depth", ok).is_ok(), "{ok}");
        }
        // Not a percentage, not a fraction, not a size — restic would
        // reject it INSIDE the weekly Job, at 06:00 on a Sunday, where
        // the failure is a red Job and no operator.
        for bad in ["10", "%", "some", "10 %", "1/", "-5%"] {
            let err = backup_set_patch("check-depth", bad)
                .expect_err(bad)
                .to_string();
            assert!(err.contains("check-depth"), "names the key: {err}");
        }
    }

    #[test]
    fn a_subset_ending_in_a_multi_byte_character_is_refused_not_a_panic() {
        // `split_at(len - 1)` landed inside `é` and panicked, taking the CLI
        // down on a typo instead of naming the grammar.
        for bad in ["5é", "é", "10€", "5ǵ", "1/1é"] {
            assert!(!is_read_data_subset(bad), "{bad}");
            let err = backup_set_patch("check-depth", bad)
                .expect_err(bad)
                .to_string();
            assert!(err.contains("check-depth"), "names the key: {err}");
        }
        assert!(!is_read_data_subset(""));
    }

    #[test]
    fn set_writes_only_the_field_it_was_given() {
        // The whole point: `enable` rewrites `spec.backup` wholesale, so
        // it cannot be used to change one thing — it resets every field
        // the operator configured elsewhere. A merge-patch of one key
        // touches one key.
        let p = backup_set_patch("at", "04:30").unwrap();
        let backup = p["spec"]["backup"].as_object().unwrap();
        assert_eq!(backup.len(), 1, "{backup:?}");
        assert_eq!(backup["schedule"], json!("30 4 * * *"));
    }

    #[test]
    fn set_check_off_writes_the_empty_schedule_that_omits_the_cronjob() {
        let p = backup_set_patch("check", "off").unwrap();
        assert_eq!(p["spec"]["backup"]["checkSchedule"], json!(""));
        let at = backup_set_patch("check", "06:00").unwrap();
        assert_eq!(at["spec"]["backup"]["checkSchedule"], json!("0 6 * * 0"));
    }

    /// The switch, settable on its own — the way back from a `backup disable`
    /// and from a restore, both of which leave a COMPLETE configuration
    /// switched off. FIRES on the patch shape: one key, a real JSON boolean
    /// (the CRD field is `boolean`, so the string `"true"` would be rejected
    /// or pruned), and nothing else touched.
    #[test]
    fn set_enabled_flips_only_the_switch() {
        let on = backup_set_patch("enabled", "true").unwrap();
        let backup = on["spec"]["backup"].as_object().unwrap();
        assert_eq!(backup.len(), 1, "only the switch: {backup:?}");
        assert_eq!(backup["enabled"], json!(true));

        let off = backup_set_patch("enabled", "false").unwrap();
        assert_eq!(off["spec"]["backup"]["enabled"], json!(false));

        // DOES NOT FIRE on nonsense: a typo must not silently read as `false`
        // and switch a schedule off.
        let err = backup_set_patch("enabled", "yes please")
            .expect_err("an unparseable value must refuse, not default")
            .to_string();
        assert!(err.contains("true"), "{err}");
    }

    #[test]
    fn set_validates_the_values_it_forwards() {
        assert!(backup_set_patch("keep-daily", "14").is_ok());
        assert!(backup_set_patch("keep-daily", "-1").is_err());
        assert!(backup_set_patch("staging-mode", "sequential").is_ok());
        assert!(backup_set_patch("staging-mode", "whatever").is_err());
        assert!(backup_set_patch("enforce", "cluster").is_ok());
        assert!(backup_set_patch("timezone", "Europe/Lisbon").is_ok());
        assert!(backup_set_patch("timezone", "CET-1CEST,M3.5.0").is_err());
    }

    /// The three retention modes the CRD takes, and nothing else: a typo
    /// would otherwise reach the apiserver as a 422, or — on an older CRD
    /// without `check` — look like a platform bug.
    #[test]
    fn set_enforce_takes_the_three_modes_and_says_what_each_does() {
        for mode in ["check", "cluster", "operator"] {
            assert_eq!(
                backup_set_patch("enforce", mode).unwrap(),
                json!({"spec": {"backup": {"retention": {"enforce": mode}}}})
            );
        }
        let err = backup_set_patch("enforce", "weekly")
            .unwrap_err()
            .to_string();
        for says in [
            "`check`",
            "`cluster`",
            "`operator`",
            "after a check that passed",
        ] {
            assert!(err.contains(says), "{says}: {err}");
        }
        assert!(validate_enable_enums(&EnableOpts {
            enforce: Some("check".into()),
            ..Default::default()
        })
        .is_ok());
        assert!(validate_enable_enums(&EnableOpts {
            enforce: Some("Check".into()),
            ..Default::default()
        })
        .is_err());
    }

    #[test]
    fn set_validates_the_timezone_it_forwards() {
        assert!(backup_set_patch("timezone", "Europe/Lisbon").is_ok());
        assert!(backup_set_patch("timezone", "CET-1CEST,M3.5.0").is_err());
    }

    #[test]
    fn set_deadline_writes_seconds_to_the_field_the_chart_reads() {
        let patch = backup_set_patch("deadline", "12h").unwrap();
        assert_eq!(
            patch,
            json!({"spec": {"backup": {"activeDeadlineSeconds": 43200}}})
        );
        let patch = backup_set_patch("check-deadline", "90m").unwrap();
        assert_eq!(
            patch,
            json!({"spec": {"backup": {"checkActiveDeadlineSeconds": 5400}}})
        );
        assert_eq!(
            backup_set_patch("deadline", "600s").unwrap()["spec"]["backup"]
                ["activeDeadlineSeconds"],
            json!(600)
        );
    }

    #[test]
    fn set_deadline_refuses_what_it_cannot_read_one_way() {
        for bad in [
            "6",                     // hours? seconds? — the ambiguity itself
            "9m",                    // under the ten-minute floor the CRD enforces
            "599s",                  // likewise
            "1.5h",                  // no fractions
            "6h30m",                 // one unit
            "-6h",                   // no sign
            "h",                     // no number
            "6d",                    // no days: a deadline that long outlives a daily slot
            "",                      // nothing
            "99999999999999999999h", // overflow
            "6ｈ",                   // a fullwidth h: multi-byte, refused rather than sliced
        ] {
            let err = backup_set_patch("deadline", bad)
                .expect_err(bad)
                .to_string();
            assert!(err.contains("deadline"), "names the key for {bad:?}: {err}");
        }
    }

    #[test]
    fn an_unknown_key_lists_the_ones_that_exist() {
        let err = backup_set_patch("bucket", "s3:elsewhere")
            .expect_err("bucket is not settable this way")
            .to_string();
        assert!(err.contains("check-depth"), "enumerates the keys: {err}");
        // Moving a repository is not a field edit — it is a new
        // repository, with its own init and its own first backup.
        assert!(err.contains("backup enable"), "{err}");
    }

    #[test]
    fn status_timestamps_are_in_the_readers_zone_and_name_it() {
        // Live output the operator pasted:
        //   lastSuccess:    2026-09-10T22:11:40.897841299+00:00
        // — the schedule two lines above it says "03:00 Europe/Lisbon",
        // so the same screen reported one time in their zone and another
        // in UTC, to the nanosecond, with nothing saying which was which.
        let spec = json!({
            "enabled": true, "bucket": "s3:x", "schedule": "0 3 * * *",
            "stagingMode": "monolithic", "timeZone": "Europe/Lisbon"
        });
        let cm = json!({"data": {
            "lastSuccess": "2026-09-10T22:11:40.897841299+00:00",
            "lastRunFormat": "monolithic"
        }});
        let s = format_backup_status(
            Some(&spec),
            &[],
            &[],
            Some(&cm),
            Some("2026-09-09T02:30:00Z"),
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("2026-09-11 07:11:40 Asia/Tokyo"), "{s}");
        assert!(!s.contains("897841299"), "{s}");
        // The prune stamp is the same kind of value and gets the same
        // treatment — it read as UTC too.
        assert!(s.contains("2026-09-09 11:30:00 Asia/Tokyo"), "{s}");
    }

    #[test]
    fn a_job_line_says_when_it_ran() {
        // "Last backup Job: … — Succeeded" answers whether, never when,
        // so a Job from last week and one from ten minutes ago look the
        // same — on the one screen an operator opens to find out whether
        // backup is current.
        let spec = json!({"enabled": true, "bucket": "s3:x", "schedule": "0 3 * * *"});
        let job = json!({
            "metadata": {"name": "apprafter-backup-manual-20260910-221128",
                         "labels": {"apprafter.io/manual": "true"}},
            "status": {"startTime": "2026-09-10T22:11:28Z", "succeeded": 1}
        });
        let s = format_backup_status(
            Some(&spec),
            std::slice::from_ref(&job),
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("Succeeded"), "{s}");
        assert!(s.contains("2026-09-11 07:11:28"), "names when it ran: {s}");
    }

    #[test]
    fn a_status_timestamp_that_does_not_parse_is_left_alone() {
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let cm = json!({"data": {"lastSuccess": "some day"}});
        let s = format_backup_status(
            Some(&spec),
            &[],
            &[],
            Some(&cm),
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("some day"), "{s}");
    }

    #[test]
    fn status_disabled_when_no_spec_backup() {
        let s = format_backup_status(None, &[], &[], None, None, &tokyo(), Some("Asia/Tokyo"));
        assert!(s.to_lowercase().contains("disabled"));
    }

    #[test]
    fn status_disabled_when_enabled_false() {
        let spec = json!({"enabled": false, "bucket": "s3:x"});
        let s = format_backup_status(
            Some(&spec),
            &[],
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.to_lowercase().contains("disabled"));
        // Config is retained and shown even when disabled.
        assert!(s.contains("s3:x"));
    }

    #[test]
    fn status_renders_enabled_config_and_last_prune() {
        let spec = json!({"enabled": true, "bucket": "s3:x", "schedule": "0 3 * * *", "stagingMode": "monolithic"});
        let s = format_backup_status(
            Some(&spec),
            &[],
            &[],
            None,
            Some("2026-07-17T03:00:00Z"),
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("s3:x"));
        // +09:00 of 03:00Z is noon, and the zone is named.
        assert!(s.contains("2026-07-17 12:00:00 Asia/Tokyo"), "{s}");
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
        let s = format_backup_status(
            Some(&spec),
            &[],
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
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
        let s = format_backup_status(
            Some(&spec),
            &[],
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("check:         off"), "{s}");
    }

    #[test]
    fn status_names_the_missing_zone_instead_of_printing_a_bare_time() {
        // A cluster enabled before 2.22g has no timeZone. Printing "03:00"
        // alone reads as local time; it is actually the
        // kube-controller-manager's zone, which is the trap D2 is about.
        let spec = json!({"enabled": true, "bucket": "s3:x", "schedule": "0 3 * * *"});
        let s = format_backup_status(
            Some(&spec),
            &[],
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
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
        let s = format_backup_status(
            Some(&spec),
            &[],
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("*/5 * * * * UTC"), "{s}");
    }

    #[test]
    fn status_reports_job_outcome() {
        let job = json!({
            "metadata": {"name": "apprafter-backup-28900000", "ownerReferences": owned_by(BACKUP_CRONJOB_NAME)},
            "status": {"succeeded": 1}
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(
            Some(&spec),
            std::slice::from_ref(&job),
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("apprafter-backup-28900000"));
        assert!(s.contains("Succeeded"));
    }

    #[test]
    fn status_names_why_a_job_failed_when_the_job_says() {
        // A run stopped at its deadline: the pods say only "failed: 1", the
        // Job's condition says why — and that is the difference between
        // "raise the deadline" and "read the log".
        let job = json!({
            "metadata": {"name": "apprafter-backup-28900000", "ownerReferences": owned_by(BACKUP_CRONJOB_NAME)},
            "status": {
                "startTime": "2026-07-17T03:00:00Z",
                "failed": 1,
                "conditions": [
                    {"type": "FailureTarget", "status": "True", "reason": "DeadlineExceeded",
                     "message": "Job was active longer than specified deadline"},
                    {"type": "Failed", "status": "True", "reason": "DeadlineExceeded",
                     "message": "Job was active longer than specified deadline"}
                ]
            }
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(
            Some(&spec),
            std::slice::from_ref(&job),
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(
            s.contains(
                "Last backup Job: apprafter-backup-28900000 — Failed: DeadlineExceeded: Job was \
                 active longer than specified deadline (2026-07-17 12:00:00 Asia/Tokyo)"
            ),
            "{s}"
        );
    }

    #[test]
    fn a_job_still_running_is_read_off_its_pods() {
        // No terminal condition yet: the pod counts are all there is, and a
        // Job between a failed attempt and its retry must not read as a
        // terminal failure with a reason it does not have.
        let running = json!({
            "metadata": {"name": "apprafter-backup-28900000"},
            "status": {"active": 1, "failed": 1}
        });
        assert_eq!(job_line_outcome(&running, &[]), "Running");
        let done = json!({
            "metadata": {"name": "apprafter-backup-28900000"},
            "status": {"succeeded": 1,
                       "conditions": [{"type": "Complete", "status": "True"}]}
        });
        assert_eq!(job_line_outcome(&done, &[]), "Succeeded");
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
        let s = format_backup_status(
            Some(&spec),
            &[],
            &[],
            Some(&cm),
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        // Rendered in the reader's zone now (+09:00 of 03:00Z is noon),
        // which is the whole point of the change; what this test guards
        // is that BOTH keys survive, not their formatting.
        assert!(
            s.contains("2026-07-17 12:00:00 Asia/Tokyo"),
            "lastSuccess not rendered: {s}"
        );
        assert!(
            s.contains("2026-07-16 12:00:00 Asia/Tokyo"),
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
        let s = format_backup_status(
            Some(&spec),
            &[],
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("Last prune: never"));
    }

    /// WI-389: who prunes is always said, set or not.
    #[test]
    fn status_says_who_prunes_even_when_it_was_never_set() {
        let unset = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(Some(&unset), &[], &[], None, None, &tokyo(), None);
        assert!(
            s.contains("enforce: not set (the platform's default)"),
            "{s}"
        );
        let set = json!({"enabled": true, "bucket": "s3:x",
                         "retention": {"keepDaily": 5, "enforce": "operator"}});
        let s = format_backup_status(Some(&set), &[], &[], None, None, &tokyo(), None);
        assert!(s.contains("keepDaily: 5"), "{s}");
        assert!(s.contains("enforce: operator"), "{s}");
        assert!(!s.contains("not set"), "{s}");
    }

    /// The scoped key's week, as the runner records it.
    fn scoped_record() -> Value {
        json!({"data": {
            "lastSuccess": "2026-09-20T03:01:00+00:00",
            "lastCheck": "2026-09-20T06:00:30+00:00", "lastCheckResult": "passed",
            "lastCheckError": "",
            "lastPrune": "2026-09-20T06:00:41+00:00", "lastPruneResult": "not-permitted",
            "lastPruneBy": "check",
            "lastPruneDetail": "not permitted: the storage refused to delete snapshot ecd0be32 \
                (Remove(<snapshot/ecd0be3219>) failed: client.RemoveObject: Access Denied.), so \
                nothing was deleted; 9 snapshot(s) of 9 run(s) are past the keep policy",
            "repoStatsAt": "2026-09-20T06:00:44+00:00", "repoBytes": "1288490189",
            "repoSnapshots": "42", "repoBlobs": "310512",
            "repoPrevStatsAt": "2026-09-13T06:00:40+00:00", "repoPrevBytes": "1125908480",
            "repoPrevSnapshots": "35", "repoPrevBlobs": "299492",
        }})
    }

    fn now_utc() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-23T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn repository_status_shows_the_check_the_prune_the_size_and_the_growth() {
        let s = format_repository_status(
            Some(&scoped_record()),
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
            now_utc(),
        );
        assert!(
            s.contains("last check:  2026-09-20 15:00:30 Asia/Tokyo — passed"),
            "{s}"
        );
        assert!(
            s.contains("last prune:  2026-09-20 15:00:41 Asia/Tokyo after the weekly check — NOT PERMITTED"),
            "{s}"
        );
        assert!(s.contains("Access Denied"), "{s}");
        assert!(
            s.contains("size:        1.2 GiB in 42 snapshots and 310512 blobs"),
            "{s}"
        );
        assert!(
            s.contains("growth:      +155.1 MiB, +7 snapshots, +11020 blobs since 2026-09-13"),
            "{s}"
        );
        // No operator verdict to read: the record alone still warns.
        assert!(s.contains("Retention: NOT ENFORCED"), "{s}");
        assert!(
            s.contains("--credential-file <full-credentials.env>"),
            "{s}"
        );
    }

    #[test]
    fn repository_status_prints_the_operators_retention_verdict_when_there_is_one() {
        let stack = json!({
            "spec": {"backup": {"enabled": true}},
            "status": {"conditions": [{
                "type": "BackupRetention", "status": "False", "reason": "PruneNotPermitted",
                "message": "retention is not enforced: the cluster's S3 key may not delete",
                "lastTransitionTime": "2026-09-20T06:00:41Z"}]},
        });
        let s = format_repository_status(
            Some(&scoped_record()),
            Some(&stack),
            &tokyo(),
            Some("Asia/Tokyo"),
            now_utc(),
        );
        assert!(s.contains("Retention: NOT ENFORCED since"), "{s}");
        assert!(s.contains("PruneNotPermitted"), "{s}");
        assert!(
            s.contains("backup-retention-and-checks/#who-runs-the-prune"),
            "{s}"
        );
        assert_eq!(
            s.matches("Retention:").count(),
            1,
            "one verdict, not two: {s}"
        );
    }

    #[test]
    fn repository_status_before_any_check_says_so_rather_than_nothing() {
        let s = format_repository_status(None, None, &tokyo(), None, now_utc());
        assert!(s.contains("last check:  none recorded yet"), "{s}");
        assert!(s.contains("last prune:  none in the cluster yet"), "{s}");
        assert!(s.contains("not measured yet"), "{s}");
        assert!(!s.contains("NOT ENFORCED"), "{s}");
    }

    #[test]
    fn a_failed_check_is_shown_with_its_reason_and_a_shrinking_repository_as_negative() {
        let cm = json!({
            "lastCheck": "2026-09-27T06:00:30+00:00", "lastCheckResult": "failed",
            "lastCheckError": "restic check: pack 5e1f0a2b contains 1 error",
            "repoStatsAt": "2026-09-20T06:00:44+00:00", "repoBytes": "1000",
            "repoPrevStatsAt": "2026-09-13T06:00:40+00:00", "repoPrevBytes": "3048",
        });
        let s = format_repository_status(Some(&cm), None, &tokyo(), None, now_utc());
        assert!(s.contains("— FAILED\n"), "{s}");
        assert!(
            s.contains("restic check: pack 5e1f0a2b contains 1 error"),
            "{s}"
        );
        assert!(s.contains("growth:      -2.0 KiB since"), "{s}");
        // restic's long output is cut to the lines that name the damage.
        let long = json!({
            "lastCheck": "2026-09-27T06:00:30+00:00", "lastCheckResult": "failed",
            "lastCheckError": "error for tree a83ddebe:\n  decrypting blob failed\npack 1a5c contains 2 errors\n\nThe repository contains damaged pack files.\nFatal: repository contains errors\n",
        });
        let s = format_repository_status(Some(&long), None, &tokyo(), None, now_utc());
        assert!(s.contains("pack 1a5c contains 2 errors"), "{s}");
        assert!(!s.contains("Fatal: repository contains errors"), "{s}");
        assert!(s.contains("… 2 more line(s)"), "{s}");
    }

    #[test]
    fn status_picks_most_recent_job_by_start_time() {
        let job_old = json!({
            "metadata": {"name": "apprafter-backup-28800000", "ownerReferences": owned_by(BACKUP_CRONJOB_NAME)},
            "status": {"startTime": "2026-07-16T03:00:00Z", "failed": 1}
        });
        let job_new = json!({
            "metadata": {"name": "apprafter-backup-28900000", "ownerReferences": owned_by(BACKUP_CRONJOB_NAME)},
            "status": {"startTime": "2026-07-17T03:00:00Z", "succeeded": 1}
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(
            Some(&spec),
            &[job_old, job_new],
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
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
            // Credentials pinned to a local source throughout, so this
            // table stays about the repo and nothing else.
            assert_eq!(
                backup_verb_needs_cluster(
                    repo,
                    RetentionArgs::NotApplicable,
                    CredSource::File,
                    None
                ),
                expect,
                "{why}"
            );
        }
    }

    /// A cluster UID shaped like the ones `kube-system` carries, for the
    /// offline-prune rows below.
    const OFFLINE_UID: &str = "11111111-2222-3333-4444-555555555555";

    /// E3/E4: prune needs an IDENTITY, and without `--cluster-uid` the only
    /// place one comes from is a live cluster.
    ///
    /// It used to go fully offline with `--repo` + all three `--keep-*` and
    /// nothing else. A prune deletes by explicit snapshot id, and a repository
    /// can be shared, so without a cluster's `kube-system` UID it planned
    /// across every snapshot in the bucket and forgot the co-tenant's runs.
    #[test]
    fn needs_cluster_table_prune() {
        let repo = Some("s3:https://h/b");
        let table = [
            // (repo, keeps, needs_cluster, why)
            (
                repo,
                prune_keeps(Some(7), Some(4), Some(6)),
                true,
                "--repo + all three --keep-* still needs an identity (E3)",
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
            assert_eq!(
                backup_verb_needs_cluster(r, keeps, CredSource::File, None),
                expect,
                "{why}"
            );
        }
    }

    /// …and `--cluster-uid` is the one thing that clears the identity reason.
    ///
    /// The real offline need: the cluster is gone, the repository remains, and
    /// its snapshots should be reclaimable. FIRES with the flag; the table
    /// above is the same rows without it, so the pair pins that the flag is
    /// what changed the answer and not the repo or the keeps.
    #[test]
    fn cluster_uid_is_the_offline_identity_and_nothing_else_substitutes() {
        let repo = Some("s3:https://h/b");
        let full = prune_keeps(Some(7), Some(4), Some(6));
        assert!(
            !backup_verb_needs_cluster(repo, full, CredSource::File, Some(OFFLINE_UID)),
            "--repo + --keep-* + --cluster-uid + local creds → fully offline"
        );
        // Every other input still has to come from somewhere: the flag buys
        // the identity and only the identity.
        assert!(
            backup_verb_needs_cluster(None, full, CredSource::File, Some(OFFLINE_UID)),
            "no --repo → the bucket still comes from the CR"
        );
        assert!(
            backup_verb_needs_cluster(
                repo,
                prune_keeps(Some(7), None, Some(6)),
                CredSource::File,
                Some(OFFLINE_UID)
            ),
            "a missing --keep-* still comes from the CR"
        );
        assert!(
            backup_verb_needs_cluster(repo, full, CredSource::Cluster, Some(OFFLINE_UID)),
            "credentials held only by the cluster still need it"
        );
    }

    /// `--cluster-uid` belongs to prune alone. check / unlock / show never
    /// delete, so they have no identity to claim and must not acquire one.
    #[test]
    fn the_identity_reason_exists_only_for_prune() {
        let need = cluster_need(
            Some("s3:https://h/b"),
            RetentionArgs::NotApplicable,
            CredSource::Cluster,
            None,
        );
        assert!(!need.identity);
        assert!(!need.flags.iter().any(|f| f.contains("--cluster-uid")));
    }

    #[test]
    fn needs_cluster_repo_alone_is_enough_for_check_but_not_for_prune() {
        // The asymmetry, stated on its own: `--repo` fully frees check/unlock,
        // but prune ALSO needs the retention policy, which otherwise comes from
        // `spec.backup.retention`.
        let repo = Some("s3:https://h/b");
        assert!(!backup_verb_needs_cluster(
            repo,
            RetentionArgs::NotApplicable,
            CredSource::File,
            None
        ));
        assert!(backup_verb_needs_cluster(
            repo,
            prune_keeps(Some(7), Some(4), None),
            CredSource::File,
            None
        ));
    }

    // ------------------------------------------------------------------
    // `backup show` — which snapshot the default inspects (E2)
    // ------------------------------------------------------------------

    /// A repository two clusters share, with the CO-TENANT having written
    /// last: the shape where restic's own `latest` displays a stranger's run.
    fn shared_show_listing() -> String {
        let theirs = "99999999-8888-7777-6666-555555555555";
        format!(
            r#"[
              {{"id":"mine1","short_id":"mine1","time":"2026-09-02T03:00:00Z",
                "tags":["{OFFLINE_UID}-2026-09-02T03:00:00Z"]}},
              {{"id":"theirs1","short_id":"theirs1","time":"2026-09-02T04:00:00Z",
                "tags":["{theirs}-2026-09-02T04:00:00Z"]}}
            ]"#
        )
    }

    /// FIRES: the default inspects THIS cluster's newest snapshot, not the
    /// repository's. `show` is read-only, but it is what an operator reads
    /// before choosing what to restore — a foreign answer here becomes a
    /// foreign restore.
    #[test]
    fn show_defaults_to_this_clusters_latest_not_the_repositorys() {
        let (id, _) =
            snapshot_to_show(None, Some(&shared_show_listing()), Some(OFFLINE_UID)).unwrap();
        assert_eq!(
            id, "mine1",
            "the newest snapshot in the repository is theirs"
        );
    }

    /// DOES NOT FIRE: a named snapshot is honoured exactly as given, whatever
    /// cluster wrote it and without fetching a listing at all. That is the
    /// escape hatch the ambiguity refusal points at, and narrowing must not
    /// take it away.
    #[test]
    fn show_honours_a_named_snapshot_without_reading_the_repository() {
        assert_eq!(
            snapshot_to_show(Some("theirs1"), None, Some(OFFLINE_UID)).unwrap(),
            ("theirs1".to_string(), Vec::new())
        );
        // …including with no identity at all.
        assert_eq!(
            snapshot_to_show(Some("abc123"), None, None).unwrap(),
            ("abc123".to_string(), Vec::new())
        );
    }

    /// And the ambiguous case refuses rather than displaying a guess: a fresh
    /// target facing two foreign clusters cannot be shown "the" latest.
    #[test]
    fn show_refuses_latest_when_the_repository_is_ambiguous() {
        let fresh = "abcdabcd-0000-0000-0000-abcdabcdabcd";
        let err = snapshot_to_show(None, Some(&shared_show_listing()), Some(fresh))
            .expect_err("two foreign clusters and none of ours is a guess")
            .to_string();
        assert!(err.contains("different clusters"), "{err}");
        assert!(err.contains("apprafter backup show <id>"), "{err}");
    }

    /// A complete sequential run, then a newer one a SIGINT stopped after its
    /// first claim: no commit snapshot, so no `manifest.json` in it.
    fn interrupted_show_listing() -> String {
        let done = format!("{OFFLINE_UID}-2026-09-23T03:00:00Z");
        let cut = format!("{OFFLINE_UID}-2026-09-24T03:00:00Z");
        format!(
            r#"[
              {{"id":"done0","short_id":"done0","time":"2026-09-23T03:00:01Z",
                "tags":["{done}"],"paths":["/staging/a/claim-0"]}},
              {{"id":"donec","short_id":"donec","time":"2026-09-23T03:00:02Z",
                "tags":["{done}"],"paths":["/staging/a/commit"]}},
              {{"id":"cut0","short_id":"cut0","time":"2026-09-24T03:00:01Z",
                "tags":["{cut}"],"paths":["/tmp/b/claim-0"]}}
            ]"#
        )
    }

    /// FIRES: after a sequential backup died between its claims, `backup
    /// show` resolved `latest` to the dead run's claim snapshot and said the
    /// repository "holds something else". It shows the newest complete run,
    /// and says which newer run it passed over.
    #[test]
    fn show_defaults_to_the_newest_complete_run_and_names_the_one_it_passed_over() {
        let (id, passed) =
            snapshot_to_show(None, Some(&interrupted_show_listing()), Some(OFFLINE_UID)).unwrap();
        assert_eq!(id, "donec");
        assert_eq!(passed.len(), 1, "{passed:?}");

        let lines = passed_over_lines(&passed, "donec, shown below", &chrono::Utc, Some("UTC"));
        let text = lines.join("\n");
        assert!(text.contains("did not finish"), "{text}");
        assert!(text.contains("1 snapshot(s)"), "{text}");
        assert!(
            text.contains(&format!("{}…", &OFFLINE_UID[..8])),
            "the run is named by its tag, shortened as `backup list` does: {text}"
        );
        assert!(text.contains("2026-09-24 03:00:01 UTC"), "{text}");
        assert!(text.contains("manifest.json"), "{text}");
        assert!(text.contains("still being written"), "{text}");
        assert!(text.contains("snapshot donec, shown below"), "{text}");
    }

    /// DOES NOT FIRE: with nothing passed over, not a word.
    #[test]
    fn nothing_passed_over_prints_nothing() {
        assert!(passed_over_lines(&[], "x", &chrono::Utc, None).is_empty());
    }

    // ------------------------------------------------------------------
    // The offline prune's claim, checked against the repository (E3)
    // ------------------------------------------------------------------

    /// A repository listing: `uids` are the clusters that wrote into it, plus
    /// `legacy` pre-identity snapshots.
    fn repo_listing(uids: &[&str], legacy: usize) -> Vec<Value> {
        let mut out: Vec<Value> = uids
            .iter()
            .enumerate()
            .map(|(i, u)| json!({"id": format!("s{i}"), "tags": [format!("{u}-2026-09-11T03:00:0{i}Z")]}))
            .collect();
        for i in 0..legacy {
            out.push(json!({"id": format!("l{i}"), "tags": ["platform-2026-09-11T03:00:00Z"]}));
        }
        out
    }

    /// FIRES: a UID this repository has never seen is refused, and the refusal
    /// NAMES the ones it holds — otherwise the operator has no way to find the
    /// right one with the cluster gone.
    ///
    /// This is the dangerous direction, and it is dangerous quietly: a
    /// mistyped UID matches nothing, spares every identified snapshot, and
    /// leaves the planner holding only the pre-identity ones, which it forgets
    /// by policy. Falling through would delete the wrong history in silence.
    #[test]
    fn an_offline_prune_refuses_a_uid_the_repository_has_never_seen() {
        let theirs = "99999999-8888-7777-6666-555555555555";
        let snaps = repo_listing(&[theirs], 2);
        let err = offline_prune_scope(&snaps, OFFLINE_UID)
            .expect_err("a UID matching nothing must not fall through to the legacy snapshots")
            .to_string();
        assert!(err.contains(theirs), "names the identity it holds: {err}");
        assert!(err.contains(OFFLINE_UID), "names what was asked for: {err}");
        assert!(err.contains("--all-clusters"), "{err}");
    }

    /// DOES NOT FIRE: the UID that DID write here is accepted. Without this
    /// the test above would also pass on a check that refused everything.
    #[test]
    fn an_offline_prune_accepts_a_uid_that_wrote_into_the_repository() {
        let snaps = repo_listing(&[OFFLINE_UID, "99999999-8888-7777-6666-555555555555"], 0);
        let note = offline_prune_scope(&snaps, OFFLINE_UID).expect("its own history is prunable");
        assert!(note.contains(OFFLINE_UID), "{note}");
        assert!(note.contains("2 cluster(s)"), "{note}");
    }

    /// …and a repository holding ONLY pre-identity snapshots is allowed, not
    /// refused: nothing in it carries an identity to match, every snapshot is
    /// attributed by the stated assumption, and reclaiming such a repository
    /// after its cluster is gone is exactly what the flag restores. Named in
    /// the output so the assumption is visible rather than silent.
    #[test]
    fn an_offline_prune_over_a_pre_identity_repository_is_allowed_and_says_so() {
        let snaps = repo_listing(&[], 3);
        let note =
            offline_prune_scope(&snaps, OFFLINE_UID).expect("a legacy repository is reclaimable");
        assert!(note.contains("predate"), "{note}");
        assert!(note.contains(OFFLINE_UID), "{note}");
    }

    // ------------------------------------------------------------------
    // `backup run` — trigger the scheduled backup now
    // ------------------------------------------------------------------

    /// A CronJob shaped like the platform chart's, trimmed to what the
    /// trigger reads.
    fn backup_cronjob() -> Value {
        json!({
            "apiVersion": "batch/v1",
            "kind": "CronJob",
            "metadata": {"name": "apprafter-backup", "namespace": "apprafter-system"},
            "spec": {
                "schedule": "0 3 * * *",
                "jobTemplate": {
                    "metadata": {"labels": {"app.kubernetes.io/name": "apprafter-backup"}},
                    "spec": {
                        "backoffLimit": 1,
                        "template": {"spec": {"restartPolicy": "Never", "containers": [
                            {"name": "backup", "image": "ghcr.io/x/apprafter-backup:1"}
                        ]}}
                    }
                }
            }
        })
    }

    #[test]
    fn a_manual_run_reuses_the_schedules_own_job_template() {
        // The whole point of triggering through the CronJob rather than
        // building a Job from scratch: image, service account, mounts,
        // env and resources are whatever the chart deployed. A hand-built
        // Job would drift from the schedule the moment either changed,
        // and the manual run would stop proving anything about the real
        // one.
        let job = job_from_cronjob(&backup_cronjob(), "apprafter-backup-manual-20260910-215301")
            .expect("template present");
        assert_eq!(job["kind"], "Job");
        assert_eq!(job["apiVersion"], "batch/v1");
        assert_eq!(
            job["metadata"]["name"],
            "apprafter-backup-manual-20260910-215301"
        );
        assert_eq!(job["metadata"]["namespace"], "apprafter-system");
        assert_eq!(
            job["spec"]["template"]["spec"]["containers"][0]["image"],
            "ghcr.io/x/apprafter-backup:1"
        );
        assert_eq!(job["spec"]["backoffLimit"], 1);
    }

    #[test]
    fn a_manual_run_is_labelled_as_one() {
        let job = job_from_cronjob(&backup_cronjob(), "apprafter-backup-manual-x").unwrap();
        // The template's own labels survive — `backup status` finds Jobs
        // by them.
        assert_eq!(
            job["metadata"]["labels"]["app.kubernetes.io/name"],
            "apprafter-backup"
        );
        // …and the run is marked, so a manual backup is distinguishable
        // from a 03:00 one in `kubectl get jobs` and in an incident.
        assert_eq!(job["metadata"]["labels"]["apprafter.io/manual"], "true");
        assert_eq!(
            job["metadata"]["annotations"]["cronjob.kubernetes.io/instantiate"],
            "manual"
        );
    }

    #[test]
    fn a_cronjob_without_a_template_is_named_in_the_error() {
        let broken = json!({"metadata": {"name": "apprafter-backup"}, "spec": {}});
        let err = job_from_cronjob(&broken, "x").unwrap_err().to_string();
        assert!(err.contains("jobTemplate"), "names what is missing: {err}");
    }

    #[test]
    fn the_deployed_cronjob_says_which_repo_it_would_write_to() {
        // `enable` patches the CR; Argo CD renders the CronJob from it
        // some minutes later. Between those two moments a CronJob EXISTS
        // but still carries the previous repo — so "the CronJob is there"
        // is not the question. This is: does the deployed one already
        // write where the CR now says?
        let cj = json!({"spec": {"jobTemplate": {"spec": {"template": {"spec": {
            "containers": [{"name": "runner", "env": [
                {"name": "RESTIC_PASSWORD", "valueFrom": {"secretKeyRef": {"name": "s"}}},
                {"name": "APPRAFTER_BACKUP_REPO", "value": "s3:https://h/b/prod"}
            ]}]
        }}}}}});
        assert_eq!(cronjob_repo(&cj).as_deref(), Some("s3:https://h/b/prod"));
    }

    #[test]
    fn a_cronjob_with_no_repo_env_reads_as_unknown_not_as_a_match() {
        // `None` must not compare equal to the repo we are waiting for,
        // or `enable` would fire the first backup at whatever the old
        // CronJob pointed at — the exact mistake this check exists to
        // prevent.
        let empty = json!({"spec": {"jobTemplate": {"spec": {"template": {"spec": {
            "containers": [{"name": "runner"}]
        }}}}}});
        assert_eq!(cronjob_repo(&empty), None);
        assert_eq!(cronjob_repo(&json!({})), None);
    }

    #[test]
    fn a_finished_job_is_read_from_its_conditions() {
        let done = json!({"status": {"succeeded": 1, "conditions": [
            {"type": "Complete", "status": "True"}
        ]}});
        assert_eq!(job_run_outcome(&done), JobOutcome::Succeeded);

        let failed = json!({"status": {"failed": 1, "conditions": [
            {"type": "Failed", "status": "True", "reason": "BackoffLimitExceeded",
             "message": "Job has reached the specified backoff limit"}
        ]}});
        match job_run_outcome(&failed) {
            JobOutcome::Failed(why) => {
                assert!(why.contains("BackoffLimitExceeded"), "{why}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn a_job_that_has_not_finished_is_still_running() {
        // A condition list that carries something OTHER than the two
        // terminal types must not be read as an outcome — `Suspended`
        // and `FailureTarget` both appear on healthy Jobs, and a false
        // "succeeded" here would report a backup that never ran.
        for status in [
            json!({"status": {"active": 1}}),
            json!({"status": {}}),
            json!({}),
            json!({"status": {"conditions": [{"type": "Suspended", "status": "True"}]}}),
            json!({"status": {"conditions": [{"type": "Complete", "status": "False"}]}}),
        ] {
            assert_eq!(job_run_outcome(&status), JobOutcome::Running, "{status}");
        }
    }

    #[test]
    fn the_manual_job_name_is_a_legal_object_name() {
        let name = manual_job_name("20260910-215301");
        assert!(name.len() <= 63, "{name}");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{name}"
        );
        assert!(name.starts_with("apprafter-backup-"), "{name}");
    }

    #[test]
    fn list_follows_the_schedule_when_one_is_configured() {
        // The complaint this answers: after `backup enable` succeeded,
        // `backup list` still read the LOCAL repository and printed
        // nothing, which reads as "the backup I just configured did not
        // work" rather than "you are looking at a different repository".
        let on = json!({"enabled": true, "bucket": "s3:https://h/b/prod"});
        assert_eq!(
            choose_list_repo(None, false, Some(&on)),
            ListRepo::OffSite("s3:https://h/b/prod".to_string())
        );
    }

    #[test]
    fn list_stays_local_when_no_schedule_claims_the_cluster() {
        // Disabled, never configured, and no cluster at all: three ways
        // of having no off-site repository, one answer.
        for spec in [
            Some(json!({"enabled": false, "bucket": "s3:https://h/b"})),
            Some(json!({})),
            None,
        ] {
            assert_eq!(
                choose_list_repo(None, false, spec.as_ref()),
                ListRepo::Local,
                "{spec:?}"
            );
        }
    }

    #[test]
    fn an_explicit_repo_and_local_both_outrank_the_schedule() {
        let on = json!({"enabled": true, "bucket": "s3:https://h/b/prod"});
        assert_eq!(
            choose_list_repo(Some("s3:elsewhere"), false, Some(&on)),
            ListRepo::Explicit("s3:elsewhere".to_string())
        );
        assert_eq!(choose_list_repo(None, true, Some(&on)), ListRepo::Local);
    }

    #[test]
    fn credentials_the_cluster_already_holds_are_a_reason_to_reach_it() {
        // The credential Secret was sealed into the cluster by `backup
        // enable`. Asking the operator to hand the same credentials back
        // on every `check` is asking them to keep a copy of a secret the
        // platform is already holding — so an invocation with no local
        // credential source has a third reason to read the CR.
        let repo = Some("s3:https://h/b");
        assert!(backup_verb_needs_cluster(
            repo,
            RetentionArgs::NotApplicable,
            CredSource::Cluster,
            None
        ));
        // …and none when the operator DID supply them locally.
        assert!(!backup_verb_needs_cluster(
            repo,
            RetentionArgs::NotApplicable,
            CredSource::File,
            None
        ));
        assert!(!backup_verb_needs_cluster(
            repo,
            RetentionArgs::NotApplicable,
            CredSource::Env,
            None
        ));
    }

    #[test]
    fn the_offline_hint_asks_for_the_credential_file_too() {
        // The DR case this hint exists for — cluster gone, verify the
        // repo before restoring — now needs credentials as well as a
        // repo, and a hint that lists only `--repo` would leave the
        // operator one flag short of running offline.
        let h = cluster_need(
            None,
            RetentionArgs::NotApplicable,
            CredSource::Cluster,
            None,
        )
        .hint("check");
        assert!(h.contains("--repo"), "{h}");
        assert!(h.contains("--credential-file"), "names the creds flag: {h}");
    }

    #[test]
    fn a_local_credential_source_beats_the_cluster_one() {
        // Precedence, stated once: an explicit file wins over the
        // environment, and both win over the cluster. The cluster is the
        // fallback that makes the common case need no flags at all.
        assert_eq!(cred_source(true, true), CredSource::File);
        assert_eq!(cred_source(true, false), CredSource::File);
        assert_eq!(cred_source(false, true), CredSource::Env);
        assert_eq!(cred_source(false, false), CredSource::Cluster);
    }

    #[test]
    fn the_credential_secret_is_the_one_the_cr_names() {
        let spec = serde_json::json!({"credentialRef": {"name": "my-own-s3"}});
        assert_eq!(credential_secret_name(Some(&spec)), "my-own-s3");
        // No CR, or a CR without the ref: the platform default, which is
        // what `backup enable` seals when `--credential` is omitted.
        assert_eq!(credential_secret_name(None), DEFAULT_BACKUP_CREDENTIAL_NAME);
        let bare = serde_json::json!({"enabled": true});
        assert_eq!(
            credential_secret_name(Some(&bare)),
            DEFAULT_BACKUP_CREDENTIAL_NAME
        );
    }

    #[test]
    fn secret_held_credentials_are_normalised_like_a_dotenv() {
        // The Secret may hold either spelling — `enable --credential-file`
        // seals the canonical S3_* names, but an operator who sealed it by
        // hand may well have used restic's own AWS_* ones.
        let data: BTreeMap<String, Vec<u8>> = [
            ("AWS_ACCESS_KEY_ID", "AK"),
            ("AWS_SECRET_ACCESS_KEY", "SK"),
            ("RESTIC_PASSWORD", "pw"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
        .collect();
        let creds = creds_from_secret_bytes(data, "apprafter-backup-s3").unwrap();
        assert_eq!(creds["S3_ACCESS_KEY_ID"], "AK");
        assert_eq!(creds["S3_SECRET_ACCESS_KEY"], "SK");
        assert_eq!(creds["RESTIC_PASSWORD"], "pw");
    }

    #[test]
    fn a_secret_missing_the_passphrase_names_the_secret_and_the_key() {
        // Half a credential is the confusing case: restic would fail on
        // the passphrase prompt much later, pointing at nothing.
        let data: BTreeMap<String, Vec<u8>> = [("S3_ACCESS_KEY_ID", "AK")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect();
        let err = creds_from_secret_bytes(data, "apprafter-backup-s3")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("apprafter-backup-s3"),
            "names the Secret: {err}"
        );
        assert!(err.contains("RESTIC_PASSWORD"), "names the key: {err}");
    }

    #[test]
    fn offline_hint_for_check_points_at_repo_only() {
        let h =
            cluster_need(None, RetentionArgs::NotApplicable, CredSource::File, None).hint("check");
        assert!(h.contains("backup check"), "names the verb: {h}");
        assert!(h.contains("--repo"), "names --repo: {h}");
        assert!(
            !h.contains("--keep-"),
            "check has no retention inputs — must not mention --keep-*: {h}"
        );
    }

    /// The prune hint names the identity it is missing, the flag that supplies
    /// it, and what supplying it CLAIMS — an operator reaching for
    /// `--cluster-uid` is about to delete by explicit id in a repository that
    /// may hold someone else's runs, and the hint is where they learn that.
    #[test]
    fn offline_hint_for_prune_names_the_identity_and_what_claiming_it_means() {
        let h = cluster_need(
            Some("s3:https://h/b"),
            prune_keeps(Some(7), Some(4), Some(6)),
            CredSource::File,
            None,
        )
        .hint("prune");
        assert!(
            h.contains("kube-system"),
            "names the identity it needs: {h}"
        );
        assert!(h.contains("--cluster-uid"), "names the flag: {h}");
        assert!(
            h.contains("WHOSE snapshots may be forgotten"),
            "says what the flag claims, not just that it exists: {h}"
        );
        assert!(
            !h.contains("--keep-daily"),
            "those were supplied — must not ask for them again: {h}"
        );
    }

    /// …and once the identity IS supplied, the hint stops selling it. A hint
    /// listing `--cluster-uid` to an operator who already passed it would send
    /// them looking for a flag they are holding.
    #[test]
    fn the_prune_hint_drops_the_identity_once_cluster_uid_is_given() {
        let h = cluster_need(
            None,
            prune_keeps(Some(7), Some(4), Some(6)),
            CredSource::File,
            Some(OFFLINE_UID),
        )
        .hint("prune");
        assert!(h.contains("--repo"), "the bucket is still unresolved: {h}");
        assert!(!h.contains("--cluster-uid"), "{h}");
        assert!(!h.contains("kube-system"), "{h}");
    }

    /// check / unlock keep the offline path, and keep naming the DR case: the
    /// operator's cluster is SUPPOSED to be gone when they verify a repo.
    #[test]
    fn offline_hint_mentions_disaster_recovery_when_repo_missing() {
        let h =
            cluster_need(None, RetentionArgs::NotApplicable, CredSource::File, None).hint("check");
        assert!(h.contains("--repo"), "{h}");
        assert!(
            h.to_lowercase().contains("no longer exists")
                || h.to_lowercase().contains("disaster recovery"),
            "the DR case must be named — the operator's cluster is SUPPOSED to be gone: {h}"
        );
    }

    #[test]
    fn spec_backup_is_not_read_when_there_is_no_cluster() {
        // Passing `None` for the kubeconfig proves the offline path never
        // reaches for the cluster (a kubectl shell-out here would fail),
        // and that the caller can still honour an explicit --repo on top.
        let spec = spec_backup_from_cluster(None).expect("no cluster is not an error");
        assert!(spec.is_none());
        assert_eq!(
            repo_from_spec_backup(Some("s3:https://h/b"), spec.as_ref()).unwrap(),
            "s3:https://h/b"
        );
    }

    #[test]
    fn no_repo_and_no_cluster_points_the_reader_at_the_flag() {
        let err = repo_from_spec_backup(None, None).expect_err("no repo, no cluster → error");
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
            "prod",
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

    /// A fixed +09:00, so the zone conversion is asserted against a
    /// constant rather than against whatever zone the test host is in.
    fn tokyo() -> chrono::FixedOffset {
        chrono::FixedOffset::east_opt(9 * 3600).unwrap()
    }

    /// This cluster's `kube-system` UID, and a co-tenant's, for the
    /// attribution tests below.
    const MINE: &str = "11111111-2222-3333-4444-555555555555";
    const THEIRS: &str = "99999999-8888-7777-6666-555555555555";

    /// A view with no cluster to compare against — what the rendering tests
    /// (which are about columns, not attribution) ask for.
    fn anon_view() -> ClusterView<'static> {
        ClusterView {
            this: None,
            all: false,
        }
    }

    /// [`format_snapshot_table`] over owned values, so the rendering tests
    /// keep reading as a list of snapshots rather than a list of references.
    fn render_table(
        repo: &str,
        snaps: &[Value],
        tz: &chrono::FixedOffset,
        zone: Option<&str>,
    ) -> String {
        let refs: Vec<&Value> = snaps.iter().collect();
        format_snapshot_table(repo, &refs, tz, zone, anon_view())
    }

    #[test]
    fn a_snapshot_time_is_shown_in_the_readers_own_zone() {
        // What the operator saw: `2026-09-10T22:11:39.771675302Z` — UTC,
        // to the nanosecond, for a backup they took at 23:11 their time.
        // Every other time this CLI prints is in their zone (the schedule
        // especially), and one raw UTC timestamp in the middle of that
        // reads as a different backup than the one they just took.
        let table = render_table(
            "s3:x",
            &[json!({
                "short_id": "354fb34e",
                "time": "2026-09-10T22:11:39.771675302Z",
                "tags": ["platform-2026-09-10T22:11:34+00:00"]
            })],
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(table.contains("2026-09-11 07:11:39"), "{table}");
        assert!(!table.contains("771675302"), "{table}");
        assert!(
            table.contains("TIME (Asia/Tokyo)"),
            "the column says which zone it is in: {table}"
        );
    }

    #[test]
    fn the_columns_line_up_whatever_the_values_are() {
        // The reported symptom: the header reserved 25 columns for a
        // timestamp that renders 30 wide, so TAGS started in a different
        // place on every line.
        let table = render_table(
            "s3:x",
            &[
                json!({"short_id": "354fb34e", "time": "2026-09-10T22:11:39.771675302Z",
                       "tags": ["one"]}),
                json!({"short_id": "aa", "time": "not-a-timestamp", "tags": ["two"]}),
                json!({}),
            ],
            &tokyo(),
            None,
        );
        let tag_column: Vec<usize> = table
            .lines()
            .filter(|l| l.contains("TAGS") || l.contains("one") || l.contains("two"))
            .map(|l| {
                l.rfind("  ")
                    .map(|i| i + 2)
                    .expect("every row has a column gap")
            })
            .collect();
        assert!(
            tag_column.windows(2).all(|w| w[0] == w[1]),
            "TAGS must start at one column on every line: {table}"
        );
    }

    #[test]
    fn a_timestamp_restic_did_not_write_is_shown_verbatim() {
        // Conservative: a value this code cannot parse is still the only
        // information there is about that snapshot, and inventing a
        // formatted time for it would be worse than showing it raw.
        let table = render_table(
            "s3:x",
            &[json!({"short_id": "x", "time": "whenever"})],
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(table.contains("whenever"), "{table}");
    }

    #[test]
    fn an_unknown_zone_still_labels_the_column() {
        let table = render_table(
            "s3:x",
            &[json!({"short_id": "x", "time": "2026-09-10T22:11:39Z"})],
            &tokyo(),
            None,
        );
        assert!(table.contains("TIME (local)"), "{table}");
    }

    #[test]
    fn the_snapshot_table_truncates_a_full_id_when_restic_omits_short_id() {
        let full = "0123456789abcdef0123456789abcdef";
        let table = render_table(
            "s3:https://h/b",
            &[json!({"id": full, "time": "2026-08-01T03:00:00Z", "tags": ["a", "b"]})],
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(table.contains("01234567"), "{table}");
        assert!(
            !table.contains(full),
            "a 32-hex id in a 12-wide column wrecks the table: {table}"
        );
        assert!(table.contains("a, b"), "tags are joined: {table}");
        // +09:00 of 03:00Z is noon the same day.
        assert!(table.contains("2026-08-01 12:00:00"), "{table}");
    }

    #[test]
    fn the_snapshot_table_prefers_short_id_and_tolerates_a_bare_snapshot() {
        let table = render_table(
            "s3:x",
            &[
                json!({"short_id": "deadbeef", "id": "ffffffffffff"}),
                json!({}),
            ],
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(table.contains("deadbeef"), "{table}");
        assert!(!table.contains("ffffffff"), "short_id wins: {table}");
        // A snapshot with neither id nor time still renders a row rather than
        // aborting the listing.
        assert!(table.contains('?'), "{table}");
    }

    #[test]
    fn an_empty_repo_says_so_instead_of_printing_an_empty_table() {
        let table = render_table("s3:https://h/b", &[], &tokyo(), Some("Asia/Tokyo"));
        assert!(table.contains("No snapshots in s3:https://h/b"), "{table}");
        assert!(
            !table.contains("TAGS"),
            "a header with no rows reads as a broken listing: {table}"
        );
    }

    // ------------------------------------------------------------------
    // E2: `backup list` in a repository two clusters share
    // ------------------------------------------------------------------

    fn mine_snapshot(id: &str, host: &str) -> Value {
        json!({"short_id": id, "time": "2026-09-11T03:00:00Z", "hostname": host,
               "tags": [format!("{MINE}-2026-09-11T03:00:00Z")]})
    }

    fn their_snapshot(id: &str, host: &str) -> Value {
        json!({"short_id": id, "time": "2026-09-11T04:00:00Z", "hostname": host,
               "tags": [format!("{THEIRS}-2026-09-11T04:00:00Z")]})
    }

    fn legacy_snapshot(id: &str) -> Value {
        json!({"short_id": id, "time": "2026-09-10T03:00:00Z", "hostname": "apprafter-backup",
               "tags": ["platform-2026-09-10T03:00:00+00:00"]})
    }

    fn my_view() -> ClusterView<'static> {
        ClusterView {
            this: Some(MINE),
            all: false,
        }
    }

    /// The same reader, with `--all-clusters`.
    fn all_clusters_view() -> ClusterView<'static> {
        ClusterView {
            this: Some(MINE),
            all: true,
        }
    }

    /// FIRES: another cluster's snapshots are withheld and counted, and the
    /// legacy one stays in scope — which is exactly the stated rule.
    #[test]
    fn a_listing_shows_this_clusters_snapshots_and_withholds_a_co_tenants() {
        let snaps = vec![
            mine_snapshot("aaaa1111", "prod"),
            their_snapshot("bbbb2222", "staging"),
            legacy_snapshot("cccc3333"),
        ];
        let scope = narrow_to_cluster(&snaps, my_view());
        let shown: Vec<&str> = scope
            .shown
            .iter()
            .map(|s| s.pointer("/short_id").unwrap().as_str().unwrap())
            .collect();
        assert_eq!(shown, vec!["aaaa1111", "cccc3333"]);
        assert_eq!(scope.hidden, 1);
    }

    /// A listing answers "what is the latest?", so the latest goes first.
    /// restic hands them over oldest-first and every listing passed that
    /// order through, which put the most useful row furthest from the
    /// prompt — and off the screen on a repository with real history.
    #[test]
    fn a_listing_puts_the_newest_snapshot_first() {
        let at = |id: &str, t: &str| {
            json!({"short_id": id, "time": t, "hostname": "h",
                   "tags": [format!("{MINE}-{t}")]})
        };
        // Handed over oldest-first, the way restic emits them.
        let snaps = vec![
            at("old00000", "2026-09-01T03:00:00Z"),
            at("mid00000", "2026-09-05T03:00:00Z"),
            at("new00000", "2026-09-11T03:00:00Z"),
        ];
        let ids = |scope: &ListingScope| -> Vec<String> {
            scope
                .shown
                .iter()
                .map(|s| {
                    s.pointer("/short_id")
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect()
        };
        assert_eq!(
            ids(&narrow_to_cluster(&snaps, my_view())),
            vec!["new00000", "mid00000", "old00000"]
        );
        // The unnarrowed path is a separate early return and sorts too.
        assert_eq!(
            ids(&narrow_to_cluster(&snaps, all_clusters_view())),
            vec!["new00000", "mid00000", "old00000"]
        );
    }

    /// Offsets are compared as instants, not as text. `+02:00` sorts before
    /// `Z` lexically while being the later moment, so a string comparison
    /// puts the newer snapshot second.
    #[test]
    fn an_offset_timestamp_is_ordered_by_instant_not_by_spelling() {
        let at = |id: &str, t: &str| {
            json!({"short_id": id, "time": t, "hostname": "h",
                   "tags": [format!("{MINE}-x")]})
        };
        let snaps = vec![
            at("earlier0", "2026-09-11T02:00:00Z"),
            at("later000", "2026-09-11T03:00:00+00:20"),
        ];
        let scope = narrow_to_cluster(&snaps, my_view());
        let first = scope.shown[0]
            .pointer("/short_id")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(
            first, "later000",
            "02:40Z is later than 02:00Z; a lexical sort reads it as earlier"
        );
    }

    /// A snapshot whose time will not parse is still shown — it goes last
    /// because it cannot be placed, never dropped.
    #[test]
    fn an_unparseable_time_sorts_last_and_is_not_dropped() {
        let snaps = vec![
            json!({"short_id": "broken00", "time": "??", "tags": [format!("{MINE}-x")]}),
            json!({"short_id": "fine0000", "time": "2026-09-01T03:00:00Z",
                   "tags": [format!("{MINE}-x")]}),
        ];
        let scope = narrow_to_cluster(&snaps, my_view());
        assert_eq!(scope.shown.len(), 2, "a row was dropped");
        assert_eq!(
            scope.shown[1]
                .pointer("/short_id")
                .unwrap()
                .as_str()
                .unwrap(),
            "broken00"
        );
    }

    /// DOES NOT FIRE: `--all-clusters` shows everything and hides nothing.
    /// Paired with the test above so "narrowed" is proved to be a decision
    /// rather than a listing that always drops rows.
    #[test]
    fn all_clusters_shows_the_co_tenants_snapshots_too() {
        let snaps = vec![
            mine_snapshot("aaaa1111", "prod"),
            their_snapshot("bbbb2222", "staging"),
        ];
        let scope = narrow_to_cluster(
            &snaps,
            ClusterView {
                this: Some(MINE),
                all: true,
            },
        );
        assert_eq!(scope.shown.len(), 2);
        assert_eq!(scope.hidden, 0);
    }

    /// With no cluster to compare against nothing can be narrowed — and the
    /// listing must show everything rather than silently emptying itself,
    /// which is the case where a repository has to be read without its
    /// cluster (disaster recovery).
    #[test]
    fn without_a_cluster_every_snapshot_is_listed() {
        let snaps = vec![
            mine_snapshot("aaaa1111", "prod"),
            their_snapshot("bbbb2222", "staging"),
        ];
        let scope = narrow_to_cluster(&snaps, anon_view());
        assert_eq!(scope.shown.len(), 2);
        assert_eq!(scope.hidden, 0);
    }

    /// A legacy snapshot is being attributed to this cluster by ASSUMPTION,
    /// and the row has to say so — that is the whole difference between a
    /// stated widening and a silent one.
    #[test]
    fn the_cluster_column_marks_a_legacy_row_and_leaves_ours_plain() {
        assert_eq!(cluster_cell(&mine_snapshot("a", "prod"), my_view()), "prod");
        assert_eq!(
            cluster_cell(&legacy_snapshot("b"), my_view()),
            "apprafter-backup (legacy)"
        );
        assert_eq!(
            cluster_cell(&their_snapshot("c", "staging"), my_view()),
            "staging (other)"
        );
        // With nothing to compare against, no marker is claimed.
        assert_eq!(
            cluster_cell(&legacy_snapshot("b"), anon_view()),
            "apprafter-backup"
        );
    }

    #[test]
    fn the_footnotes_say_what_was_withheld_and_what_was_assumed() {
        let snaps = [mine_snapshot("a", "prod")];
        let scope = ListingScope {
            shown: snaps.iter().collect(),
            hidden: 3,
        };
        let notes = listing_footnotes(&scope, my_view(), true);
        let joined = notes.join("\n");
        assert!(
            joined.contains("3 snapshot(s) belong to another cluster"),
            "{joined}"
        );
        assert!(joined.contains("--all-clusters"), "{joined}");
        assert!(joined.contains("(legacy)"), "{joined}");

        // Nothing withheld, nothing assumed, a cluster present → silence.
        let quiet = listing_footnotes(
            &ListingScope {
                shown: snaps.iter().collect(),
                hidden: 0,
            },
            my_view(),
            false,
        );
        assert!(quiet.is_empty(), "{quiet:?}");
    }

    /// FIRES: `--all-clusters` names the identities the repository holds, in
    /// full. This is the one place the whole UID is printed — the TAGS column
    /// abbreviates it to eight characters — and it is where an operator reads
    /// off the one `backup prune --cluster-uid` needs after a cluster is gone.
    #[test]
    fn all_clusters_names_the_identities_the_repository_holds() {
        let snaps = [
            mine_snapshot("a", "prod"),
            their_snapshot("c", "staging"),
            legacy_snapshot("b"),
        ];
        let scope = ListingScope {
            shown: snaps.iter().collect(),
            hidden: 0,
        };
        let joined = listing_footnotes(&scope, all_clusters_view(), true).join("\n");
        assert!(
            joined.contains(MINE),
            "the whole UID, not the short tag: {joined}"
        );
        assert!(joined.contains(THEIRS), "{joined}");

        // DOES NOT FIRE on the narrowed default: that listing is this
        // cluster's by construction, so naming identities would be noise.
        let quiet = listing_footnotes(&scope, my_view(), false).join("\n");
        assert!(!quiet.contains("cluster identities"), "{quiet}");
    }

    #[test]
    fn a_tag_is_rendered_with_its_cluster_uid_abbreviated() {
        let tag = format!("{MINE}-2026-09-11T03:00:00Z");
        let short = short_tag(&tag, &chrono::Utc);
        assert!(short.starts_with("11111111…"), "{short}");
        // Both halves are rendered now: the UID is abbreviated AND the stamp
        // reads the way the TIME column on the same row reads.
        assert!(short.ends_with("-2026-09-11 03:00:00"), "{short}");
        assert!(
            !short.contains("T03:00:00Z"),
            "the machine stamp survived into the column: {short}"
        );
        // A legacy tag has no UID to abbreviate; its stamp is still rendered.
        assert_eq!(
            short_tag("platform-2026-09-10T03:00:00Z", &chrono::Utc),
            "platform-2026-09-10 03:00:00"
        );
    }

    #[test]
    fn a_tag_stamp_is_shown_in_the_readers_zone_like_the_time_column() {
        // One row must not report one moment in two zones.
        let tz = chrono::FixedOffset::east_opt(2 * 3600).unwrap();
        assert_eq!(
            short_tag("platform-2026-09-10T03:00:00Z", &tz),
            format!("platform-{}", format_timestamp("2026-09-10T03:00:00Z", &tz))
        );
    }

    #[test]
    fn a_tag_without_a_timestamp_is_left_alone() {
        assert_eq!(short_tag("platform", &chrono::Utc), "platform");
        assert_eq!(
            short_tag("some-hyphenated-tag", &chrono::Utc),
            "some-hyphenated-tag"
        );
        assert_eq!(short_tag("", &chrono::Utc), "");
    }

    #[test]
    fn an_unparseable_stamp_in_a_tag_survives_verbatim() {
        // Losing it would delete the only record of whatever wrote the tag.
        assert_eq!(
            short_tag("platform-not-a-date", &chrono::Utc),
            "platform-not-a-date"
        );
    }

    // ------------------------------------------------------------------
    // The human cluster label
    // ------------------------------------------------------------------

    #[test]
    fn the_cluster_name_defaults_to_the_target_name_and_an_explicit_one_wins() {
        assert_eq!(resolve_cluster_name(None, "prod").unwrap(), "prod");
        assert_eq!(resolve_cluster_name(Some("eu-1"), "prod").unwrap(), "eu-1");
        // Surrounding whitespace is a typo, not a name.
        assert_eq!(
            resolve_cluster_name(Some("  eu-1 "), "prod").unwrap(),
            "eu-1"
        );
    }

    #[test]
    fn a_cluster_name_that_would_wreck_a_listing_is_refused() {
        // It becomes the restic `--host`; a space or a comma there makes a
        // listing unreadable and a `--host` filter unusable.
        assert!(resolve_cluster_name(Some("eu west"), "prod").is_err());
        assert!(resolve_cluster_name(Some("a,b"), "prod").is_err());
        assert!(resolve_cluster_name(Some("   "), "prod").is_err());
        assert!(resolve_cluster_name(None, "").is_err());
    }

    #[test]
    fn the_enable_patch_carries_the_cluster_name() {
        let p = backup_enable_patch(
            &EnableOpts {
                bucket: "s3:x".into(),
                credential: "c".into(),
                cluster_name: Some("prod".into()),
                ..Default::default()
            },
            &ResolvedSchedule {
                schedule: "0 3 * * *".into(),
                check_schedule: "".into(),
                time_zone: "UTC".into(),
            },
        );
        assert_eq!(p["spec"]["backup"]["clusterName"], json!("prod"));
    }

    #[test]
    fn set_cluster_name_patches_only_that_field() {
        let p = backup_set_patch("cluster-name", "eu-1").unwrap();
        assert_eq!(p["spec"]["backup"]["clusterName"], json!("eu-1"));
        assert_eq!(
            p["spec"]["backup"].as_object().unwrap().len(),
            1,
            "a single-field edit must not rewrite the block: {p}"
        );
        assert!(backup_set_patch("cluster-name", "eu west").is_err());
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

    /// The cluster the prune resolved: its configured repository and UID.
    const CLUSTER_REPO: &str = "s3:https://fsn1.example/bk/cluster";
    const OTHER_UID: &str = "22222222-3333-4444-5555-666666666666";

    /// Stamped: the cluster's own repository, pruned as the cluster's own
    /// history — with or without `--repo` / `--cluster-uid` naming them.
    #[test]
    fn the_last_prune_stamp_goes_on_the_cluster_whose_history_was_pruned() {
        for repo in [CLUSTER_REPO, "s3:https://fsn1.example/bk/cluster/"] {
            assert_eq!(
                last_prune_stamp(true, repo, Some(CLUSTER_REPO), OFFLINE_UID, Ok(OFFLINE_UID)),
                Ok(())
            );
        }
    }

    /// FIRES (live walk): `backup prune --repo <a rehearsal repository>
    /// --cluster-uid <another cluster>` with no `--keep-*` flags resolves the
    /// ACTIVE cluster only to read its retention — and then stamped
    /// `apprafter.io/last-prune` on it, so its `BackupRetention` said
    /// `apprafter backup prune` last ran against it when nothing of it was
    /// touched. Neither a different repository nor a different identity is
    /// this cluster's prune.
    #[test]
    fn a_prune_of_another_repository_or_identity_stamps_nothing() {
        let other_repo = last_prune_stamp(
            true,
            "/srv/rehearsal-repo",
            Some(CLUSTER_REPO),
            OFFLINE_UID,
            Ok(OFFLINE_UID),
        )
        .unwrap_err();
        assert!(other_repo.contains("/srv/rehearsal-repo"), "{other_repo}");
        assert!(other_repo.contains(CLUSTER_REPO), "{other_repo}");

        let other_uid = last_prune_stamp(
            true,
            CLUSTER_REPO,
            Some(CLUSTER_REPO),
            OTHER_UID,
            Ok(OFFLINE_UID),
        )
        .unwrap_err();
        assert!(other_uid.contains(OTHER_UID), "{other_uid}");
        assert!(other_uid.contains(OFFLINE_UID), "{other_uid}");

        let unconfigured =
            last_prune_stamp(true, CLUSTER_REPO, None, OFFLINE_UID, Ok(OFFLINE_UID)).unwrap_err();
        assert!(
            unconfigured.contains("no backup repository"),
            "{unconfigured}"
        );

        let unreadable = last_prune_stamp(
            true,
            CLUSTER_REPO,
            Some(CLUSTER_REPO),
            OTHER_UID,
            Err("namespaces \"kube-system\" is forbidden"),
        )
        .unwrap_err();
        assert!(unreadable.contains("forbidden"), "{unreadable}");

        let offline =
            last_prune_stamp(false, CLUSTER_REPO, None, OFFLINE_UID, Ok(OFFLINE_UID)).unwrap_err();
        assert!(offline.contains("no cluster"), "{offline}");
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
            &backup_core::prune::PruneOutcome::Pruned {
                forgot_snapshots: 4,
                forgot_runs: 3,
                kept_runs: 6,
                unfinished_runs: 1,
            },
        );
        assert!(s.contains("s3:https://h/b"), "{s}");
        assert!(
            s.contains("keepDaily=1 keepWeekly=2 keepMonthly=3"),
            "each number must sit against its own label: {s}"
        );
        assert!(s.contains("forgot 4 snapshot(s) of 3 run(s)"), "{s}");
        assert!(s.contains("6 run(s) kept"), "{s}");
        assert!(
            s.contains("1 unfinished run(s) left alone, as a backup may still be writing them"),
            "{s}"
        );
    }

    /// The cluster's own scoped key is what `backup prune` falls back to with
    /// no credential file, and it may not delete. That is an error, which
    /// says nothing was deleted and names the flag that fixes it — not a
    /// "✓ Pruned".
    #[test]
    fn a_prune_the_key_may_not_run_is_an_error_that_names_the_full_credentials() {
        let outcome = backup_core::prune::PruneOutcome::NotPermitted {
            snapshot: "ecd0be3219c6a9adb39e".into(),
            restic_said: "Remove(<snapshot/ecd0be3219>) failed: client.RemoveObject: Access \
                          Denied."
                .into(),
            would_forget_snapshots: 9,
            would_forget_runs: 9,
        };
        // Only NotPermitted stops the command before the stamp.
        assert!(refuse_an_unenforced_prune(
            "s3:x",
            &backup_core::prune::PruneOutcome::NothingToPrune {
                kept_runs: 2,
                unfinished_runs: 0
            },
            false
        )
        .is_ok());
        assert!(refuse_an_unenforced_prune("s3:x", &outcome, false).is_err());
        for had_file in [false, true] {
            let msg = prune_not_permitted_error("s3:https://h/b", &outcome, had_file).to_string();
            assert!(
                msg.contains("retention was not enforced on s3:https://h/b"),
                "{msg}"
            );
            assert!(msg.contains("nothing was deleted"), "{msg}");
            assert!(msg.contains("Access Denied"), "{msg}");
            assert!(
                msg.contains("--credential-file <full-credentials.env>"),
                "{msg}"
            );
            assert_eq!(
                msg.contains("cluster's own backup Secret"),
                !had_file,
                "{msg}"
            );
        }
    }

    /// The `ownerReferences` the Job controller — or `kubectl create job
    /// --from=cronjob/<cronjob>` — puts on a Job it makes from `cronjob`.
    fn owned_by(cronjob: &str) -> Value {
        json!([{
            "apiVersion": "batch/v1", "kind": "CronJob", "name": cronjob,
            "uid": format!("{cronjob}-uid"), "controller": true, "blockOwnerDeletion": true
        }])
    }

    #[test]
    fn status_reports_only_apprafter_backup_jobs() {
        let list = json!({"items": [
            {"metadata": {"name": "apprafter-backup-1", "ownerReferences": owned_by(BACKUP_CRONJOB_NAME)}},
            {"metadata": {"name": "apprafter-backup-check-1",
                          "ownerReferences": owned_by(CHECK_CRONJOB_NAME)}},
            // `kubectl create job --from=cronjob/apprafter-backup <any-name>`.
            {"metadata": {"name": "walk-from-014903", "ownerReferences": owned_by(BACKUP_CRONJOB_NAME)}},
            {"metadata": {"name": "apprafter-backup-manual-20260923-030000",
                          "labels": {"apprafter.io/manual": "true"}}},
            {"metadata": {"name": "some-other-job"}},
            // The name alone makes nothing a backup: a lookalike with no
            // owner and no mark, and another CronJob's Job.
            {"metadata": {"name": "apprafter-backup-lookalike"}},
            {"metadata": {"name": "apprafter-backup-7", "ownerReferences": owned_by("nightly-report")}},
        ]});
        let jobs = backup_jobs_of(Some(&list));
        let names: Vec<&str> = jobs.iter().map(job_metadata_name).collect();
        assert_eq!(
            names,
            vec![
                "apprafter-backup-1",
                "apprafter-backup-check-1",
                "walk-from-014903",
                "apprafter-backup-manual-20260923-030000"
            ]
        );
        // No Jobs listing at all (or no items) is "none", not a failure.
        assert!(backup_jobs_of(None).is_empty());
        assert!(backup_jobs_of(Some(&json!({}))).is_empty());
    }

    /// Which runner a Job is, by the operator's rule (`Run::owns`): the
    /// CronJob that owns it, else the name and label of `backup run`'s own.
    #[test]
    fn a_runner_job_is_told_by_its_owner_not_its_name() {
        let job = |meta: Value| json!({ "metadata": meta });
        let cases = [
            (
                json!({"name": "x", "ownerReferences": owned_by(BACKUP_CRONJOB_NAME)}),
                Some(RunnerJob::Backup),
            ),
            (
                json!({"name": "apprafter-backup-y", "ownerReferences": owned_by(CHECK_CRONJOB_NAME)}),
                Some(RunnerJob::Check),
            ),
            (
                json!({"name": "apprafter-backup-check-z", "ownerReferences": owned_by("other")}),
                None,
            ),
            (
                json!({"name": "apprafter-backup-manual-1", "labels": {"apprafter.io/manual": "true"}}),
                Some(RunnerJob::Backup),
            ),
            // The mark without the name, and the name without the mark.
            (
                json!({"name": "mine", "labels": {"apprafter.io/manual": "true"}}),
                None,
            ),
            (json!({"name": "apprafter-backup-manual-2"}), None),
            (json!({"name": "apprafter-backup-check-3"}), None),
        ];
        for (meta, want) in cases {
            assert_eq!(runner_job(&job(meta.clone())), want, "{meta}");
        }
    }

    /// FIRES (P4 of the live walk): a scheduled-style Job made the usual
    /// Kubernetes way under another name. The operator counted it; `backup
    /// status` printed "Last backup Job: none" beside it.
    #[test]
    fn status_shows_a_job_made_from_the_cronjob_under_any_name() {
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let job = json!({
            "metadata": {"name": "walk-from-014903", "ownerReferences": owned_by(BACKUP_CRONJOB_NAME)},
            "status": {"startTime": "2026-09-23T01:49:03Z", "succeeded": 1}
        });
        let s = format_backup_status(
            Some(&spec),
            std::slice::from_ref(&job),
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(
            s.contains("Last backup Job: walk-from-014903 — Succeeded"),
            "{s}"
        );
        assert!(s.contains("Last check Job:  none"), "{s}");
    }

    /// FIRES: `backup run` beside such a Job would have started a second
    /// runner. It refuses, naming the Job.
    #[test]
    fn backup_run_refuses_beside_a_job_made_from_the_cronjob_under_any_name() {
        let mut job = unfinished_job("walk-from-014903", "job-1", Some("CronJob"));
        job["metadata"]["ownerReferences"] = owned_by(BACKUP_CRONJOB_NAME);
        let (report, err) = refusal(&[job], &[]).expect("a runner that has not finished");
        assert!(
            report.contains("walk-from-014903 has not finished"),
            "{report}"
        );
        assert!(
            matches!(err, CliError::BackupJobActive { ref job } if job == "walk-from-014903"),
            "{err:?}"
        );
    }

    #[test]
    fn a_job_that_has_started_but_not_finished_is_neither_succeeded_nor_failed() {
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let running = json!({
            "metadata": {"name": "apprafter-backup-running", "ownerReferences": owned_by(BACKUP_CRONJOB_NAME)},
            "status": {"active": 1}
        });
        let s = format_backup_status(
            Some(&spec),
            std::slice::from_ref(&running),
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("Running"), "{s}");

        // A Job with no counters at all must not be reported as a success.
        let bare = json!({
            "metadata": {"name": "apprafter-backup-bare", "ownerReferences": owned_by(BACKUP_CRONJOB_NAME)},
            "status": {}
        });
        let s = format_backup_status(
            Some(&spec),
            std::slice::from_ref(&bare),
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
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
            &[],
        );
        assert!(s.contains("2 namespace(s)"), "{s}");
        assert!(s.contains("demo, prod"), "{s}");
        assert!(s.contains("/srv/dump"), "{s}");
        // "5 claims, 2 of them extractable" — swapping these tells the
        // operator more data was captured than was.
        assert!(s.contains("5 (2 extractable)"), "{s}");
    }

    /// A cluster whose backup deadline is `deadline` seconds, recording
    /// every helper pod applied in it — enough of one for the paths that
    /// size and build an interactive command's helpers.
    struct SizingKube {
        deadline: u64,
        applied: std::sync::Mutex<Vec<Value>>,
    }

    impl SizingKube {
        fn with_deadline(deadline: u64) -> Self {
            Self {
                deadline,
                applied: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn keep_alives(&self) -> Vec<Value> {
            self.applied
                .lock()
                .unwrap()
                .iter()
                .map(|spec| spec["spec"]["containers"][0]["command"].clone())
                .collect()
        }
    }

    impl KubeExec for SizingKube {
        fn apply_and_wait_pod_ready(&self, spec: &Value) -> Result<()> {
            self.applied.lock().unwrap().push(spec.clone());
            Ok(())
        }
        fn exec_stream_to_file(
            &self,
            _: &str,
            _: &str,
            _: &[&str],
            out: &Path,
            _: Option<Duration>,
        ) -> Result<()> {
            std::fs::write(out, b"DUMP").unwrap();
            Ok(())
        }
        fn exec_stream_from_file(&self, _: &str, _: &str, _: &[&str], _: &Path) -> Result<()> {
            unreachable!("nothing is loaded")
        }
        fn delete_pod_best_effort(&self, _: &str, _: &str) {}
        fn get_secret_key(&self, _: &str, _: &str, key: &str) -> Result<String> {
            Ok(match key {
                "host" => "platform.nats.svc".to_string(),
                "port" => "4222".to_string(),
                other => format!("{other}-value"),
            })
        }
        fn get_json(&self, args: &[&str]) -> Result<Option<Value>> {
            assert_eq!(
                args,
                [
                    "get",
                    "platformstack",
                    "default",
                    "-n",
                    "apprafter-system",
                    "-o",
                    "json"
                ],
                "the only read these paths make"
            );
            Ok(Some(json!({
                "spec": {"backup": {"activeDeadlineSeconds": self.deadline}}
            })))
        }
    }

    /// `backup create`'s options, from a cluster with the given deadline.
    fn local_pull_opts_under(k: &SizingKube) -> BackupOpts {
        local_pull_backup_opts(
            k,
            "s3:https://h/b",
            "pw".into(),
            "prod-cluster",
            MINE,
            "0.2.58",
            &["prod".to_string()],
            true,
            Path::new("/staging"),
            "postgres:18-alpine".into(),
            StagingMode::Sequential,
        )
        .unwrap()
    }

    /// The interactive commands have no Job deadline, so their helpers'
    /// keep-alive is the only limit on one dump. A cluster backing up every
    /// fifteen minutes sets a ten-minute deadline, and a helper sized by it
    /// killed a long load at ten minutes; each path's helpers live six hours
    /// all the same, and longer when the deadline is. `backup create` hands
    /// the engine what [`local_pull_backup_opts`] read (the engine's own test
    /// holds every helper to it); `export` applies them itself.
    #[test]
    fn an_interactive_backup_and_export_size_their_helpers_past_a_short_schedule_deadline() {
        for (deadline, want) in [(600, "21600"), (43200, "43200")] {
            let k = SizingKube::with_deadline(deadline);
            assert_eq!(
                local_pull_opts_under(&k).helper_keep_alive,
                Duration::from_secs(want.parse().unwrap()),
                "backup create under a deadline of {deadline}s"
            );

            let k = SizingKube::with_deadline(deadline);
            let plan = plan_extraction(&[
                json!({"spec": {"type": "pg"}, "metadata": {"name": "db", "namespace": "shop"},
                       "status": {"connectionSecretRef": "db-conn"}}),
                json!({"spec": {"type": "disk"}, "metadata": {"name": "files", "namespace": "shop"},
                       "status": {"volumeClaimRef": "pvc"}}),
                json!({"spec": {"type": "jetstream"}, "metadata": {"name": "js", "namespace": "shop"},
                       "status": {"connectionSecretRef": "js-conn",
                                  "streams": {"declared": ["orders"]}}}),
            ]);
            let dir = tempfile::tempdir().unwrap();
            export_extract(&k, &plan, dir.path(), "postgres:18-alpine").unwrap();
            assert_eq!(
                k.keep_alives(),
                vec![json!(["sleep", want]); 3],
                "export under a deadline of {deadline}s"
            );
        }
    }

    #[test]
    fn the_local_pull_keeps_the_operator_stations_hostname_as_the_restic_group() {
        // spec §Retention M-r3-1a: only the in-cluster runner pins a fixed
        // host (its pod name is ephemeral). Pinning it here would merge every
        // operator's snapshots into one retention group.
        let opts = local_pull_backup_opts(
            &SizingKube::with_deadline(43200),
            "s3:https://h/b",
            "pw".into(),
            "prod-cluster",
            MINE,
            "0.2.58",
            &["prod".to_string()],
            true,
            Path::new("/staging"),
            "postgres:18-alpine".into(),
            StagingMode::Sequential,
        )
        .unwrap();
        assert_eq!(opts.backup_host, None);
        assert!(opts.is_subset, "--select must reach the tag decoration");
        assert_eq!(opts.repo, "s3:https://h/b");
        assert_eq!(opts.cluster_id, "prod-cluster");
        // …and the local pull carries the SAME machine key the scheduled
        // runner writes, so a `--repo s3:…` pull into a shared repository is
        // attributable rather than landing as an unidentified snapshot (E1).
        assert_eq!(opts.cluster_uid, MINE);
        assert_eq!(opts.platform_version, "0.2.58");
        assert_eq!(opts.namespaces, vec!["prod".to_string()]);
        assert_eq!(opts.staging_root, PathBuf::from("/staging"));
        assert_eq!(opts.pg_image, "postgres:18-alpine");
        // The cluster's run deadline, not a fixed hour: it is how long each
        // helper pod — and so each extraction — may live.
        assert_eq!(opts.helper_keep_alive, Duration::from_secs(43200));
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
            cert_count: 0,
            claim_count: 5,
            extracted_count: 2,
            uncaptured_claims: Vec::new(),
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

    /// A1: the imported certificate is named in the run summary when there is
    /// one — it is the object an operator can check by eye against their
    /// `target domain list`, and the one whose absence used to surface only as
    /// a Gateway pointing at nothing after a restore. Silent on the clusters
    /// that never connected a domain.
    #[test]
    fn the_backup_summary_names_the_imported_certificate_only_when_one_was_captured() {
        let mut summary = backup_core::engine::BackupSummary {
            snapshot_id: Some("abc123".into()),
            cr_count: 3,
            secret_count: 4,
            cert_count: 1,
            claim_count: 5,
            extracted_count: 2,
            uncaptured_claims: Vec::new(),
            tag: "t".into(),
        };
        let with = backup_summary_report("prod", "s3:https://h/b", &["prod".into()], &summary);
        assert!(
            with.contains("4 secret(s), 1 imported cert(s), 5 claim(s)"),
            "{with}"
        );

        summary.cert_count = 0;
        let without = backup_summary_report("prod", "s3:https://h/b", &["prod".into()], &summary);
        assert!(
            without.contains("4 secret(s), 5 claim(s)"),
            "no certificate, no clause: {without}"
        );
    }

    // =======================================================================
    // A6 — claims captured as configuration only
    // =======================================================================

    fn jetstream_claim(ns: &str, name: &str) -> UncapturedClaim {
        UncapturedClaim {
            namespace: ns.into(),
            name: name.into(),
            claim_type: "jetstream".into(),
        }
    }

    /// FIRES: the run summary names the type, the count and the claims, and
    /// says what a restore of them produces. Without this the operator's only
    /// evidence is a claim count that looks right.
    #[test]
    fn the_backup_summary_names_the_claims_it_captured_as_configuration_only() {
        let summary = backup_core::engine::BackupSummary {
            snapshot_id: Some("abc123".into()),
            cr_count: 3,
            secret_count: 4,
            cert_count: 0,
            claim_count: 5,
            extracted_count: 2,
            uncaptured_claims: vec![
                jetstream_claim("demo", "events"),
                jetstream_claim("demo", "audit"),
            ],
            tag: "t".into(),
        };
        let s = backup_summary_report("prod", "s3:https://h/b", &["demo".into()], &summary);
        assert!(s.contains("jetstream: 2 claim(s)"), "{s}");
        assert!(s.contains("demo/audit, demo/events"), "sorted + named: {s}");
        assert!(s.contains("brings them back empty"), "{s}");
        // …and says nothing about capture arriving later: it is separate work
        // that is not underway, and a hint here invites someone to wait.
        for tease in ["yet", "coming", "future", "soon", "will be"] {
            assert!(
                !s.contains(tease),
                "must not promise capture ({tease}): {s}"
            );
        }
    }

    /// DOES NOT FIRE on the clusters that declare none — which is most of
    /// them. A line printed on every backup is a line nobody reads.
    #[test]
    fn the_backup_summary_is_silent_when_every_claim_was_captured() {
        let summary = backup_core::engine::BackupSummary {
            snapshot_id: Some("abc123".into()),
            cr_count: 3,
            secret_count: 4,
            cert_count: 0,
            claim_count: 2,
            extracted_count: 2,
            uncaptured_claims: Vec::new(),
            tag: "t".into(),
        };
        let s = backup_summary_report("prod", "s3:https://h/b", &["demo".into()], &summary);
        assert!(!s.contains("configuration only"), "{s}");
        assert!(!s.contains("jetstream"), "{s}");
    }

    /// `export` has the same gap and gets the same line: it plans its
    /// extraction with the same function and writes the same manifest.
    #[test]
    fn the_export_summary_says_it_too() {
        let s = export_summary(
            "prod",
            Path::new("/srv/dump"),
            &["demo".to_string()],
            3,
            2,
            &[jetstream_claim("demo", "events")],
        );
        assert!(s.contains("jetstream: 1 claim(s)"), "{s}");
        assert!(s.contains("demo/events"), "{s}");
        assert!(
            s.contains("in this export"),
            "names the artifact it is: {s}"
        );
    }

    /// FIRES: `backup show` reads the marker out of the manifest, so an
    /// operator deciding what to restore learns it from the snapshot itself.
    #[test]
    fn backup_show_says_which_listed_claims_carry_no_data() {
        let manifest = json!({
            "clusterId": "prod", "platformVersion": "0.2.65",
            "namespaces": ["demo"],
            "resources": [
                {"namespace": "demo", "kind": "ResourceClaim", "name": "events",
                 "claim_type": "jetstream", "no_data": true},
                {"namespace": "demo", "kind": "ResourceClaim", "name": "db",
                 "claim_type": "pg"},
            ]
        });
        let out =
            format_snapshot_contents("abc123", None, None, &manifest, Some(0), &chrono::Utc, None);
        // The listing still says the claim is in the snapshot …
        assert!(
            out.contains("ResourceClaim  2  (jetstream 1, pg 1)"),
            "{out}"
        );
        // … and the qualifier says what it is NOT.
        assert!(out.contains("jetstream: 1 claim(s)"), "{out}");
        assert!(out.contains("demo/events"), "{out}");
        assert!(out.contains("in this snapshot"), "{out}");
        assert!(
            !out.contains("demo/db"),
            "the pg claim is not qualified: {out}"
        );
    }

    /// A snapshot written before the marker existed still gets an honest
    /// answer, from what this build knows about a snapshot of THAT format
    /// version. The alternative is a pre-marker snapshot reading as complete
    /// forever — and after 2.6d-6 that is no longer hypothetical: jetstream
    /// data is captured now, so without the version every older snapshot would
    /// have gone quiet about exactly the claims A6 was about.
    #[test]
    fn backup_show_falls_back_to_the_type_when_the_manifest_predates_the_marker() {
        let manifest = json!({
            "clusterId": "prod", "platformVersion": "0.2.64",
            "resources": [
                {"namespace": "demo", "kind": "ResourceClaim", "name": "events",
                 "claim_type": "jetstream"},
            ]
        });
        let out =
            format_snapshot_contents("abc123", None, None, &manifest, Some(0), &chrono::Utc, None);
        assert!(out.contains("jetstream: 1 claim(s)"), "{out}");
    }

    /// …and the other direction: in a snapshot whose FORMAT can hold jetstream
    /// data, an unmarked jetstream claim is one whose data is there. A warning
    /// here would send an operator looking for data they already have.
    ///
    /// The pair is the whole point of versioning the fallback. After 2.6d-6
    /// the claim type alone answers nothing — only the manifest version
    /// distinguishes "captured, and this claim had no streams" from "this
    /// format could not capture it".
    #[test]
    fn backup_show_is_silent_about_a_jetstream_claim_in_a_capture_era_snapshot() {
        let manifest = json!({
            "manifestVersion": backup_core::manifest::MANIFEST_VERSION_CURRENT,
            "clusterId": "prod", "platformVersion": "0.2.75",
            "resources": [
                {"namespace": "demo", "kind": "ResourceClaim", "name": "events",
                 "claim_type": "jetstream"},
            ]
        });
        let out =
            format_snapshot_contents("abc123", None, None, &manifest, Some(0), &chrono::Utc, None);
        assert!(!out.contains("configuration only"), "{out}");
        assert!(!out.contains("brings them back empty"), "{out}");
    }

    /// DOES NOT FIRE for a snapshot whose claims all carry data.
    #[test]
    fn backup_show_is_silent_when_nothing_is_missing() {
        let manifest = json!({
            "clusterId": "prod", "platformVersion": "0.2.65",
            "resources": [
                {"namespace": "demo", "kind": "ResourceClaim", "name": "db",
                 "claim_type": "pg"},
                {"namespace": "demo", "kind": "Application", "name": "shop"},
            ]
        });
        let out =
            format_snapshot_contents("abc123", None, None, &manifest, Some(0), &chrono::Utc, None);
        assert!(!out.contains("configuration only"), "{out}");
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
        assert!(kubeconfig_if_cluster_needed(
            "check",
            Some("s3:x"),
            RetentionArgs::NotApplicable,
            CredSource::File,
            None
        )
        .unwrap()
        .is_none());
        // …but NOT prune (E3): it deletes, so it must know whose snapshots it
        // may delete, and only the cluster can say. Asserted as "not Ok(None)"
        // rather than a concrete value, because whether the kubeconfig then
        // RESOLVES depends on the machine — the point is that it was reached
        // for at all. `Ok(None)` would be an offline prune planning across a
        // shared bucket.
        assert!(
            !matches!(
                kubeconfig_if_cluster_needed(
                    "prune",
                    Some("s3:x"),
                    prune_keeps(Some(7), Some(4), Some(6)),
                    CredSource::File,
                    None
                ),
                Ok(None)
            ),
            "prune must never take the offline shortcut"
        );
    }

    // The mirror case — a verb with NO local credentials must not take
    // the offline shortcut — is asserted on `backup_verb_needs_cluster`
    // and `cluster_need` above, not here. Asserting it through
    // `kubeconfig_if_cluster_needed` would depend on whether the machine
    // running the tests happens to have a target configured: on a
    // developer's laptop the kubeconfig resolves and the call succeeds,
    // on CI it does not. A guard that reads the environment instead of
    // the code is worse than no guard.

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

    /// The diagnostic help attached to an error, or a marker when it has none.
    fn help_of(err: &CliError) -> String {
        use miette::Diagnostic;
        err.help()
            .map(|h| format!("{h}"))
            .unwrap_or_else(|| "<no help>".to_string())
    }

    /// The diagnostic code attached to an error.
    fn code_of(err: &CliError) -> String {
        use miette::Diagnostic;
        err.code()
            .map(|c| format!("{c}"))
            .unwrap_or_else(|| "<no code>".to_string())
    }

    #[test]
    fn a_failure_saving_config_reports_the_key_the_run_left_behind() {
        // Verbatim from the report. restic names the object it was
        // saving, and `config` is written AFTER the master key — so this
        // stderr is proof that a key file is now sitting in the bucket,
        // which is what wedges the next attempt. The CLI knows this
        // without asking the store anything.
        let e = repo_unreachable_error(
            "s3:https://h/b",
            b"Fatal: repository does not exist: unable to open config file: Stat: \
              The specified key does not exist.\n",
            b"Save(<config/0000000000>) failed: client.PutObject: Access Denied.\n\
              Fatal: create key in repository at s3:https://h/b failed: \
              client.PutObject: Access Denied.\n",
        );
        let help = help_of(&e);
        assert!(
            help.contains("left a key file behind"),
            "states the leftover as fact: {help}"
        );
    }

    #[test]
    fn a_failure_saving_the_key_itself_says_nothing_was_left() {
        // Captured from a real `restic 0.18.1 init` against a repository
        // whose key directory refused the write: the run failed on the
        // FIRST object it writes, and the location is untouched. Telling
        // the reader to go delete keys here would send them hunting for
        // something that is not there.
        let e = repo_unreachable_error(
            "/repo",
            b"Fatal: unable to open config file: <config/> does not exist\n",
            b"Save(<key/d82a996d80>) failed: open /repo/keys/d82a996d80-tmp-2303320433: \
              permission denied\nFatal: create key in repository at /repo failed: \
              open /repo/keys/d82a996d80-tmp-2303320433: permission denied\n",
        );
        let help = help_of(&e);
        assert!(
            help.contains("Nothing was left behind"),
            "rules the leftover out: {help}"
        );
    }

    #[test]
    fn a_stderr_that_names_no_object_claims_nothing_either_way() {
        // The conservative default. "already contains keys" says which
        // check refused, not which object was being written, so there is
        // nothing to conclude about what this run did or did not leave.
        let e = repo_unreachable_error(
            "s3:https://h/b",
            b"Fatal: repository does not exist\n",
            b"Fatal: create key in repository at s3:https://h/b failed: repository \
              already contains keys\n",
        );
        let help = help_of(&e);
        assert!(!help.contains("left a key file behind"), "{help}");
        assert!(!help.contains("Nothing was left behind"), "{help}");
    }

    #[test]
    fn the_probe_failure_is_typed_rather_than_the_catch_all() {
        // Verbatim from an operator's terminal. The catch-all's own help
        // asks for exactly this promotion when a wording recurs, and this
        // one recurs by construction: a failed `enable` wedges the bucket
        // so that every following `enable` fails too.
        let e = repo_unreachable_error(
            "s3:https://nbg1.your-objectstorage.com/apprafter",
            b"Fatal: repository does not exist: unable to open config file: Stat: \
              The specified key does not exist.\n",
            b"Fatal: create key in repository at s3:https://nbg1.your-objectstorage.com/\
              apprafter failed: repository already contains keys\n",
        );
        assert_eq!(code_of(&e), "apprafter::backup::repo_probe_failed");
    }

    #[test]
    fn a_wedged_repo_names_the_leftover_keys_instead_of_the_credentials() {
        let e = repo_unreachable_error(
            "s3:https://nbg1.your-objectstorage.com/apprafter",
            b"Fatal: repository does not exist: unable to open config file: Stat: \
              The specified key does not exist.\n",
            b"Fatal: create key in repository at s3:https://nbg1.your-objectstorage.com/\
              apprafter failed: repository already contains keys\n",
        );
        let msg = format!("{e}");
        // The credentials demonstrably work — restic listed the keys with
        // them. Saying "bad credentials" here is the CLI guessing wrong.
        assert!(
            !msg.contains("bad credentials"),
            "must not blame working credentials: {msg}"
        );
        let help = help_of(&e);
        assert!(help.contains("keys/"), "names what to clear: {help}");
        assert!(help.contains("--prefix"), "names the alternative: {help}");
    }

    #[test]
    fn a_refused_write_points_at_the_grant_not_the_passphrase() {
        let e = repo_unreachable_error(
            "s3:https://nbg1.your-objectstorage.com/apprafter",
            b"Fatal: repository does not exist: unable to open config file: Stat: \
              The specified key does not exist.\n",
            b"Save(<config/0000000000>) failed: client.PutObject: Access Denied.\n\
              Fatal: create key in repository at s3:https://h/b failed: \
              client.PutObject: Access Denied.\n",
        );
        let help = help_of(&e);
        assert!(
            help.contains("permission"),
            "names the actual obstacle: {help}"
        );
        assert!(
            help.contains("keys/"),
            "warns that the failed init left one behind: {help}"
        );
    }

    #[test]
    fn a_config_that_will_not_open_is_read_as_a_passphrase_problem() {
        // The one case where the FIRST stderr carries the diagnosis: the
        // repository is there and intact, the passphrase is wrong, and
        // `init` can only ever answer "already initialized" — classifying
        // on it would report a healthy repository as a broken one.
        let e = repo_unreachable_error(
            "s3:https://h/b",
            b"Fatal: wrong password or no key found\n",
            b"Fatal: create key in repository at s3:https://h/b failed: repository master \
              key and config already initialized\n",
        );
        let help = help_of(&e);
        assert!(help.contains("passphrase"), "names the passphrase: {help}");
        assert!(
            help.contains("intact"),
            "says the data is still there: {help}"
        );
    }

    #[test]
    fn an_unrecognised_probe_failure_still_says_what_to_check() {
        // Conservative classification means most novel failures fall
        // through — but falling through must not mean falling back to the
        // catch-all's "the message above is the only context".
        let e = repo_unreachable_error(
            "s3:https://h/b",
            b"Fatal: something entirely new\n",
            b"Fatal: something entirely new\n",
        );
        let help = help_of(&e);
        assert!(
            !help.contains("catch-all"),
            "an unclassified probe is still a probe, not the catch-all: {help}"
        );
        assert!(
            help.contains("restic") && help.len() > 60,
            "says something actionable: {help}"
        );
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
        // The stub's kubeconfig: the interrupt's record of a helper apply
        // keeps the file's bytes, so it must be there to read.
        let kubeconfig = dir.path().join("kubeconfig.yaml");
        std::fs::write(&kubeconfig, "apiVersion: v1\nkind: Config\n").unwrap();
        KubectlExec {
            kubeconfig,
            kubectl_bin: path,
            // Private to the test: the process-wide set is the interrupt's.
            helpers: helper_interrupt::HelperPods::default(),
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
        k.exec_stream_to_file("pg-0", "prod", &["pg_dump", "-Fc"], &out, None)
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
            .exec_stream_to_file("pg-0", "prod", &["pg_dump"], &dir.path().join("out"), None)
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("exec_stream_to_file"), "{msg}");
        assert!(msg.contains('7'), "the exit status is stated: {msg}");
        assert!(
            msg.contains("pg_dump: server version mismatch"),
            "the pod's own error is the only useful part: {msg}"
        );
    }

    /// Run `exec_stream_to_file` on a thread and give up after `watchdog`, so
    /// a missing bound FAILS the test instead of hanging it.
    fn stream_with_watchdog(
        k: KubectlExec,
        out: PathBuf,
        bound: Option<Duration>,
        watchdog: Duration,
    ) -> (Duration, Result<()>) {
        let (done, finished) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let started = std::time::Instant::now();
            let result =
                k.exec_stream_to_file("bk-pg-db", "prod", &["pg_dump", "-Fc"], &out, bound);
            let _ = done.send((started.elapsed(), result));
        });
        finished
            .recv_timeout(watchdog)
            .unwrap_or_else(|_| panic!("exec_stream_to_file still running after {watchdog:?}"))
    }

    #[test]
    fn a_command_that_writes_nothing_within_the_bound_is_abandoned() {
        // The shape of a pg_dump waiting on a lock during its schema read:
        // alive, silent, and not about to change.
        let dir = tempfile::tempdir().unwrap();
        let k = stub_kubectl(&dir, "exec sleep 60");
        let bound = Duration::from_secs(1);
        let (elapsed, result) = stream_with_watchdog(
            k,
            dir.path().join("out"),
            Some(bound),
            Duration::from_secs(20),
        );
        let err = result.expect_err("a command silent past its bound must fail");
        assert!(backup_core::kube::is_no_output_error(&err), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("prod/bk-pg-db"), "{msg}");
        assert!(
            elapsed >= bound,
            "gave up after {elapsed:?}, before the bound"
        );
        assert!(
            elapsed < bound + Duration::from_secs(5),
            "gave up after {elapsed:?}, well past the {bound:?} bound"
        );
    }

    #[test]
    fn the_bound_times_only_the_first_byte() {
        // Writing at once and then going quiet for longer than the bound is a
        // dump copying a large table: it must run to the end.
        let dir = tempfile::tempdir().unwrap();
        let k = stub_kubectl(&dir, "printf 'PGDMP'\nsleep 3\nprintf 'REST'");
        let out = dir.path().join("out");
        let (_, result) = stream_with_watchdog(
            k,
            out.clone(),
            Some(Duration::from_secs(1)),
            Duration::from_secs(20),
        );
        result.expect("a command that wrote in time is never cut");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "PGDMPREST");
    }

    #[test]
    fn a_first_byte_that_arrives_inside_the_bound_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let k = stub_kubectl(&dir, "sleep 1\nprintf 'PGDMP'");
        let out = dir.path().join("out");
        let (_, result) = stream_with_watchdog(
            k,
            out.clone(),
            Some(Duration::from_secs(10)),
            Duration::from_secs(20),
        );
        result.expect("a first byte inside the bound is a success");
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "PGDMP");
    }

    #[test]
    fn a_bounded_command_that_fails_before_writing_reports_its_own_error() {
        // A pg_dump whose TABLE lock wait ran out exits before the first-output
        // bound: its own error, not the bound's, is what must come back.
        let dir = tempfile::tempdir().unwrap();
        let k = stub_kubectl(&dir, "echo 'LOCK TABLE public.t1' >&2\nexit 1");
        let (_, result) = stream_with_watchdog(
            k,
            dir.path().join("out"),
            Some(Duration::from_secs(10)),
            Duration::from_secs(20),
        );
        let msg = result.expect_err("exit 1 fails the step").to_string();
        assert!(msg.contains("LOCK TABLE public.t1"), "{msg}");
        assert!(!msg.contains(backup_core::kube::NO_OUTPUT_MARKER), "{msg}");
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

    /// A helper pod as `kubectl get` shows it once it is Running and Ready,
    /// with uid `uid`.
    fn ready_pod(uid: &str) -> String {
        json!({
            "metadata": {"name": "helper", "namespace": "prod", "uid": uid},
            "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": "True"}]}
        })
        .to_string()
    }

    #[test]
    fn apply_and_wait_pod_ready_pipes_the_spec_in_and_then_waits_for_ready() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        let seen = dir.path().join("spec.json");
        let applied = dir.path().join("applied");
        // `get` answers "no such pod" until the create, and the Ready pod
        // after.
        let k = stub_kubectl(
            &dir,
            &format!(
                "echo \"$@\" >> {log}\n\
                 if [ \"$1\" = create ]; then cat > {seen}; touch {applied}; fi\n\
                 if [ \"$1\" = get ] && [ -f {applied} ]; then printf '%s' '{ready}'; fi\n\
                 exit 0",
                log = log.display(),
                seen = seen.display(),
                applied = applied.display(),
                ready = ready_pod("u-1"),
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

        let argv: Vec<String> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        // Read (no pod), created — the apiserver's answer names the pod it
        // made — then read until Ready: readiness, not existence — a Pod that
        // exists but is not Ready cannot be exec'd into, which is the only
        // reason this helper is created.
        assert_eq!(
            argv,
            vec![
                "get pod helper -n prod --ignore-not-found -o json",
                "create --save-config -f - -n prod -o jsonpath={.metadata.uid}",
                "get pod helper -n prod --ignore-not-found -o json",
            ]
        );
    }

    /// A stub kubectl that logs every call and plays a pod named `helper`:
    /// `get` prints `pod` (a JSON document, or nothing: absent) until a
    /// `delete` removes it. `create` refuses as the apiserver does while
    /// there is a pod (`AlreadyExists`); otherwise it and `apply` run `put`
    /// (a shell snippet; it must read stdin) and, when that succeeds, leave
    /// the pod Running and Ready and print its uid, as `-o jsonpath` does:
    /// `u-created` for a pod a create made, and for an apply the uid the pod
    /// had (`u-applied` if it had none).
    fn stateful_stub(dir: &tempfile::TempDir, pod: &str, put: &str) -> (KubectlExec, PathBuf) {
        let log = dir.path().join("argv");
        let present = dir.path().join("present.json");
        if !pod.is_empty() {
            std::fs::write(&present, pod).unwrap();
        }
        let k = stub_kubectl(
            dir,
            &format!(
                "echo \"$@\" >> {log}\n\
                 case \"$1\" in\n\
                 get) cat {present} 2>/dev/null; exit 0;;\n\
                 delete) rm -f {present}; exit 0;;\n\
                 create) if [ -f {present} ]; then cat >/dev/null\n\
                     echo 'Error from server (AlreadyExists): error when creating \"STDIN\": \
                 pods \"helper\" already exists' >&2; exit 1; fi\n\
                   ( {put} ); rc=$?\n\
                   if [ $rc -eq 0 ]; then printf '{ready}' u-created > {present}; \
                 printf u-created; fi\n\
                   exit $rc;;\n\
                 apply) ( {put} ); rc=$?\n\
                   if [ $rc -eq 0 ]; then\n\
                     uid=$(sed -n 's/.*\"uid\": *\"\\([^\"]*\\)\".*/\\1/p' {present} 2>/dev/null)\n\
                     printf '{ready}' \"${{uid:-u-applied}}\" > {present}\n\
                     printf '%s' \"${{uid:-u-applied}}\"\n\
                   fi\n\
                   exit $rc;;\n\
                 esac",
                log = log.display(),
                present = present.display(),
                // Inside the single quotes of `printf`, where `"` is literal;
                // the one `%s` is the uid.
                ready = ready_pod("%s"),
            ),
        );
        (k, log)
    }

    fn calls(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(|l| l.split(' ').next().unwrap().to_string())
            .collect()
    }

    const HELPER: &str = r#"{"metadata": {"name": "helper", "namespace": "prod"}}"#;

    /// A helper pod left behind `Completed` by an earlier run never becomes
    /// Ready again, so applying over it used to cost the whole five-minute
    /// wait and then the run. It is deleted, waited out, and created again.
    #[test]
    fn an_ended_leftover_helper_is_deleted_and_created_again() {
        let dir = tempfile::tempdir().unwrap();
        let (k, log) = stateful_stub(
            &dir,
            r#"{"metadata": {"name": "helper"}, "status": {"phase": "Succeeded"}}"#,
            "cat >/dev/null; exit 0",
        );
        let spec: Value = serde_json::from_str(HELPER).unwrap();
        k.apply_and_wait_pod_ready(&spec).unwrap();

        assert_eq!(calls(&log), vec!["get", "delete", "create", "get"]);
        let argv = std::fs::read_to_string(&log).unwrap();
        assert!(
            argv.contains("get pod helper -n prod --ignore-not-found -o json"),
            "{argv}"
        );
        // A short grace (its `sleep` ignores SIGTERM) and a wait until it has
        // gone, bounded — the new pod cannot be created while it is there.
        assert!(
            argv.contains(
                "delete pod helper -n prod --ignore-not-found --grace-period=1 --wait=true \
                 --timeout=60s"
            ),
            "{argv}"
        );
    }

    /// A pod that is still running is used as it is: a same-spec apply over
    /// it changes nothing, and deleting it would kill whatever runs in it.
    #[test]
    fn a_running_helper_of_the_same_spec_is_applied_over_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let (k, log) = stateful_stub(
            &dir,
            r#"{"metadata": {"name": "helper"}, "status": {"phase": "Running"}}"#,
            "cat >/dev/null; exit 0",
        );
        let spec: Value = serde_json::from_str(HELPER).unwrap();
        k.apply_and_wait_pod_ready(&spec).unwrap();
        assert_eq!(calls(&log), vec!["get", "apply", "get"]);
    }

    /// The six-hour helper a command applies, and the same pod as `kubectl
    /// get` shows it, running, its container started `ago` before now.
    fn six_hour_helper(ago: Duration) -> (Value, String) {
        let spec = json!({
            "metadata": {"name": "helper", "namespace": "prod"},
            "spec": {"containers": [{"name": "dump", "command": ["sleep", "21600"]}]}
        });
        let started = chrono::Utc::now() - chrono::Duration::from_std(ago).unwrap();
        let mut pod = spec.clone();
        pod["status"] = json!({"phase": "Running", "containerStatuses": [{"name": "dump",
        "state": {"running": {
            "startedAt": started.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        }}}]});
        (spec, pod.to_string())
    }

    /// A helper left running by a command interrupted before its cleanup —
    /// Ctrl-C on `backup create` five hours ago — has one hour of its `sleep`
    /// left, and a dump in it would die then. It is replaced.
    #[test]
    fn a_running_leftover_with_hours_of_its_keep_alive_used_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let (spec, pod) = six_hour_helper(Duration::from_secs(5 * 3600));
        let (k, log) = stateful_stub(&dir, &pod, "cat >/dev/null; exit 0");
        k.apply_and_wait_pod_ready(&spec).unwrap();
        assert_eq!(calls(&log), vec!["get", "delete", "create", "get"]);
    }

    /// One another command created moments ago is used as it is.
    #[test]
    fn a_running_helper_started_moments_ago_is_used_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let (spec, pod) = six_hour_helper(Duration::from_secs(10));
        let (k, log) = stateful_stub(&dir, &pod, "cat >/dev/null; exit 0");
        k.apply_and_wait_pod_ready(&spec).unwrap();
        assert_eq!(calls(&log), vec!["get", "apply", "get"]);
    }

    /// A leftover whose spec cannot be applied over — an older CLI's or
    /// runner's — is refused by the apiserver on the FIRST line of kubectl's
    /// stderr, above a diff of the pod spec that can run longer than the lines
    /// an error keeps (sixty here). It is replaced all the same.
    #[test]
    fn a_leftover_whose_spec_cannot_change_in_place_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let refused = dir.path().join("refused-once");
        let (k, log) = stateful_stub(
            &dir,
            r#"{"metadata": {"name": "helper"}, "status": {"phase": "Running"}}"#,
            &format!(
                "cat >/dev/null\n\
                 if [ ! -f {refused} ]; then\n\
                   touch {refused}\n\
                   echo 'The Pod \"helper\" is invalid: spec: Forbidden: pod updates may not \
                 change fields other than `spec.containers[*].image`' >&2\n\
                   i=0; while [ $i -lt 60 ]; do echo \"  diff line $i\" >&2; i=$((i+1)); done\n\
                   exit 1\n\
                 fi\n\
                 exit 0",
                refused = refused.display()
            ),
        );
        let spec: Value = serde_json::from_str(HELPER).unwrap();
        k.apply_and_wait_pod_ready(&spec).unwrap();
        assert_eq!(calls(&log), vec!["get", "apply", "delete", "create", "get"]);
    }

    /// A backup helper pod spec, as the builders stamp it: the interrupt
    /// tracks only pods carrying the helper label.
    const LABELLED_HELPER: &str = r#"{"metadata": {"name": "helper", "namespace": "prod",
        "labels": {"apprafter.io/backup-helper": "true"}}}"#;

    /// WI-383: which pod each helper put left under its name is recorded for
    /// the interrupt, from the apiserver's answer — created by this command
    /// (deleted on Ctrl-C, by uid) or there before it (left for the run using
    /// it).
    #[test]
    fn each_helper_put_records_whether_it_created_its_pod() {
        use helper_interrupt::Origin;
        let spec: Value = serde_json::from_str(LABELLED_HELPER).unwrap();

        // No pod of that name: this command's create made the one there now.
        let dir = tempfile::tempdir().unwrap();
        let (k, _) = stateful_stub(&dir, "", "cat >/dev/null; exit 0");
        k.apply_and_wait_pod_ready(&spec).unwrap();
        assert_eq!(
            k.helpers.origin_of("prod", "helper"),
            Some(Origin::Created("u-created".into()))
        );

        // A running pod of the same spec, used as it is: not this command's.
        let dir = tempfile::tempdir().unwrap();
        let (k, _) = stateful_stub(
            &dir,
            r#"{"metadata": {"name": "helper", "uid": "u-theirs"}, "status": {"phase": "Running"}}"#,
            "cat >/dev/null; exit 0",
        );
        k.apply_and_wait_pod_ready(&spec).unwrap();
        assert_eq!(
            k.helpers.origin_of("prod", "helper"),
            Some(Origin::Reused("u-theirs".into()))
        );

        // An ended leftover is replaced: the pod there now is this create's.
        let dir = tempfile::tempdir().unwrap();
        let (k, _) = stateful_stub(
            &dir,
            r#"{"metadata": {"name": "helper", "uid": "u-old"}, "status": {"phase": "Succeeded"}}"#,
            "cat >/dev/null; exit 0",
        );
        k.apply_and_wait_pod_ready(&spec).unwrap();
        assert_eq!(
            k.helpers.origin_of("prod", "helper"),
            Some(Origin::Created("u-created".into()))
        );

        // A pod that is not a backup helper is none of the interrupt's.
        let dir = tempfile::tempdir().unwrap();
        let (k, _) = stateful_stub(&dir, "", "cat >/dev/null; exit 0");
        k.apply_and_wait_pod_ready(&serde_json::from_str(HELPER).unwrap())
            .unwrap();
        assert_eq!(k.helpers.origin_of("prod", "helper"), None);
    }

    /// The WI-383 review's case: no pod at the read, and another run — a
    /// scheduled backup of the same claim — creates one before this
    /// command's create lands. The create is refused (`AlreadyExists`), and
    /// that pod is read and applied over like any other: recorded as there
    /// before, so Ctrl-C leaves it and the other run's dump goes on. Before,
    /// an apply "configured" it and the first read took it for this
    /// command's.
    #[test]
    fn a_pod_another_run_created_after_the_read_is_not_taken_for_this_ones() {
        use helper_interrupt::Origin;
        let spec: Value = serde_json::from_str(LABELLED_HELPER).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        let theirs = dir.path().join("theirs.json");
        std::fs::write(
            &theirs,
            r#"{"metadata": {"name": "helper", "uid": "u-theirs"}, "status": {"phase": "Running",
                "conditions": [{"type": "Ready", "status": "True"}]}}"#,
        )
        .unwrap();
        // `get` finds nothing until the create; the other run's pod is there
        // by the time the create reaches the apiserver.
        let k = stub_kubectl(
            &dir,
            &format!(
                "echo \"$@\" >> {log}\n\
                 case \"$1\" in\n\
                 get) [ -f {log}.raced ] && cat {theirs}; exit 0;;\n\
                 create) cat >/dev/null; touch {log}.raced\n\
                   echo 'Error from server (AlreadyExists): error when creating \"STDIN\": \
                 pods \"helper\" already exists' >&2; exit 1;;\n\
                 apply) cat >/dev/null; printf u-theirs; exit 0;;\n\
                 esac",
                log = log.display(),
                theirs = theirs.display(),
            ),
        );
        k.apply_and_wait_pod_ready(&spec).unwrap();
        assert_eq!(calls(&log), vec!["get", "create", "get", "apply", "get"]);
        assert_eq!(
            k.helpers.origin_of("prod", "helper"),
            Some(Origin::Reused("u-theirs".into()))
        );
        assert_eq!(
            helper_interrupt::cleanup_action(&Origin::Reused("u-theirs".into())),
            helper_interrupt::CleanupAction::NotCreatedHere {
                uid: "u-theirs".into()
            }
        );
    }

    /// A create refused as `AlreadyExists`, and an apply refused as an
    /// immutable update, made nothing: when the step fails right after, the
    /// interrupt has no record of that pod at all, rather than one it cannot
    /// settle.
    #[test]
    fn a_refused_create_or_update_leaves_no_record() {
        let spec: Value = serde_json::from_str(LABELLED_HELPER).unwrap();
        // AlreadyExists, then the second read fails.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        let k = stub_kubectl(
            &dir,
            &format!(
                "echo \"$@\" >> {log}\n\
                 case \"$1\" in\n\
                 get) [ -f {log}.raced ] && {{ echo 'Unable to connect' >&2; exit 1; }}; exit 0;;\n\
                 create) cat >/dev/null; touch {log}.raced\n\
                   echo 'Error from server (AlreadyExists): pods \"helper\" already exists' >&2\n\
                   exit 1;;\n\
                 esac",
                log = log.display(),
            ),
        );
        k.apply_and_wait_pod_ready(&spec).unwrap_err();
        assert_eq!(calls(&log), vec!["get", "create", "get"]);
        assert_eq!(k.helpers.origin_of("prod", "helper"), None);

        // Refused as an immutable update, then the old pod will not go.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        let k = stub_kubectl(
            &dir,
            &format!(
                "echo \"$@\" >> {log}\n\
                 case \"$1\" in\n\
                 get) echo '{{\"metadata\": {{\"name\": \"helper\", \"uid\": \"u-old\"}}, \
                 \"status\": {{\"phase\": \"Running\"}}}}'; exit 0;;\n\
                 apply) cat >/dev/null; echo 'The Pod \"helper\" is invalid: spec: Forbidden: pod \
                 updates may not change fields other than `spec.containers[*].image`' >&2; exit 1;;\n\
                 delete) echo 'timed out waiting for the condition' >&2; exit 1;;\n\
                 esac",
                log = log.display(),
            ),
        );
        k.apply_and_wait_pod_ready(&spec).unwrap_err();
        assert_eq!(calls(&log), vec!["get", "apply", "delete"]);
        assert_eq!(k.helpers.origin_of("prod", "helper"), None);
    }

    /// Only an answer settles a pod. An apply answered with a pod other than
    /// the one read just before it (that one was replaced in between), and a
    /// create whose kubectl died unanswered — the same Ctrl-C reaches it —
    /// stay unconfirmed, and the interrupt leaves both.
    #[test]
    fn a_put_without_a_telling_answer_stays_unconfirmed() {
        use helper_interrupt::Origin;
        let spec: Value = serde_json::from_str(LABELLED_HELPER).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let (k, _) = stateful_stub(
            &dir,
            r#"{"metadata": {"name": "helper", "uid": "u-theirs"}, "status": {"phase": "Running"}}"#,
            // The apply lands on a pod created since the read.
            &format!(
                "cat >/dev/null; printf '{}' > {}",
                r#"{"metadata": {"name": "helper", "uid": "u-other"}, "status": {"phase": "Running"}}"#,
                dir.path().join("present.json").display()
            ),
        );
        k.apply_and_wait_pod_ready(&spec).unwrap();
        assert_eq!(
            k.helpers.origin_of("prod", "helper"),
            Some(Origin::Unconfirmed)
        );

        let dir = tempfile::tempdir().unwrap();
        let (k, _) = stateful_stub(&dir, "", "cat >/dev/null; kill -9 $$");
        k.apply_and_wait_pod_ready(&spec).unwrap_err();
        assert_eq!(
            k.helpers.origin_of("prod", "helper"),
            Some(Origin::Unconfirmed)
        );

        // A create answered with no uid names no pod.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        let k = stub_kubectl(
            &dir,
            &format!(
                "echo \"$@\" >> {log}\n\
                 case \"$1\" in\n\
                 get) [ -f {log}.made ] && printf '%s' '{ready}'; exit 0;;\n\
                 create) cat >/dev/null; touch {log}.made; exit 0;;\n\
                 esac",
                log = log.display(),
                ready = ready_pod("u-1"),
            ),
        );
        k.apply_and_wait_pod_ready(&spec).unwrap();
        assert_eq!(
            k.helpers.origin_of("prod", "helper"),
            Some(Origin::Unconfirmed)
        );
    }

    /// Forgotten once the command's own delete went through — and kept when
    /// it did not (its kubectl may have died of the same Ctrl-C), for the
    /// interrupt to delete.
    #[test]
    fn a_helper_is_forgotten_only_once_its_delete_went_through() {
        use helper_interrupt::Origin;
        let spec: Value = serde_json::from_str(LABELLED_HELPER).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (k, _) = stateful_stub(&dir, "", "cat >/dev/null; exit 0");
        k.apply_and_wait_pod_ready(&spec).unwrap();
        k.delete_pod_best_effort("helper", "prod");
        assert_eq!(k.helpers.origin_of("prod", "helper"), None);

        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        let k = stub_kubectl(
            &dir,
            &format!(
                "echo \"$@\" >> {log}\n\
                 case \"$1\" in\n\
                 get) [ -f {log}.applied ] && printf '%s' '{ready}'; exit 0;;\n\
                 create) cat >/dev/null; touch {log}.applied; printf u-1; exit 0;;\n\
                 delete) exit 1;;\n\
                 esac",
                log = log.display(),
                ready = ready_pod("u-1"),
            ),
        );
        k.apply_and_wait_pod_ready(&spec).unwrap();
        k.delete_pod_best_effort("helper", "prod");
        assert_eq!(
            k.helpers.origin_of("prod", "helper"),
            Some(Origin::Created("u-1".into()))
        );
    }

    /// Once interrupted, the command's own thread makes no kubectl call at
    /// all: no apply that would outlive it, no exec, and no delete by name —
    /// the interrupt's deletes, by uid, are the only ones.
    #[test]
    fn once_interrupted_no_kubectl_is_run_from_the_command() {
        let spec: Value = serde_json::from_str(LABELLED_HELPER).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (k, log) = stateful_stub(&dir, "", "cat >/dev/null; exit 0");
        k.helpers.close();
        let refused = k.apply_and_wait_pod_ready(&spec).unwrap_err().to_string();
        assert!(refused.starts_with("interrupted"), "{refused}");
        let out = dir.path().join("out");
        assert!(k
            .exec_stream_to_file("helper", "prod", &["pg_dump"], &out, None)
            .is_err());
        assert!(k
            .exec_stream_from_file("helper", "prod", &["pg_restore"], &out,)
            .is_err());
        assert!(k.get_secret_key("db-conn", "prod", "user").is_err());
        assert!(k.get_json(&["get", "pods", "-n", "prod"]).is_err());
        k.delete_pod_best_effort("helper", "prod");
        assert!(!log.exists(), "{}", std::fs::read_to_string(&log).unwrap());
    }

    /// Nor does the command start restic, or read the cluster's identity,
    /// once it has had its signal.
    #[test]
    fn once_interrupted_no_restic_and_no_identity_read_is_started() {
        struct Counting(std::cell::Cell<usize>);
        impl ResticRunner for Counting {
            fn run(&self, _: &[String], _: &str) -> Result<()> {
                self.0.set(self.0.get() + 1);
                Ok(())
            }
            fn run_stdout(&self, _: &[String], _: &str) -> Result<String> {
                self.0.set(self.0.get() + 1);
                Ok(String::new())
            }
            fn run_backup(&self, _: &[String], _: &str) -> Result<Option<String>> {
                self.0.set(self.0.get() + 1);
                Ok(None)
            }
            fn run_capture(&self, _: &[String], _: &str) -> Result<backup_core::ResticOutput> {
                self.0.set(self.0.get() + 1);
                Ok(backup_core::ResticOutput::default())
            }
        }
        let r = RefusingAfterInterrupt(Counting(std::cell::Cell::new(0)));
        let argv = vec!["backup".to_string()];
        r.run(&argv, "pw").unwrap();
        r.run_stdout(&argv, "pw").unwrap();
        r.run_backup(&argv, "pw").unwrap();
        assert_eq!(r.0 .0.get(), 3, "before the signal every call goes through");

        let kc = helper_interrupt::test_seam::unreachable_kubeconfig();
        let _interrupted = helper_interrupt::test_seam::interrupt_this_thread();
        for e in [
            r.run(&argv, "pw").unwrap_err(),
            r.run_stdout(&argv, "pw").unwrap_err(),
            r.run_backup(&argv, "pw").unwrap_err(),
            read_cluster_uid(kc.path()).unwrap_err(),
        ] {
            assert!(e.to_string().starts_with("interrupted"), "{e}");
        }
        assert_eq!(r.0 .0.get(), 3, "no restic after the signal");
    }

    /// WI-383, the CLI's side: a helper whose credential Secret is missing
    /// cannot start its container, and the wait says so with the kubelet's
    /// words once that has held for the grace — not after five minutes.
    #[test]
    fn a_helper_whose_credential_secret_is_missing_fails_the_wait_with_the_kubelets_words() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = json!({
            "metadata": {"name": "helper", "uid": "u-1"},
            "status": {"phase": "Pending", "containerStatuses": [{"name": "dump",
                "state": {"waiting": {"reason": "CreateContainerConfigError",
                    "message": "couldn't find key pass in Secret prod/db-conn"}}}]}
        });
        // From a file: the kubelet's message has an apostrophe in it.
        let pod = dir.path().join("pod.json");
        std::fs::write(&pod, blocked.to_string()).unwrap();
        let k = stub_kubectl(
            &dir,
            &format!("[ \"$1\" = get ] && cat {}\nexit 0", pod.display()),
        );
        let started = std::time::Instant::now();
        let msg = k
            .wait_pod_ready(
                "helper",
                "prod",
                Duration::from_secs(20),
                Duration::from_millis(50),
                Duration::from_millis(300),
            )
            .unwrap_err()
            .to_string();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(
            msg.starts_with(
                "helper pod prod/helper cannot start its container: couldn't find key pass in \
                 Secret prod/db-conn (CreateContainerConfigError)"
            ),
            "{msg}"
        );
    }

    /// Every other apply failure is the run's own, and nothing is deleted.
    #[test]
    fn another_apply_failure_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (k, log) = stateful_stub(
            &dir,
            r#"{"metadata": {"name": "helper"}, "status": {"phase": "Running"}}"#,
            "cat >/dev/null; echo 'The Pod \"helper\" is invalid: metadata.name' >&2; exit 1",
        );
        let spec: Value = serde_json::from_str(HELPER).unwrap();
        let msg = k.apply_and_wait_pod_ready(&spec).unwrap_err().to_string();
        assert!(msg.contains("metadata.name"), "{msg}");
        assert_eq!(calls(&log), vec!["get", "apply"]);
    }

    /// A stale pod that will not go — its node unreachable — stops the step
    /// with the way out, rather than applying over it.
    #[test]
    fn a_stale_helper_that_will_not_go_fails_with_the_way_out() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv");
        let k = stub_kubectl(
            &dir,
            &format!(
                "echo \"$@\" >> {log}\n\
                 case \"$1\" in\n\
                 get) echo '{{\"metadata\": {{\"name\": \"helper\", \"deletionTimestamp\": \
                 \"2026-09-23T00:00:00Z\"}}}}'; exit 0;;\n\
                 delete) echo 'error: timed out waiting for the condition' >&2; exit 1;;\n\
                 *) cat >/dev/null; exit 0;;\n\
                 esac",
                log = log.display()
            ),
        );
        let spec: Value = serde_json::from_str(HELPER).unwrap();
        let msg = k.apply_and_wait_pod_ready(&spec).unwrap_err().to_string();
        assert!(
            msg.contains("was not gone 60s after it was deleted"),
            "{msg}"
        );
        assert!(msg.contains("--force --grace-period=0"), "{msg}");
        assert!(msg.contains("timed out waiting for the condition"), "{msg}");
        assert_eq!(calls(&log), vec!["get", "delete"]);
    }

    /// Real-apiserver proof of the same replacement through `kubectl`: the
    /// ended pod as `kubectl get` shows it, and the apiserver's refusal to
    /// change a pod's spec in place as `kubectl apply` prints it — first line
    /// of a long stderr. Skipped by default; opt in against a DISPOSABLE kind
    /// cluster:
    ///
    /// ```text
    /// APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig> cargo test -p apprafter \
    ///     --lib a_leftover_helper_pod_is_replaced_through_kubectl_on_kind -- --ignored
    /// ```
    ///
    /// Refuses any context that is not `kind-*`. The helper image,
    /// `docker.io/library/alpine:3.24`, is pulled `IfNotPresent`.
    #[test]
    #[ignore = "needs a kind cluster: APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig>"]
    fn a_leftover_helper_pod_is_replaced_through_kubectl_on_kind() {
        const NS: &str = "apprafter-stale-helper-kubectl";
        const POD: &str = "bk-vol-stale";
        // Explicitly opted in, so a missing precondition is a FAILURE.
        assert_eq!(
            std::env::var("APPRAFTER_K8S_SMOKE").as_deref(),
            Ok("1"),
            "run with APPRAFTER_K8S_SMOKE=1 (this test creates objects in the cluster)"
        );
        let kubeconfig = PathBuf::from(
            std::env::var_os("KUBECONFIG").expect("KUBECONFIG must name the kind kubeconfig"),
        );
        let kubectl = |args: &[&str]| {
            let out = Command::new("kubectl")
                .args(args)
                .env("KUBECONFIG", &kubeconfig)
                .output()
                .expect("run kubectl");
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            )
        };
        let (_, ctx) = kubectl(&["config", "current-context"]);
        assert!(
            ctx.starts_with("kind-"),
            "refusing to run against context {ctx:?}: this test only targets kind clusters"
        );
        struct DeleteNs<'a>(&'a Path);
        impl Drop for DeleteNs<'_> {
            fn drop(&mut self) {
                let _ = Command::new("kubectl")
                    .args(["delete", "namespace", NS, "--wait=false"])
                    .env("KUBECONFIG", self.0)
                    .output();
            }
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        while kubectl(&["get", "namespace", NS, "-o", "jsonpath={.status.phase}"]).1
            == "Terminating"
        {
            assert!(
                std::time::Instant::now() < deadline,
                "{NS} stuck Terminating"
            );
            thread::sleep(Duration::from_secs(1));
        }
        let _ = kubectl(&["create", "namespace", NS]);
        let _cleanup = DeleteNs(&kubeconfig);

        let k = KubectlExec::new(kubeconfig.clone());
        let helper = |secs: u64| {
            json!({
                "apiVersion": "v1", "kind": "Pod",
                "metadata": {"name": POD, "namespace": NS,
                             "labels": {"apprafter.io/backup-helper": "true"}},
                "spec": {"restartPolicy": "Never", "containers": [{
                    "name": "dump", "image": "docker.io/library/alpine:3.24",
                    "imagePullPolicy": "IfNotPresent",
                    "command": ["sleep", secs.to_string()]}]}
            })
        };
        let field = |path: &str| kubectl(&["get", "pod", POD, "-n", NS, "-o", path]).1;

        // An ENDED leftover of the same spec.
        k.apply_and_wait_pod_ready(&helper(3))
            .expect("first helper Ready");
        let ended = field("jsonpath={.metadata.uid}");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        while field("jsonpath={.status.phase}") != "Succeeded" {
            assert!(
                std::time::Instant::now() < deadline,
                "the 3 s helper never ended"
            );
            thread::sleep(Duration::from_secs(1));
        }
        let started = std::time::Instant::now();
        k.apply_and_wait_pod_ready(&helper(3))
            .expect("an ended leftover is replaced");
        let took = started.elapsed();
        assert_ne!(field("jsonpath={.metadata.uid}"), ended);
        assert!(took < Duration::from_secs(90), "took {took:?}");
        eprintln!("kubectl: ended leftover replaced in {took:?}");

        // A RUNNING leftover with another keep-alive, two ways: an older
        // runner's `sleep 3600` has less than this command's whole keep-alive
        // and is replaced on what `kubectl get` shows, before any apply; one
        // with a LONGER keep-alive (a deadline since lowered) passes that
        // check, and the apiserver refuses to change its spec in place.
        for (old_secs, how) in [
            (3600, "by its keep-alive"),
            (43200, "by the apply's refusal"),
        ] {
            let _ = kubectl(&[
                "delete",
                "pod",
                POD,
                "-n",
                NS,
                "--grace-period=1",
                "--wait=true",
            ]);
            k.apply_and_wait_pod_ready(&helper(old_secs))
                .expect("the old helper Ready");
            let old = field("jsonpath={.metadata.uid}");
            let started = std::time::Instant::now();
            k.apply_and_wait_pod_ready(&helper(21600))
                .expect("a leftover with another keep-alive is replaced");
            let took = started.elapsed();
            assert_ne!(field("jsonpath={.metadata.uid}"), old, "{old_secs}");
            assert_eq!(
                field("jsonpath={.spec.containers[0].command}"),
                r#"["sleep","21600"]"#
            );
            assert!(took < Duration::from_secs(90), "took {took:?}");
            eprintln!("kubectl: running `sleep {old_secs}` leftover replaced {how} in {took:?}");
        }
    }

    /// Real-apiserver proof, through `kubectl`, that a running helper of the
    /// SAME spec is used as it is while it is new and replaced once it has
    /// used more than `RUNNING_HELPER_REUSE_MARGIN` of its keep-alive — the
    /// helper an interrupted `backup create` leaves behind. It waits out the
    /// margin, about six minutes. Skipped by default; opt in against a
    /// DISPOSABLE kind cluster:
    ///
    /// ```text
    /// APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig> cargo test -p apprafter \
    ///     --lib a_running_helper_is_reused_while_new_and_replaced_once_aged_through_kubectl \
    ///     -- --ignored
    /// ```
    #[test]
    #[ignore = "needs a kind cluster: APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig>"]
    fn a_running_helper_is_reused_while_new_and_replaced_once_aged_through_kubectl_on_kind() {
        const NS: &str = "apprafter-stale-helper-reuse-kubectl";
        const POD: &str = "bk-vol-aged";
        assert_eq!(
            std::env::var("APPRAFTER_K8S_SMOKE").as_deref(),
            Ok("1"),
            "run with APPRAFTER_K8S_SMOKE=1 (this test creates objects in the cluster)"
        );
        let kubeconfig = PathBuf::from(
            std::env::var_os("KUBECONFIG").expect("KUBECONFIG must name the kind kubeconfig"),
        );
        let kubectl = |args: &[&str]| {
            let out = Command::new("kubectl")
                .args(args)
                .env("KUBECONFIG", &kubeconfig)
                .output()
                .expect("run kubectl");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let ctx = kubectl(&["config", "current-context"]);
        assert!(
            ctx.starts_with("kind-"),
            "refusing to run against context {ctx:?}: this test only targets kind clusters"
        );
        struct DeleteNs<'a>(&'a Path);
        impl Drop for DeleteNs<'_> {
            fn drop(&mut self) {
                let _ = Command::new("kubectl")
                    .args(["delete", "namespace", NS, "--wait=false"])
                    .env("KUBECONFIG", self.0)
                    .output();
            }
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        while kubectl(&["get", "namespace", NS, "-o", "jsonpath={.status.phase}"]) == "Terminating"
        {
            assert!(
                std::time::Instant::now() < deadline,
                "{NS} stuck Terminating"
            );
            thread::sleep(Duration::from_secs(1));
        }
        let _ = kubectl(&["create", "namespace", NS]);
        let _cleanup = DeleteNs(&kubeconfig);

        let k = KubectlExec::new(kubeconfig.clone());
        let spec = json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": POD, "namespace": NS,
                         "labels": {"apprafter.io/backup-helper": "true"}},
            "spec": {"restartPolicy": "Never", "containers": [{
                "name": "dump", "image": "docker.io/library/alpine:3.24",
                "imagePullPolicy": "IfNotPresent",
                "command": ["sleep", "21600"]}]}
        });
        let uid = || {
            kubectl(&[
                "get",
                "pod",
                POD,
                "-n",
                NS,
                "-o",
                "jsonpath={.metadata.uid}",
            ])
        };

        k.apply_and_wait_pod_ready(&spec).expect("helper Ready");
        let first = uid();
        k.apply_and_wait_pod_ready(&spec)
            .expect("a new helper is applied over");
        assert_eq!(uid(), first, "a helper created a moment ago is reused");

        let margin = backup_core::helper_pod::RUNNING_HELPER_REUSE_MARGIN;
        thread::sleep(margin + Duration::from_secs(10));
        let started = std::time::Instant::now();
        k.apply_and_wait_pod_ready(&spec)
            .expect("an aged helper is replaced");
        let took = started.elapsed();
        assert_ne!(uid(), first, "a new pod, not the aged one");
        assert!(took < Duration::from_secs(90), "took {took:?}");
        eprintln!("kubectl: running helper aged past {margin:?} replaced in {took:?}");
    }

    /// Real-cluster proof, through `kubectl exec -i` as a restore runs it,
    /// that a load killed by its helper pod's keep-alive is explained: the
    /// words kubectl uses for the killed exec, and the kubelet's report of the
    /// container's end. Skipped by default; opt in against a DISPOSABLE kind
    /// cluster:
    ///
    /// ```text
    /// APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig> cargo test -p apprafter \
    ///     --lib a_load_killed_by_its_keep_alive_is_explained_through_kubectl_on_kind \
    ///     -- --ignored
    /// ```
    #[test]
    #[ignore = "needs a kind cluster: APPRAFTER_K8S_SMOKE=1 KUBECONFIG=<kind kubeconfig>"]
    fn a_load_killed_by_its_keep_alive_is_explained_through_kubectl_on_kind() {
        const NS: &str = "apprafter-keep-alive-kubectl";
        const POD: &str = "ld-pg-keepalive";
        assert_eq!(
            std::env::var("APPRAFTER_K8S_SMOKE").as_deref(),
            Ok("1"),
            "run with APPRAFTER_K8S_SMOKE=1 (this test creates objects in the cluster)"
        );
        let kubeconfig = PathBuf::from(
            std::env::var_os("KUBECONFIG").expect("KUBECONFIG must name the kind kubeconfig"),
        );
        let kubectl = |args: &[&str]| {
            let out = Command::new("kubectl")
                .args(args)
                .env("KUBECONFIG", &kubeconfig)
                .output()
                .expect("run kubectl");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let ctx = kubectl(&["config", "current-context"]);
        assert!(
            ctx.starts_with("kind-"),
            "refusing to run against context {ctx:?}: this test only targets kind clusters"
        );
        struct DeleteNs<'a>(&'a Path);
        impl Drop for DeleteNs<'_> {
            fn drop(&mut self) {
                let _ = Command::new("kubectl")
                    .args(["delete", "namespace", NS, "--wait=false"])
                    .env("KUBECONFIG", self.0)
                    .output();
            }
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        while kubectl(&["get", "namespace", NS, "-o", "jsonpath={.status.phase}"]) == "Terminating"
        {
            assert!(
                std::time::Instant::now() < deadline,
                "{NS} stuck Terminating"
            );
            thread::sleep(Duration::from_secs(1));
        }
        let _ = kubectl(&["create", "namespace", NS]);
        let _cleanup = DeleteNs(&kubeconfig);

        let k = KubectlExec::new(kubeconfig.clone());
        k.apply_and_wait_pod_ready(&json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": POD, "namespace": NS,
                         "labels": {"apprafter.io/backup-helper": "true"}},
            "spec": {"restartPolicy": "Never", "containers": [{
                "name": "dump", "image": "docker.io/library/alpine:3.24",
                "imagePullPolicy": "IfNotPresent",
                "command": backup_core::helper_pod::keep_alive_command(Duration::from_secs(8))}]}
        }))
        .expect("helper Ready");

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("dump");
        std::fs::write(&input, b"payload").unwrap();
        let started = std::time::Instant::now();
        // Reads its stdin, then outlives the pod's keep-alive.
        let err = k
            .exec_stream_from_file(POD, NS, &["sh", "-c", "cat >/dev/null; sleep 60"], &input)
            .expect_err("the keep-alive ends the load");
        eprintln!("raw exec error after {:?}: {err}", started.elapsed());
        assert!(backup_core::helper_pod::is_exit_137(&err), "{err}");
        let explained =
            backup_core::helper_pod::explain_keep_alive_end(&k, POD, NS, err).to_string();
        eprintln!("explained: {explained}");
        assert!(
            explained.contains("keep-alive of 8s ran out"),
            "{explained}"
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
        // The apply branch DRAINS stdin, because the real `kubectl apply -f -`
        // does and a stub that exits without reading is a different scenario
        // from the one this test names. It is also a race: the parent writes
        // the spec right after `spawn`, and if the stub has already exited the
        // write gets `EPIPE` and the assertion below sees "Broken pipe"
        // instead of the readiness message. That is what turned this test red
        // on a loaded CI runner (2026-09-10) after passing since it landed —
        // 500 local runs, including pinned to one CPU, never reproduced it.
        //
        // The wait itself is driven with a one-second timeout: the command's
        // own is `POD_READY_TIMEOUT`, five minutes.
        let waits = stub_kubectl(
            &dir,
            "if [ \"$1\" = get ]; then \
             printf '%s' '{\"metadata\": {\"name\": \"helper\"}, \"status\": {\"phase\": \"Pending\"}}'; \
             fi\ncat >/dev/null\nexit 0",
        );
        let err = waits
            .wait_pod_ready(
                "helper",
                "prod",
                Duration::from_secs(1),
                Duration::from_millis(50),
                backup_core::helper_pod::CONTAINER_CONFIG_ERROR_GRACE,
            )
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("did not reach Ready within 1s"), "{msg}");
        assert!(msg.contains("helper") && msg.contains("prod"), "{msg}");
        assert_eq!(
            backup_core::helper_pod::POD_READY_TIMEOUT,
            Duration::from_secs(300),
            "the documented five minutes (docs: How a run may take)"
        );

        // the create itself fails → the apiserver's own complaint is carried.
        // (The look for a leftover pod before it finds none.)
        let dir2 = tempfile::tempdir().unwrap();
        let applies = stub_kubectl(
            &dir2,
            "if [ \"$1\" = create ]; then\n  cat >/dev/null\n  \
             echo 'error: forbidden: pods is forbidden' >&2\n  exit 1\nfi\nexit 0",
        );
        let err = applies.apply_and_wait_pod_ready(&spec).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("apply_and_wait_pod_ready(create)"), "{msg}");
        assert!(msg.contains("pods is forbidden"), "{msg}");
    }

    #[test]
    fn a_kubectl_that_dies_before_reading_the_spec_reports_its_own_complaint() {
        // The failure this guards is a diagnosis being replaced by a symptom.
        // kubectl that exits before touching stdin — an unreadable kubeconfig,
        // a denied RBAC rule, a bad flag — leaves the parent's `write_all`
        // with `EPIPE`. Returning that surfaces "Broken pipe (os error 32)"
        // and drops the one message that says what actually went wrong, which
        // is already sitting on the child's stderr. The write result is
        // therefore held until the child has been reaped.
        // The padding is what makes the pipe break at all, and without it this
        // test passes against the defect it exists to catch: a small spec fits
        // entirely in the pipe buffer, so the parent's `write_all` returns
        // before the child's exit can be noticed and there is no `EPIPE` to
        // mishandle. Over the buffer (64 KiB on Linux) the write must block
        // for a reader that is never coming, and the child's exit delivers
        // `EPIPE` every time. Verified by reverting the fix: with the small
        // spec the test still passed, with this one it fails.
        let dir = tempfile::tempdir().unwrap();
        let spec = json!({
            "metadata": {"name": "helper", "namespace": "prod",
                         "annotations": {"pad": "x".repeat(256 * 1024)}}
        });
        let dies = stub_kubectl(
            &dir,
            "if [ \"$1\" = create ]; then echo 'error: Unauthorized' >&2; exit 1; fi\nexit 0",
        );
        let msg = format!("{}", dies.apply_and_wait_pod_ready(&spec).unwrap_err());
        assert!(msg.contains("Unauthorized"), "{msg}");
        assert!(!msg.contains("Broken pipe"), "{msg}");

        // The other half of the same decision, and the reason the write
        // result is checked AFTER the status rather than discarded: a child
        // that exits 0 without consuming the spec still failed us. Exit 0 is
        // the tool's claim about what it did with its input, and it is not
        // evidence that the input arrived — so an undelivered manifest is
        // reported, never quietly treated as applied.
        //
        // Padded for the same reason as above, and this half pins the decision
        // rather than guarding a regression: the old shape reported the pipe
        // error here too, so it passes either way.
        let dir2 = tempfile::tempdir().unwrap();
        let quiet = stub_kubectl(&dir2, "exit 0");
        let msg = format!("{}", quiet.apply_and_wait_pod_ready(&spec).unwrap_err());
        assert!(msg.contains("write pod spec to kubectl create"), "{msg}");
        assert!(msg.contains("Broken pipe"), "{msg}");
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
            "metadata": {"name": "apprafter-backup-28900000", "ownerReferences": owned_by(BACKUP_CRONJOB_NAME)},
            "status": {"succeeded": 1}
        });
        // A finished Job carries its condition. Counts alone (`failed: 1`,
        // nothing active) are a Job between an attempt and its retry.
        let check_job = json!({
            "metadata": {"name": "apprafter-backup-check-28900000", "ownerReferences": owned_by(CHECK_CRONJOB_NAME)},
            "status": {"failed": 7, "conditions": [
                {"type": "Failed", "status": "True", "reason": "BackoffLimitExceeded"}
            ]}
        });
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let s = format_backup_status(
            Some(&spec),
            &[backup_job, check_job],
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("apprafter-backup-28900000"));
        assert!(s.contains("apprafter-backup-check-28900000"));
        // backup is Succeeded, check is Failed
        assert!(s.contains("Succeeded"));
        assert!(s.contains("Failed"));
    }

    // ------------------------------------------------------------------
    // A backup Job whose pod no node has room for (WI-386)
    // ------------------------------------------------------------------

    /// The scheduler's message, verbatim from the live run that found this.
    const NO_ROOM: &str = "0/1 nodes are available: 1 Insufficient memory. no new claims to \
                           deallocate, preemption: 0/1 nodes are available: 1 No preemption \
                           victims found for incoming pod.";

    fn unfinished_job(name: &str, uid: &str, owner_kind: Option<&str>) -> Value {
        let mut j = json!({
            "metadata": {"name": name, "uid": uid},
            "spec": {"template": {"spec": {"containers": [{"name": "runner", "resources": {
                "requests": {"cpu": "100m", "memory": "256Mi"}, "limits": {"memory": "512Mi"}
            }}]}}},
            "status": {"active": 1, "startTime": "2026-09-23T14:39:47Z"}
        });
        let cronjob = if name.starts_with(CHECK_CRONJOB_NAME) {
            CHECK_CRONJOB_NAME
        } else {
            BACKUP_CRONJOB_NAME
        };
        match owner_kind {
            Some(kind) => {
                j["metadata"]["ownerReferences"] =
                    json!([{"kind": kind, "name": cronjob, "uid": "cj-uid"}]);
            }
            // `backup run`'s own Job: no owner, and its mark.
            None => j["metadata"]["labels"] = json!({"apprafter.io/manual": "true"}),
        }
        j
    }

    fn pending_pod(job_uid: &str, pod_uid: &str) -> Value {
        json!({
            "metadata": {"name": format!("{pod_uid}-pod"), "uid": pod_uid,
                         "creationTimestamp": "2026-09-23T14:39:47Z",
                         "ownerReferences": [{"kind": "Job", "uid": job_uid, "name": "x"}]},
            "spec": {},
            "status": {"phase": "Pending", "conditions": [{
                "type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                "message": NO_ROOM
            }]}
        })
    }

    #[test]
    fn backup_status_says_a_runner_nobody_can_place_is_pending_not_running() {
        // Live output on the 4 GB node:
        //   Last backup Job: apprafter-backup-manual-20260923-144838 — Running (…)
        // while its pod had been Pending for 22 minutes with FailedScheduling.
        let spec = json!({"enabled": true, "bucket": "s3:x", "schedule": "0 3 * * *"});
        let scheduled = unfinished_job("apprafter-backup-29312345", "job-1", Some("CronJob"));
        let check = unfinished_job("apprafter-backup-check-29312346", "job-2", Some("CronJob"));
        let pods = [pending_pod("job-1", "pod-1"), pending_pod("job-2", "pod-2")];
        let s = format_backup_status(
            Some(&spec),
            &[scheduled, check],
            &pods,
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(
            s.contains(&format!(
                "Last backup Job: apprafter-backup-29312345 — Pending, cannot be scheduled: \
                 {NO_ROOM} (2026-09-23 23:39:47 Asia/Tokyo)"
            )),
            "{s}"
        );
        assert!(
            s.contains("Last check Job:  apprafter-backup-check-29312346 — Pending, cannot be"),
            "the check Job asks for the same room and is read the same way: {s}"
        );
        assert!(!s.contains("Running"), "{s}");
        assert!(s.contains("`apprafter top`"), "{s}");
        assert!(s.contains(job_pod::RUNNER_UNSCHEDULABLE_DOC), "{s}");
        assert!(
            s.contains("the schedule starts no other backup"),
            "a scheduled Job holds the schedule while it waits: {s}"
        );
        // The hint sits under its own Job line, before the next section.
        let backup_at = s.find("Last backup Job:").unwrap();
        let hint_at = s.find("`apprafter top`").unwrap();
        let check_at = s.find("Last check Job:").unwrap();
        assert!(backup_at < hint_at && hint_at < check_at, "{s}");
    }

    #[test]
    fn backup_status_without_the_pods_keeps_the_jobs_own_view() {
        // No pod listing (or none matched): the Job's counts are what is
        // left, as before. Nothing is invented.
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let job = unfinished_job("apprafter-backup-manual-x", "job-1", None);
        let s = format_backup_status(
            Some(&spec),
            std::slice::from_ref(&job),
            &[],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("apprafter-backup-manual-x — Running"), "{s}");
        assert!(!s.contains("`apprafter top`"), "{s}");
    }

    #[test]
    fn backup_status_says_a_job_between_attempts_is_retrying_not_failed() {
        // After an attempt fails the Job controller waits (10 s, doubling up
        // to 6 min) before it starts the next. In that gap the Job has no
        // live pod, `active: 0` and `failed: 1`, and the counts alone read
        // `Failed`. It has not failed, and it still holds the schedule.
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let mut job = unfinished_job("apprafter-backup-29312345", "job-1", Some("CronJob"));
        job["spec"]["backoffLimit"] = json!(6);
        job["status"] = json!({"failed": 1, "startTime": "2026-09-23T14:39:47Z"});
        let mut failed = pending_pod("job-1", "pod-1");
        failed["spec"]["nodeName"] = json!("node-1");
        failed["status"] = json!({"phase": "Failed", "reason": "Evicted"});
        let s = format_backup_status(
            Some(&spec),
            std::slice::from_ref(&job),
            &[failed],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(
            s.contains(
                "apprafter-backup-29312345 — Retrying after 1 failed attempt (7 attempts at most)"
            ),
            "{s}"
        );
        assert!(!s.contains("— Failed"), "{s}");
    }

    #[test]
    fn a_finished_job_is_not_re_read_from_a_pod_left_behind() {
        // The Job's condition is the verdict; a leftover pod cannot turn a
        // failed Job into "Pending".
        let spec = json!({"enabled": true, "bucket": "s3:x"});
        let mut job = unfinished_job("apprafter-backup-1", "job-1", Some("CronJob"));
        job["status"] = json!({"failed": 1, "startTime": "2026-09-23T14:39:47Z", "conditions": [
            {"type": "Failed", "status": "True", "reason": "DeadlineExceeded",
             "message": "Job was active longer than specified deadline"}
        ]});
        let s = format_backup_status(
            Some(&spec),
            std::slice::from_ref(&job),
            &[pending_pod("job-1", "pod-1")],
            None,
            None,
            &tokyo(),
            Some("Asia/Tokyo"),
        );
        assert!(s.contains("— Failed: DeadlineExceeded"), "{s}");
        assert!(!s.contains("Pending"), "{s}");
        assert!(!s.contains("`apprafter top`"), "{s}");
    }

    #[test]
    fn the_wait_gives_up_on_a_pod_unschedulable_for_the_whole_grace() {
        let job = unfinished_job("apprafter-backup-manual-x", "job-1", None);
        let pods = [pending_pod("job-1", "pod-1")];
        let t0 = std::time::Instant::now();
        let hour = Duration::from_secs(3600);
        let mut clock = UnschedulableClock::default();
        let step = |clock: &mut UnschedulableClock, at: u64| {
            wait_step(
                Some(&job),
                &pods,
                &[],
                clock,
                t0 + Duration::from_secs(at),
                Duration::from_secs(at),
                hour,
            )
        };
        assert!(
            matches!(step(&mut clock, 5), WaitStep::Wait(JobPod::Unschedulable { .. }, Some(d)) if d.is_zero()),
            "the first sighting starts the streak"
        );
        let just_under = 5 + UNSCHEDULABLE_GRACE.as_secs() - 1;
        assert!(matches!(
            step(&mut clock, just_under),
            WaitStep::Wait(JobPod::Unschedulable { .. }, Some(_))
        ));
        match step(&mut clock, 5 + UNSCHEDULABLE_GRACE.as_secs()) {
            WaitStep::Unschedulable {
                message,
                cause,
                requests,
                for_,
                failed,
                last_failure,
            } => {
                assert_eq!(message, NO_ROOM);
                assert_eq!(cause, Unplaced::NoRoom);
                assert_eq!(failed, 0);
                assert_eq!(last_failure, None);
                assert_eq!(for_, UNSCHEDULABLE_GRACE);
                assert_eq!(requests.as_deref(), Some("256Mi of memory and 100m of CPU"));
            }
            other => panic!("expected the give-up, got {other:?}"),
        }
    }

    #[test]
    fn a_pod_placed_before_the_grace_ends_is_waited_for_as_usual() {
        let job = unfinished_job("apprafter-backup-manual-x", "job-1", None);
        let t0 = std::time::Instant::now();
        let hour = Duration::from_secs(3600);
        let mut clock = UnschedulableClock::default();
        let pending = [pending_pod("job-1", "pod-1")];
        let mut placed = pending_pod("job-1", "pod-1");
        placed["spec"]["nodeName"] = json!("node-1");
        placed["status"] = json!({"phase": "Running"});
        let placed = [placed];
        let at = |s: u64| t0 + Duration::from_secs(s);
        wait_step(
            Some(&job),
            &pending,
            &[],
            &mut clock,
            at(0),
            Duration::ZERO,
            hour,
        );
        wait_step(
            Some(&job),
            &pending,
            &[],
            &mut clock,
            at(100),
            Duration::from_secs(100),
            hour,
        );
        assert_eq!(
            wait_step(
                Some(&job),
                &placed,
                &[],
                &mut clock,
                at(110),
                Duration::from_secs(110),
                hour
            ),
            WaitStep::Wait(JobPod::Running, None)
        );
        // Long past the grace since the first sighting: nothing gives up on
        // a runner that is running.
        assert_eq!(
            wait_step(
                Some(&job),
                &placed,
                &[],
                &mut clock,
                at(900),
                Duration::from_secs(900),
                hour
            ),
            WaitStep::Wait(JobPod::Running, None)
        );
    }

    #[test]
    fn the_jobs_verdict_comes_before_its_pods_and_the_reason_before_the_timeout() {
        let t0 = std::time::Instant::now();
        let s = Duration::from_secs;
        let pods = [pending_pod("job-1", "pod-1")];

        // Gone.
        let mut clock = UnschedulableClock::default();
        assert_eq!(
            wait_step(None, &pods, &[], &mut clock, t0, s(0), s(3600)),
            WaitStep::Vanished
        );

        // Finished: whatever a pod still says, the Job's condition wins.
        let mut done = unfinished_job("j", "job-1", None);
        done["status"]["conditions"] = json!([{"type": "Complete", "status": "True"}]);
        assert_eq!(
            wait_step(Some(&done), &pods, &[], &mut clock, t0, s(0), s(3600)),
            WaitStep::Succeeded
        );
        let mut failed = unfinished_job("j", "job-1", None);
        failed["status"]["conditions"] =
            json!([{"type": "Failed", "status": "True", "reason": "DeadlineExceeded"}]);
        assert_eq!(
            wait_step(Some(&failed), &pods, &[], &mut clock, t0, s(0), s(3600)),
            WaitStep::Failed("DeadlineExceeded".to_string())
        );

        // The grace and the timeout end on the same look: the known reason
        // is what is reported, not "no longer waiting".
        let job = unfinished_job("j", "job-1", None);
        let mut clock = UnschedulableClock::default();
        wait_step(Some(&job), &pods, &[], &mut clock, t0, s(0), s(120));
        assert!(matches!(
            wait_step(
                Some(&job),
                &pods,
                &[],
                &mut clock,
                t0 + s(120),
                s(120),
                s(120)
            ),
            WaitStep::Unschedulable { .. }
        ));

        // A timeout shorter than the grace ends the wait first, and says the
        // pod never started rather than that it is running.
        let mut clock = UnschedulableClock::default();
        wait_step(Some(&job), &pods, &[], &mut clock, t0, s(0), s(60));
        let step = wait_step(Some(&job), &pods, &[], &mut clock, t0 + s(60), s(60), s(60));
        let WaitStep::TimedOut(pod) = step else {
            panic!("expected the timeout, got {step:?}");
        };
        let note = job_pod::timeout_note(&pod, 1, PLATFORMSTACK_NAMESPACE, "j");
        assert!(note.contains("could not be scheduled in 1m"), "{note}");
    }

    #[test]
    fn the_wait_says_a_job_between_attempts_is_retrying_not_still_running() {
        let mut job = unfinished_job("j", "job-1", None);
        job["status"] = json!({"failed": 1, "startTime": "2026-09-23T14:39:47Z"});
        let t0 = std::time::Instant::now();
        let mut clock = UnschedulableClock::default();
        let step = wait_step(
            Some(&job),
            &[],
            &[],
            &mut clock,
            t0,
            Duration::from_secs(40),
            Duration::from_secs(3600),
        );
        assert_eq!(
            step,
            WaitStep::Wait(
                JobPod::Retrying {
                    failed: 1,
                    attempts: 7
                },
                None
            )
        );
    }

    /// A timeout reached while the Job retries is an error that says why its
    /// last attempt failed: that Job has taken no backup. The finding: an
    /// over-limit staging failed every attempt, `backup run` said only
    /// "retrying after N failed attempts" and, at its timeout, exited 0.
    /// A timeout with no attempt failed stays a plain note: that backup is
    /// only slow.
    #[test]
    fn a_timeout_after_failed_attempts_is_an_error_and_a_slow_backup_is_not() {
        let t0 = std::time::Instant::now();
        let s = Duration::from_secs;
        let mut retrying = unfinished_job("j", "job-1", None);
        retrying["status"] = json!({"failed": 2, "startTime": "2026-09-23T14:39:47Z"});
        let mut clock = UnschedulableClock::default();
        let step = wait_step(Some(&retrying), &[], &[], &mut clock, t0, s(3600), s(3600));
        assert!(
            matches!(step, WaitStep::TimedOut(JobPod::Retrying { failed: 2, .. })),
            "{step:?}"
        );
        let failures = unfinished_failures(Some(&retrying), &step);
        assert_eq!(failures, 2);
        let err = timed_out("j", 60, failures, Some("the staging volume held 318Mi"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("backup Job j has taken no backup"), "{err}");
        assert!(err.contains("2 failed attempts"), "{err}");
        assert!(err.contains("the staging volume held 318Mi"), "{err}");

        // A third attempt running when the wait ends: two have failed, and
        // none has succeeded.
        let mut third = retrying.clone();
        third["status"]["active"] = json!(1);
        let step = wait_step(Some(&third), &[], &[], &mut clock, t0, s(3600), s(3600));
        assert!(matches!(step, WaitStep::TimedOut(_)), "{step:?}");
        assert!(timed_out("j", 60, unfinished_failures(Some(&third), &step), None).is_err());

        // Slow, with nothing failed: the wait ends quietly, as it always has.
        let slow = unfinished_job("j", "job-1", None);
        let step = wait_step(Some(&slow), &[], &[], &mut clock, t0, s(3600), s(3600));
        assert!(matches!(step, WaitStep::TimedOut(_)), "{step:?}");
        assert_eq!(unfinished_failures(Some(&slow), &step), 0);
        assert!(timed_out("j", 60, 0, None).is_ok());

        // A finished Job reports its own ending, not its attempts.
        let mut failed = retrying;
        failed["status"]["conditions"] =
            json!([{"type": "Failed", "status": "True", "reason": "PodFailurePolicy"}]);
        let step = wait_step(Some(&failed), &[], &[], &mut clock, t0, s(0), s(3600));
        assert!(matches!(step, WaitStep::Failed(_)), "{step:?}");
        assert_eq!(unfinished_failures(Some(&failed), &step), 0);
    }

    /// A node under memory pressure: the kubelet taints it, and keeps the
    /// taint for five minutes after the pressure ends.
    const MEMORY_PRESSURE: &str = "0/1 nodes are available: 1 node(s) had untolerated taint \
                                   {node.kubernetes.io/memory-pressure: }. preemption: 0/1 nodes \
                                   are available: 1 Preemption is not helpful for scheduling.";

    fn pending_pod_saying(job_uid: &str, pod_uid: &str, message: &str) -> Value {
        let mut p = pending_pod(job_uid, pod_uid);
        p["status"]["conditions"][0]["message"] = json!(message);
        p
    }

    #[test]
    fn a_pod_kept_off_by_a_node_condition_is_waited_for_past_the_taints_five_minutes() {
        // A runner evicted under memory pressure is retried into the
        // pressure taint, which outlives the pressure by five minutes. At
        // two minutes that Job would still have run.
        let job = unfinished_job("j", "job-1", None);
        let pods = [pending_pod_saying("job-1", "pod-1", MEMORY_PRESSURE)];
        let t0 = std::time::Instant::now();
        let s = Duration::from_secs;
        let mut clock = UnschedulableClock::default();
        for at in [0, 125, 300, 599] {
            assert!(
                matches!(
                    wait_step(
                        Some(&job),
                        &pods,
                        &[],
                        &mut clock,
                        t0 + s(at),
                        s(at),
                        s(3600)
                    ),
                    WaitStep::Wait(JobPod::Unschedulable { .. }, Some(_))
                ),
                "at {at}s"
            );
        }
        match wait_step(
            Some(&job),
            &pods,
            &[],
            &mut clock,
            t0 + s(600),
            s(600),
            s(3600),
        ) {
            WaitStep::Unschedulable { cause, for_, .. } => {
                assert_eq!(
                    cause,
                    Unplaced::NodeCondition(vec!["node.kubernetes.io/memory-pressure".to_string()])
                );
                assert_eq!(for_, s(600));
            }
            other => panic!("expected the give-up at ten minutes, got {other:?}"),
        }
    }

    #[test]
    fn the_clock_does_not_run_while_pods_are_stopping_and_giving_room_back() {
        // A CNPG pod being deleted shuts down smartly for up to 180 s, and
        // holds its requests until it is gone: longer than the grace. The
        // room it gives back may be exactly what the runner waits for.
        let job = unfinished_job("j", "job-1", None);
        let pods = [pending_pod("job-1", "pod-1")];
        let stopping = ["demo/shop-pg-1".to_string()];
        let t0 = std::time::Instant::now();
        let s = Duration::from_secs;
        let mut clock = UnschedulableClock::default();
        // No room and nothing stopping yet: the clock starts.
        assert!(matches!(
            wait_step(Some(&job), &pods, &[], &mut clock, t0, s(0), s(3600)),
            WaitStep::Wait(JobPod::Unschedulable { .. }, Some(d)) if d.is_zero()
        ));
        for at in [100, 200, 300] {
            match wait_step(
                Some(&job),
                &pods,
                &stopping,
                &mut clock,
                t0 + s(at),
                s(at),
                s(3600),
            ) {
                WaitStep::RoomReturning {
                    stopping: named, ..
                } => {
                    assert_eq!(named, stopping.to_vec(), "at {at}s")
                }
                other => panic!("at {at}s expected to wait for the stopping pod, got {other:?}"),
            }
        }
        // Gone, and still no room: the two minutes start again now, not at
        // 0, where they first began.
        assert!(matches!(
            wait_step(Some(&job), &pods, &[], &mut clock, t0 + s(305), s(305), s(3600)),
            WaitStep::Wait(JobPod::Unschedulable { .. }, Some(d)) if d.is_zero()
        ));
        assert!(matches!(
            wait_step(
                Some(&job),
                &pods,
                &[],
                &mut clock,
                t0 + s(424),
                s(424),
                s(3600)
            ),
            WaitStep::Wait(JobPod::Unschedulable { .. }, Some(_))
        ));
        assert!(matches!(
            wait_step(
                Some(&job),
                &pods,
                &[],
                &mut clock,
                t0 + s(425),
                s(425),
                s(3600)
            ),
            WaitStep::Unschedulable { .. }
        ));
        // The caller's timeout still ends a wait for stopping pods.
        let mut clock = UnschedulableClock::default();
        assert!(matches!(
            wait_step(
                Some(&job),
                &pods,
                &stopping,
                &mut clock,
                t0,
                s(3600),
                s(3600)
            ),
            WaitStep::TimedOut(JobPod::Unschedulable { .. })
        ));
        // A node condition is not a lack of room: pods stopping elsewhere
        // do not change it, and its own clock runs.
        let tainted = [pending_pod_saying("job-1", "pod-1", MEMORY_PRESSURE)];
        let mut clock = UnschedulableClock::default();
        wait_step(
            Some(&job),
            &tainted,
            &stopping,
            &mut clock,
            t0,
            s(0),
            s(3600),
        );
        assert!(matches!(
            wait_step(
                Some(&job),
                &tainted,
                &stopping,
                &mut clock,
                t0 + s(600),
                s(600),
                s(3600)
            ),
            WaitStep::Unschedulable { .. }
        ));
    }

    #[test]
    fn the_give_up_carries_the_attempts_that_failed_before_it() {
        let mut job = unfinished_job("j", "job-1", None);
        job["status"]["failed"] = json!(1);
        let mut evicted = pending_pod("job-1", "pod-0");
        evicted["metadata"]["creationTimestamp"] = json!("2026-09-23T14:30:00Z");
        evicted["spec"]["nodeName"] = json!("node-1");
        evicted["status"] = json!({"phase": "Failed", "reason": "Evicted",
                                   "message": "The node was low on resource: memory."});
        let pods = [evicted, pending_pod("job-1", "pod-1")];
        let t0 = std::time::Instant::now();
        let s = Duration::from_secs;
        let mut clock = UnschedulableClock::default();
        wait_step(Some(&job), &pods, &[], &mut clock, t0, s(0), s(3600));
        match wait_step(
            Some(&job),
            &pods,
            &[],
            &mut clock,
            t0 + s(120),
            s(120),
            s(3600),
        ) {
            WaitStep::Unschedulable {
                failed,
                last_failure,
                ..
            } => {
                assert_eq!(failed, 1);
                assert_eq!(
                    last_failure.as_deref(),
                    Some("Evicted: The node was low on resource: memory.")
                );
            }
            other => panic!("expected the give-up, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // `backup enable` whose first backup does not complete
    // ------------------------------------------------------------------

    #[test]
    fn an_enable_whose_first_backup_cannot_start_says_backup_is_enabled() {
        // The PlatformStack patch is applied before the first backup runs.
        // A reader of a bare exit 1 re-runs `enable`; the words must say
        // that is not needed, in the line printed and in the error's help.
        let err = CliError::BackupRunnerUnschedulable {
            job: "apprafter-backup-manual-x".to_string(),
            what: "never started: no node had room for its pod for 2m 3s".to_string(),
            help: "`apprafter top` shows how much of each node is requested.".to_string(),
        };
        let (line, exit) = first_backup_outcome(err);
        assert!(line.contains("Backup IS enabled"), "{line}");
        assert!(line.contains("could not start"), "{line}");
        match exit {
            Some(CliError::BackupRunnerUnschedulable { job, what, help }) => {
                assert_eq!(job, "apprafter-backup-manual-x");
                assert_eq!(
                    what,
                    "never started: no node had room for its pod for 2m 3s"
                );
                assert!(help.starts_with("Backup IS enabled"), "{help}");
                assert!(
                    help.contains("do not run `apprafter backup enable` again"),
                    "{help}"
                );
                assert!(
                    help.ends_with("`apprafter top` shows how much of each node is requested."),
                    "the command's own advice is kept: {help}"
                );
            }
            other => panic!("the exit stays an error, on purpose: {other:?}"),
        }
    }

    #[test]
    fn enable_exits_with_its_first_backups_error_and_zero_when_it_completed() {
        assert!(take_first_backup(|| Ok(())).is_ok());
        let unschedulable = take_first_backup(|| {
            Err(CliError::BackupRunnerUnschedulable {
                job: "j".into(),
                what: "never started".into(),
                help: "h".into(),
            })
        });
        match unschedulable {
            Err(CliError::BackupRunnerUnschedulable { help, .. }) => {
                assert!(help.starts_with("Backup IS enabled"), "{help}")
            }
            other => panic!("expected the error with enable's help, got {other:?}"),
        }
        assert!(matches!(
            take_first_backup(|| Err(CliError::Other("failed".into()))),
            Err(CliError::Other(_))
        ));
    }

    #[test]
    fn an_enable_whose_first_backup_failed_says_backup_is_enabled_and_keeps_the_error() {
        let (line, exit) = first_backup_outcome(CliError::Other(
            "backup Job apprafter-backup-manual-x failed after 12s: BackoffLimitExceeded".into(),
        ));
        assert!(line.contains("Backup IS enabled"), "{line}");
        assert!(line.contains("did not complete"), "{line}");
        match exit {
            Some(CliError::Other(m)) => assert!(m.contains("BackoffLimitExceeded"), "{m}"),
            other => panic!("the error is passed on unchanged: {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // One run at a time: `backup run` beside a Job that has not finished
    // ------------------------------------------------------------------

    fn running_pod_of(job_uid: &str, pod_uid: &str) -> Value {
        let mut p = pending_pod(job_uid, pod_uid);
        p["spec"]["nodeName"] = json!("node-1");
        p["status"] = json!({"phase": "Running", "conditions": [
            {"type": "PodScheduled", "status": "True"}
        ]});
        p
    }

    fn with_start(mut j: Value, start: &str) -> Value {
        j["status"]["startTime"] = json!(start);
        j
    }

    #[test]
    fn only_backup_and_check_jobs_that_have_not_finished_are_active() {
        let running = with_start(
            unfinished_job("apprafter-backup-check-29312350", "chk", Some("CronJob")),
            "2026-09-23T06:00:00Z",
        );
        let pending = with_start(
            unfinished_job("apprafter-backup-29312345", "bk", Some("CronJob")),
            "2026-09-23T03:00:00Z",
        );
        let mut done = unfinished_job("apprafter-backup-29312300", "done", Some("CronJob"));
        done["status"] = json!({"succeeded": 1, "conditions": [
            {"type": "Complete", "status": "True"}
        ]});
        let mut failed = unfinished_job("apprafter-backup-29312301", "failed", Some("CronJob"));
        failed["status"] = json!({"failed": 7, "conditions": [
            {"type": "Failed", "status": "True", "reason": "BackoffLimitExceeded"}
        ]});
        // Succeeded, and the Complete condition not written yet.
        let mut succeeded = unfinished_job("apprafter-backup-29312302", "ok", None);
        succeeded["status"] = json!({"succeeded": 1});
        let mut deleting = unfinished_job("apprafter-backup-manual-x", "del", None);
        deleting["metadata"]["deletionTimestamp"] = json!("2026-09-23T07:00:00Z");
        let other = unfinished_job("nightly-report", "rep", None);
        let pods = [
            running_pod_of("chk", "chk-pod"),
            pending_pod("bk", "bk-pod"),
        ];
        let active = active_runner_jobs(
            &[done, failed, succeeded, deleting, other, pending, running],
            &pods,
        );
        let names: Vec<&str> = active.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "apprafter-backup-check-29312350",
                "apprafter-backup-29312345"
            ],
            "newest first"
        );
        assert_eq!(active[0].state, "Running");
        assert_eq!(active[0].pod, JobPod::Running);
        assert!(
            active[1]
                .state
                .starts_with("Pending, cannot be scheduled: 0/1 nodes are available"),
            "{}",
            active[1].state
        );
    }

    #[test]
    fn a_job_the_controller_is_failing_is_active_and_says_it_is_stopping() {
        // FailureTarget comes first, while its pods stop (the runner takes
        // up to 90 s); Failed follows once they are gone.
        let mut stopping = unfinished_job("apprafter-backup-29312345", "bk", Some("CronJob"));
        stopping["status"] = json!({"active": 1, "startTime": "2026-09-23T03:00:00Z",
            "conditions": [{"type": "FailureTarget", "status": "True",
                            "reason": "DeadlineExceeded"}]});
        let active = active_runner_jobs(&[stopping], &[]);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].state, "Stopping (DeadlineExceeded)");
    }

    #[test]
    fn a_run_is_refused_beside_a_job_that_has_not_finished() {
        assert!(refusal(&[], &[]).is_none());
        let mut done = unfinished_job("apprafter-backup-29312300", "done", Some("CronJob"));
        done["status"]["conditions"] = json!([{"type": "Complete", "status": "True"}]);
        assert!(refusal(&[done], &[]).is_none());

        // The weekly check is running: a backup started now would fail on
        // its exclusive lock, and on a full node it would not even start.
        let check = unfinished_job("apprafter-backup-check-29312350", "chk", Some("CronJob"));
        let (report, err) = refusal(
            std::slice::from_ref(&check),
            &[running_pod_of("chk", "chk-pod")],
        )
        .expect("refused");
        assert!(
            report.contains("  ✗ apprafter-backup-check-29312350 has not finished: Running"),
            "{report}"
        );
        assert!(report.contains("would not finish"), "{report}");
        assert!(
            !report.contains("delete job"),
            "a running Job is waited for: {report}"
        );
        match &err {
            CliError::BackupJobActive { job } => {
                assert_eq!(job, "apprafter-backup-check-29312350")
            }
            other => panic!("expected BackupJobActive, got {other:?}"),
        }

        // A scheduled Job no node takes holds the schedule: say why, and how
        // to clear it, since it may hold on until its deadline.
        let stuck = unfinished_job("apprafter-backup-29312345", "bk", Some("CronJob"));
        let (report, _) =
            refusal(std::slice::from_ref(&stuck), &[pending_pod("bk", "bk-pod")]).expect("refused");
        assert!(
            report.contains(
                "  ✗ apprafter-backup-29312345 has not finished: Pending, cannot be scheduled:"
            ),
            "{report}"
        );
        assert!(report.contains("`apprafter top`"), "{report}");
        assert!(
            report.contains("kubectl -n apprafter-system delete job apprafter-backup-29312345"),
            "{report}"
        );
    }

    #[test]
    fn the_jobs_holding_room_are_the_other_runners_that_are_running() {
        let own = unfinished_job("apprafter-backup-manual-x", "own", None);
        let check = unfinished_job("apprafter-backup-check-29312350", "chk", Some("CronJob"));
        let waiting = unfinished_job("apprafter-backup-29312345", "bk", Some("CronJob"));
        let pods = [
            pending_pod("own", "own-pod"),
            running_pod_of("chk", "chk-pod"),
            pending_pod("bk", "bk-pod"),
        ];
        assert_eq!(
            room_holders("apprafter-backup-manual-x", &[own, check, waiting], &pods),
            vec!["apprafter-backup-check-29312350".to_string()]
        );
    }

    #[test]
    fn an_enable_beside_a_job_that_has_not_finished_skips_its_first_backup_and_exits_zero() {
        let (line, exit) = first_backup_outcome(CliError::BackupJobActive {
            job: "apprafter-backup-29312345".into(),
        });
        assert!(line.contains("Backup IS enabled"), "{line}");
        assert!(line.contains("apprafter-backup-29312345"), "{line}");
        assert!(line.contains("`apprafter backup run`"), "{line}");
        assert!(exit.is_none(), "nothing failed: {exit:?}");
        assert!(take_first_backup(|| Err(CliError::BackupJobActive {
            job: "apprafter-backup-29312345".into(),
        }))
        .is_ok());
    }

    #[test]
    fn preemption_never_runs_the_unschedulable_clock() {
        // The scheduler is evicting lower-priority pods to make room: the pod
        // will be placed. Only the caller's timeout ends that wait.
        let job = unfinished_job("j", "job-1", None);
        let mut pod = pending_pod("job-1", "pod-1");
        pod["status"]["nominatedNodeName"] = json!("node-1");
        let pods = [pod];
        let t0 = std::time::Instant::now();
        let s = Duration::from_secs;
        let mut clock = UnschedulableClock::default();
        for at in [0, 60, 120, 600, 1800] {
            assert!(matches!(
                wait_step(
                    Some(&job),
                    &pods,
                    &[],
                    &mut clock,
                    t0 + s(at),
                    s(at),
                    s(3600)
                ),
                WaitStep::Wait(JobPod::Preempting { .. }, None)
            ));
        }
        assert!(matches!(
            wait_step(
                Some(&job),
                &pods,
                &[],
                &mut clock,
                t0 + s(3600),
                s(3600),
                s(3600)
            ),
            WaitStep::TimedOut(JobPod::Preempting { .. })
        ));
    }
}
