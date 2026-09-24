// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Pure env-config parser for the in-cluster backup runner.
//!
//! [`RunnerConfig::from_env_map`] is a pure function that takes a
//! `BTreeMap<String, String>` (testable without touching the real process env).
//! [`RunnerConfig::from_env`] is the thin impure wrapper used by `main`.

use std::collections::BTreeMap;
use std::time::Duration;

use cli_core::{CliError, Result};

/// The restic `--host` a run uses when the cluster carries no
/// `spec.backup.clusterName`. Fixed rather than the pod name, which is
/// ephemeral (spec §Retention M-r3-1a), and unchanged from what every
/// pre-`clusterName` cluster has been writing. Defined in the engine,
/// because `apprafter backup create` stamps the same one.
pub use backup_core::engine::DEFAULT_BACKUP_HOST;

/// Who enforces retention: `spec.backup.retention.enforce`, which the chart
/// renders into `APPRAFTER_BACKUP_ENFORCE` for both CronJobs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enforce {
    /// The weekly check Job prunes after a check that passed, as far as the
    /// cluster's key may delete (the platform default).
    Check,
    /// The backup Job prunes after every backup; a prune that fails fails the
    /// backup.
    Cluster,
    /// Nothing in the cluster prunes: retention is `apprafter backup prune`,
    /// run outside it with full credentials.
    Operator,
}

impl Enforce {
    /// The value as the chart and the status record spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Enforce::Check => "check",
            Enforce::Cluster => "cluster",
            Enforce::Operator => "operator",
        }
    }

    /// Read `APPRAFTER_BACKUP_ENFORCE`. The runner deletes nothing unless it
    /// is told to: an absent variable is `operator`, and so is a value this
    /// runner does not know, with a warning — the CRD's enum allows none,
    /// and a backup must not fail over a retention setting.
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("check") => Enforce::Check,
            Some("cluster") => Enforce::Cluster,
            None | Some("operator") => Enforce::Operator,
            Some(other) => {
                eprintln!(
                    "warning: APPRAFTER_BACKUP_ENFORCE={other:?} is not check, cluster or \
                     operator; this run prunes nothing"
                );
                Enforce::Operator
            }
        }
    }
}

/// Full resolved configuration for one backup runner invocation.
pub struct RunnerConfig {
    /// Restic repository URL, e.g. `s3:https://endpoint/bucket`.
    pub repo: String,
    /// HUMAN cluster label, stamped into the backup manifest's `clusterId` and
    /// the failure-webhook payload. `spec.backup.clusterName` when the operator
    /// set one, else the Helm release name. NOT the snapshot's identity — that
    /// is the `kube-system` UID the runner reads at run time (E1).
    pub cluster_id: String,
    /// restic `--host` for every snapshot of the run: the human cluster name,
    /// so a listing is legible and groupable. Defaults to the fixed
    /// `apprafter-backup` on a cluster that has not been named.
    pub backup_host: String,
    /// Restic repository passphrase (from `RESTIC_PASSWORD`).
    pub passphrase: String,
    /// Whether to run claims sequentially or as a single monolithic snapshot.
    pub staging_mode: backup_core::StagingMode,
    /// Who enforces retention. The backup run prunes after the backup only
    /// under [`Enforce::Cluster`]; the check run prunes after a passing check
    /// only under [`Enforce::Check`].
    pub enforce: Enforce,
    /// How many daily/weekly/monthly run representatives to retain, and the
    /// zone those days are counted in (`APPRAFTER_BACKUP_TIME_ZONE`, the
    /// schedules' own).
    pub retention: backup_core::prune::RetentionPolicy,
    /// Optional URL to POST on backup failure.
    pub failure_webhook: Option<String>,
    /// The run's deadline: the Job's `activeDeadlineSeconds`, which the chart
    /// renders into `APPRAFTER_BACKUP_DEADLINE_SECONDS` from the same value.
    /// Kubernetes stops the run there with SIGTERM; the runner uses it to say
    /// so, and keeps its helper pods alive at least this long
    /// ([`Self::helper_keep_alive`]). `None` when the variable is absent — a
    /// Job template older than it.
    pub deadline: Option<Duration>,
    /// The BACKUP Job's deadline, whichever Job this is:
    /// `APPRAFTER_BACKUP_RUN_DEADLINE_SECONDS`, which the chart renders into
    /// both CronJobs from the backup Job's `activeDeadlineSeconds`. In the
    /// backup Job it is [`Self::deadline`]; in the check Job `deadline` is
    /// the check's own. A prune waits it out before it sweeps a run with no
    /// manifest ([`Self::prune_run_deadline`]). `None` when the variable is
    /// absent — a Job template older than it.
    pub backup_run_deadline: Option<Duration>,
    /// The staging volume's size limit in bytes: `spec.backup.stagingSizeLimit`,
    /// which the chart renders into the volume's `sizeLimit` and, from the
    /// same value, into `APPRAFTER_BACKUP_STAGING_SIZE_LIMIT`. The runner
    /// stops a run whose staging passes it ([`crate::staging`]). `None` when
    /// the variable is absent (a Job template older than it) or holds a
    /// quantity this parser does not read; the kubelet's eviction is then the
    /// only bound, as it was before.
    pub staging_limit: Option<u64>,
    /// How much data the check run's `restic check` reads:
    /// `APPRAFTER_BACKUP_CHECK_READ_DATA` (`true` = every pack) and
    /// `APPRAFTER_BACKUP_CHECK_READ_DATA_SUBSET` (`10%`, `n/t`, a size), from
    /// `checkReadData` / `checkReadDataSubset`. Both absent is structure
    /// only; the chart always renders the subset.
    pub check_depth: backup_core::restic::CheckDepth,
}

impl RunnerConfig {
    /// Build a [`RunnerConfig`] from an explicit env map (pure, testable).
    ///
    /// Required keys: `APPRAFTER_BACKUP_REPO`, `APPRAFTER_CLUSTER_ID`,
    /// `RESTIC_PASSWORD`.  All others are optional with documented defaults.
    pub fn from_env_map(e: &BTreeMap<String, String>) -> Result<Self> {
        let repo = require(e, "APPRAFTER_BACKUP_REPO")?;
        let cluster_id = require(e, "APPRAFTER_CLUSTER_ID")?;
        let passphrase = require(e, "RESTIC_PASSWORD")?;

        // Optional and defaulted rather than required: a cluster whose chart
        // predates `clusterName` keeps the fixed host it has always used, so
        // upgrading the runner never silently re-groups an existing repository.
        let backup_host = e
            .get("APPRAFTER_BACKUP_HOST")
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| DEFAULT_BACKUP_HOST.to_string());

        let staging_mode =
            if e.get("APPRAFTER_BACKUP_STAGING_MODE").map(|s| s.as_str()) == Some("sequential") {
                backup_core::StagingMode::Sequential
            } else {
                backup_core::StagingMode::Monolithic
            };

        let enforce = Enforce::parse(e.get("APPRAFTER_BACKUP_ENFORCE").map(String::as_str));

        let mut retention = backup_core::prune::RetentionPolicy::default();
        if let Some(v) = e.get("APPRAFTER_BACKUP_KEEP_DAILY") {
            retention.keep_daily = parse_u32(v, "APPRAFTER_BACKUP_KEEP_DAILY")?;
        }
        if let Some(v) = e.get("APPRAFTER_BACKUP_KEEP_WEEKLY") {
            retention.keep_weekly = parse_u32(v, "APPRAFTER_BACKUP_KEEP_WEEKLY")?;
        }
        if let Some(v) = e.get("APPRAFTER_BACKUP_KEEP_MONTHLY") {
            retention.keep_monthly = parse_u32(v, "APPRAFTER_BACKUP_KEEP_MONTHLY")?;
        }
        // The zone the schedules run in (`spec.backup.timeZone`), which the
        // chart renders here from the value it gives both CronJobs'
        // `timeZone`: the keep policy counts its days in it, as the CLI's
        // `backup prune` does. A name the zone database does not know is a
        // warning and UTC — it moves no more than which run is the newest of
        // its day, and a backup must not fail over it.
        retention.zone = backup_core::prune::policy_zone(
            e.get("APPRAFTER_BACKUP_TIME_ZONE")
                .map(String::as_str)
                .unwrap_or(""),
        )
        .unwrap_or_else(|why| {
            eprintln!("warning: APPRAFTER_BACKUP_TIME_ZONE: {why}");
            backup_core::prune::Tz::UTC
        });

        let failure_webhook = e
            .get("APPRAFTER_BACKUP_FAILURE_WEBHOOK")
            .filter(|s| !s.is_empty())
            .cloned();

        let deadline = optional_deadline(e, "APPRAFTER_BACKUP_DEADLINE_SECONDS")?;
        let backup_run_deadline = optional_deadline(e, "APPRAFTER_BACKUP_RUN_DEADLINE_SECONDS")?;

        let staging_limit = e
            .get("APPRAFTER_BACKUP_STAGING_SIZE_LIMIT")
            .and_then(|v| parse_staging_limit(v));

        let read_data = match e
            .get("APPRAFTER_BACKUP_CHECK_READ_DATA")
            .map(String::as_str)
        {
            None | Some("") | Some("false") => false,
            Some("true") => true,
            Some(other) => {
                return Err(CliError::Other(format!(
                    "env APPRAFTER_BACKUP_CHECK_READ_DATA={other:?} is not true or false"
                )))
            }
        };
        let check_depth = backup_core::restic::CheckDepth::from_knobs(
            read_data,
            e.get("APPRAFTER_BACKUP_CHECK_READ_DATA_SUBSET")
                .map(String::as_str)
                .unwrap_or(""),
        );

        Ok(RunnerConfig {
            repo,
            cluster_id,
            backup_host,
            passphrase,
            staging_mode,
            enforce,
            retention,
            failure_webhook,
            deadline,
            backup_run_deadline,
            staging_limit,
            check_depth,
        })
    }

    /// Build a [`RunnerConfig`] from the real process environment.
    pub fn from_env() -> Result<Self> {
        let map: BTreeMap<String, String> = std::env::vars().collect();
        Self::from_env_map(&map)
    }

    /// The backup run deadline a prune in either Job waits out before it
    /// sweeps a run with no manifest
    /// ([`backup_core::prune::unfinished_run_window`]):
    /// [`Self::backup_run_deadline`], else the chart's default.
    pub fn prune_run_deadline(&self) -> Duration {
        self.backup_run_deadline
            .unwrap_or(backup_core::helper_pod::DEFAULT_RUN_DEADLINE)
    }

    /// How long this run's helper pods keep themselves alive: the rule the
    /// CLI's follow too (`backup_core::helper_pod::helper_keep_alive`) — the
    /// deadline, never less than six hours; the chart's default deadline when
    /// the variable is absent. One rule on both sides keeps one spec per
    /// helper pod name, so the runner reuses a pod the CLI left running
    /// rather than replacing it, and the other way round. The Job's deadline
    /// still stops the run first.
    pub fn helper_keep_alive(&self) -> Duration {
        backup_core::helper_pod::helper_keep_alive(
            self.deadline
                .unwrap_or(backup_core::helper_pod::DEFAULT_RUN_DEADLINE),
        )
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require(e: &BTreeMap<String, String>, key: &str) -> Result<String> {
    e.get(key)
        .cloned()
        .ok_or_else(|| CliError::Other(format!("missing required env: {key}")))
}

/// The deadline in `key`, `None` when the variable is absent ([`parse_deadline`]).
fn optional_deadline(e: &BTreeMap<String, String>, key: &str) -> Result<Option<Duration>> {
    e.get(key).map(|v| parse_deadline(v, key)).transpose()
}

/// `APPRAFTER_BACKUP_DEADLINE_SECONDS` and
/// `APPRAFTER_BACKUP_RUN_DEADLINE_SECONDS` (`key`): whole seconds, above zero.
/// The chart renders a Job's `activeDeadlineSeconds` here, which its schema
/// already holds to ten minutes or more; anything unreadable is a broken
/// render, and a precondition error says so rather than running with no
/// deadline at all.
fn parse_deadline(value: &str, key: &str) -> Result<Duration> {
    match value.parse::<u64>() {
        Ok(secs) if secs > 0 => Ok(Duration::from_secs(secs)),
        _ => Err(CliError::Other(format!(
            "env {key}={value:?} is not a whole number of seconds above zero"
        ))),
    }
}

/// `APPRAFTER_BACKUP_STAGING_SIZE_LIMIT`: a Kubernetes quantity (`10Gi`,
/// `500Mi`, `20G`) above zero, in bytes. The value already passed the
/// apiserver as the volume's `sizeLimit`, so it is a quantity; one in a form
/// [`cli_core::quantity::parse_bytes`] does not read (an exponent, `Ei`) is
/// reported and leaves the runner without its own check rather than failing
/// the run. The kubelet's eviction still bounds the volume.
fn parse_staging_limit(value: &str) -> Option<u64> {
    match cli_core::quantity::parse_bytes(value) {
        Some(bytes) if bytes > 0 => Some(bytes as u64),
        _ => {
            eprintln!(
                "warning: APPRAFTER_BACKUP_STAGING_SIZE_LIMIT={value:?} is not a size this runner \
                 reads; it does not watch the staging volume, and only the kubelet's eviction \
                 bounds it"
            );
            None
        }
    }
}

fn parse_u32(value: &str, key: &str) -> Result<u32> {
    value
        .parse::<u32>()
        .map_err(|_| CliError::Other(format!("env {key}={value:?} is not a valid u32")))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_runners_helpers_follow_the_clis_keep_alive_rule() {
        let base = [
            ("APPRAFTER_BACKUP_REPO", "s3:https://h/b"),
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "pw"),
        ];
        for (env, want) in [
            // A deadline set for a frequent schedule: the Job stops the run at
            // ten minutes, and its helpers have the CLI's six hours.
            (Some("600"), 21600),
            (Some("21600"), 21600),
            (Some("43200"), 43200),
            (None, 21600),
        ] {
            let mut pairs = base.to_vec();
            if let Some(secs) = env {
                pairs.push(("APPRAFTER_BACKUP_DEADLINE_SECONDS", secs));
            }
            let cfg = RunnerConfig::from_env_map(&map(&pairs)).unwrap();
            assert_eq!(
                cfg.helper_keep_alive(),
                Duration::from_secs(want),
                "{env:?}"
            );
        }
    }

    #[test]
    fn parses_a_full_config() {
        let e = map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:https://ep/b"),
            ("APPRAFTER_CLUSTER_ID", "c1"),
            ("APPRAFTER_BACKUP_STAGING_MODE", "sequential"),
            ("APPRAFTER_BACKUP_ENFORCE", "cluster"),
            ("APPRAFTER_BACKUP_KEEP_DAILY", "5"),
            ("RESTIC_PASSWORD", "p"),
            ("APPRAFTER_BACKUP_HOST", "prod"),
            ("APPRAFTER_BACKUP_FAILURE_WEBHOOK", "https://hook"),
            ("APPRAFTER_BACKUP_DEADLINE_SECONDS", "21600"),
            ("APPRAFTER_BACKUP_STAGING_SIZE_LIMIT", "10Gi"),
        ]);
        let c = RunnerConfig::from_env_map(&e).unwrap();
        assert_eq!(c.staging_limit, Some(10 * 1024 * 1024 * 1024));
        assert_eq!(c.repo, "s3:https://ep/b");
        assert_eq!(c.cluster_id, "c1");
        assert_eq!(c.backup_host, "prod");
        assert_eq!(c.passphrase, "p");
        assert_eq!(c.staging_mode, backup_core::StagingMode::Sequential);
        assert_eq!(c.enforce, Enforce::Cluster);
        assert_eq!(c.retention.keep_daily, 5);
        assert_eq!(c.failure_webhook.as_deref(), Some("https://hook"));
        assert_eq!(c.deadline, Some(Duration::from_secs(21600)));
    }

    #[test]
    fn defaults_when_absent() {
        let e = map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p"),
        ]);
        let c = RunnerConfig::from_env_map(&e).unwrap();
        assert_eq!(c.staging_mode, backup_core::StagingMode::Monolithic);
        // An unnamed cluster keeps the host it has always written under —
        // upgrading the runner must not re-group an existing repository.
        assert_eq!(c.backup_host, DEFAULT_BACKUP_HOST);
        // Not told to prune: prunes nothing.
        assert_eq!(c.enforce, Enforce::Operator);
        assert_eq!(c.check_depth, backup_core::restic::CheckDepth::Structure);
        assert_eq!(c.retention.keep_daily, 7); // RetentionPolicy::default
        assert_eq!(c.retention.keep_weekly, 4);
        assert_eq!(c.retention.keep_monthly, 6);
        assert_eq!(c.retention.zone, backup_core::prune::Tz::UTC);
        assert!(c.failure_webhook.is_none());
        assert!(c.deadline.is_none());
        assert!(c.staging_limit.is_none());
    }

    /// The keep policy counts its days in the zone the schedules run in,
    /// which the chart renders from `spec.backup.timeZone`; the CLI's prune
    /// reads the same field, and both parse it with `policy_zone`.
    #[test]
    fn the_keep_policy_counts_days_in_the_schedules_zone() {
        let zone_of = |value: Option<&str>| {
            let mut pairs = vec![
                ("APPRAFTER_BACKUP_REPO", "s3:x"),
                ("APPRAFTER_CLUSTER_ID", "c"),
                ("RESTIC_PASSWORD", "p"),
            ];
            if let Some(v) = value {
                pairs.push(("APPRAFTER_BACKUP_TIME_ZONE", v));
            }
            RunnerConfig::from_env_map(&map(&pairs))
                .unwrap()
                .retention
                .zone
        };
        assert_eq!(zone_of(Some("Europe/Berlin")).name(), "Europe/Berlin");
        assert_eq!(zone_of(Some("America/New_York")).name(), "America/New_York");
        assert_eq!(zone_of(None), backup_core::prune::Tz::UTC);
        assert_eq!(zone_of(Some("")), backup_core::prune::Tz::UTC);
        // A POSIX TZ rule is not a zone name: counted in UTC, with a
        // warning, and the run goes on.
        assert_eq!(
            zone_of(Some("CET-1CEST,M3.5.0,M10.5.0/3")),
            backup_core::prune::Tz::UTC
        );
    }

    #[test]
    fn the_staging_limit_reads_every_unit_the_chart_can_render() {
        for (raw, want) in [
            ("10Gi", Some(10u64 << 30)),
            ("512Mi", Some(512u64 << 20)),
            ("20G", Some(20_000_000_000u64)),
            ("1073741824", Some(1u64 << 30)),
            ("1.5Gi", Some(3u64 << 29)),
            // Not a size this runner reads: no check of its own, and the run
            // goes ahead under the kubelet's.
            ("0", None),
            ("1e9", None),
            ("", None),
        ] {
            let e = map(&[
                ("APPRAFTER_BACKUP_REPO", "s3:x"),
                ("APPRAFTER_CLUSTER_ID", "c"),
                ("RESTIC_PASSWORD", "p"),
                ("APPRAFTER_BACKUP_STAGING_SIZE_LIMIT", raw),
            ]);
            let c = RunnerConfig::from_env_map(&e)
                .unwrap_or_else(|err| panic!("{raw:?} failed the config: {err}"));
            assert_eq!(c.staging_limit, want, "{raw:?}");
        }
    }

    #[test]
    fn an_unreadable_deadline_is_a_precondition_error_not_no_deadline() {
        for bad in ["", "0", "-5", "6h", "21600.5", "abc"] {
            let e = map(&[
                ("APPRAFTER_BACKUP_REPO", "s3:x"),
                ("APPRAFTER_CLUSTER_ID", "c"),
                ("RESTIC_PASSWORD", "p"),
                ("APPRAFTER_BACKUP_DEADLINE_SECONDS", bad),
            ]);
            let err = RunnerConfig::from_env_map(&e)
                .err()
                .unwrap_or_else(|| panic!("{bad:?} must be refused"));
            assert!(
                err.to_string()
                    .contains("APPRAFTER_BACKUP_DEADLINE_SECONDS"),
                "{err}"
            );
        }
    }

    /// The backup Job's deadline reaches the check Job too, where
    /// `APPRAFTER_BACKUP_DEADLINE_SECONDS` is the check's own: the prune after
    /// the check waits it out before it sweeps a run with no manifest.
    #[test]
    fn the_prune_waits_out_the_backup_jobs_deadline_not_its_own_jobs() {
        let base = [
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p"),
            // The check Job's own deadline.
            ("APPRAFTER_BACKUP_DEADLINE_SECONDS", "43200"),
        ];
        let mut pairs = base.to_vec();
        pairs.push(("APPRAFTER_BACKUP_RUN_DEADLINE_SECONDS", "2700"));
        let c = RunnerConfig::from_env_map(&map(&pairs)).unwrap();
        assert_eq!(c.backup_run_deadline, Some(Duration::from_secs(2700)));
        assert_eq!(c.prune_run_deadline(), Duration::from_secs(2700));

        // A Job template older than the variable: the chart's default.
        let c = RunnerConfig::from_env_map(&map(&base)).unwrap();
        assert_eq!(c.backup_run_deadline, None);
        assert_eq!(
            c.prune_run_deadline(),
            backup_core::helper_pod::DEFAULT_RUN_DEADLINE
        );

        for bad in ["", "0", "6h", "abc"] {
            let mut pairs = base.to_vec();
            pairs.push(("APPRAFTER_BACKUP_RUN_DEADLINE_SECONDS", bad));
            let err = RunnerConfig::from_env_map(&map(&pairs))
                .err()
                .unwrap_or_else(|| panic!("{bad:?} must be refused"));
            assert!(
                err.to_string()
                    .contains("env APPRAFTER_BACKUP_RUN_DEADLINE_SECONDS="),
                "{err}"
            );
        }
    }

    #[test]
    fn missing_required_fields_error() {
        // no repo
        assert!(RunnerConfig::from_env_map(&map(&[
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p")
        ]))
        .is_err());
        // no cluster_id
        assert!(RunnerConfig::from_env_map(&map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("RESTIC_PASSWORD", "p")
        ]))
        .is_err());
        // no passphrase
        assert!(RunnerConfig::from_env_map(&map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c")
        ]))
        .is_err());
    }

    #[test]
    fn non_numeric_retention_is_an_error() {
        let e = map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p"),
            ("APPRAFTER_BACKUP_KEEP_DAILY", "not-a-number"),
        ]);
        assert!(RunnerConfig::from_env_map(&e).is_err());
    }

    #[test]
    fn every_retention_mode_the_chart_renders_is_read_and_nothing_else_prunes() {
        for (raw, want) in [
            (Some("check"), Enforce::Check),
            (Some("cluster"), Enforce::Cluster),
            (Some("operator"), Enforce::Operator),
            // Absent, or a value no CRD allows: the runner deletes nothing.
            (None, Enforce::Operator),
            (Some("Check"), Enforce::Operator),
            (Some(""), Enforce::Operator),
        ] {
            let mut pairs = vec![
                ("APPRAFTER_BACKUP_REPO", "s3:x"),
                ("APPRAFTER_CLUSTER_ID", "c"),
                ("RESTIC_PASSWORD", "p"),
            ];
            if let Some(v) = raw {
                pairs.push(("APPRAFTER_BACKUP_ENFORCE", v));
            }
            let c = RunnerConfig::from_env_map(&map(&pairs)).unwrap();
            assert_eq!(c.enforce, want, "{raw:?}");
            if let Some(v) = raw.filter(|v| matches!(*v, "check" | "cluster" | "operator")) {
                assert_eq!(c.enforce.as_str(), v);
            }
        }
    }

    #[test]
    fn the_check_depth_follows_the_charts_two_knobs() {
        use backup_core::restic::CheckDepth;
        for (full, subset, want) in [
            (None, Some("10%"), CheckDepth::Subset("10%".into())),
            (Some("false"), Some("10%"), CheckDepth::Subset("10%".into())),
            (Some("true"), Some("10%"), CheckDepth::Full),
            (Some("false"), Some(""), CheckDepth::Structure),
            (None, None, CheckDepth::Structure),
        ] {
            let mut pairs = vec![
                ("APPRAFTER_BACKUP_REPO", "s3:x"),
                ("APPRAFTER_CLUSTER_ID", "c"),
                ("RESTIC_PASSWORD", "p"),
            ];
            if let Some(v) = full {
                pairs.push(("APPRAFTER_BACKUP_CHECK_READ_DATA", v));
            }
            if let Some(v) = subset {
                pairs.push(("APPRAFTER_BACKUP_CHECK_READ_DATA_SUBSET", v));
            }
            let c = RunnerConfig::from_env_map(&map(&pairs)).unwrap();
            assert_eq!(c.check_depth, want, "{full:?} {subset:?}");
        }
        let err = RunnerConfig::from_env_map(&map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p"),
            ("APPRAFTER_BACKUP_CHECK_READ_DATA", "yes"),
        ]))
        .err()
        .expect("an unreadable knob is a precondition error, not a quieter check");
        assert!(
            err.to_string().contains("APPRAFTER_BACKUP_CHECK_READ_DATA"),
            "{err}"
        );
    }

    #[test]
    fn an_empty_backup_host_falls_back_to_the_fixed_default() {
        // The chart renders the env unconditionally, so an unnamed cluster
        // sends an EMPTY string rather than omitting the variable.
        let e = map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p"),
            ("APPRAFTER_BACKUP_HOST", ""),
        ]);
        let c = RunnerConfig::from_env_map(&e).unwrap();
        assert_eq!(c.backup_host, DEFAULT_BACKUP_HOST);
    }

    #[test]
    fn empty_failure_webhook_is_none() {
        let e = map(&[
            ("APPRAFTER_BACKUP_REPO", "s3:x"),
            ("APPRAFTER_CLUSTER_ID", "c"),
            ("RESTIC_PASSWORD", "p"),
            ("APPRAFTER_BACKUP_FAILURE_WEBHOOK", ""),
        ]);
        let c = RunnerConfig::from_env_map(&e).unwrap();
        assert!(c.failure_webhook.is_none());
    }
}
