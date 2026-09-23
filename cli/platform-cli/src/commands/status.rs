// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter status` — the one command that answers "is anything wrong
//! with my cluster?".
//!
//! # What this is, and what it replaced (2.23a)
//!
//! Until 2.23a this command was a skeleton from the original
//! six-subcommand clap tree: it read the local state file and printed
//! `would show status …`. The real cluster roll-up had accumulated
//! under `apprafter platform status`, which is a command named for the
//! PlatformStack. `platform` manages the platform; the health of the
//! workloads running on it is a different question, and it now has its
//! own answer. ADR 0057 carries the retraction of the paragraph that
//! decided otherwise.
//!
//! # It composes, it does not compute
//!
//! Every signal here already had a reader: the PlatformStack fetch and
//! its version/condition summary live in [`crate::commands::platform`],
//! the application roll-ups in [`crate::commands::app_rollup`], the
//! MigrationPlan rows in [`crate::commands::migration`]. This module
//! orders them and owns exactly one thing they did not need: what to
//! print when the cluster cannot be reached.
//!
//! # Degradation is the point, not a fallback
//!
//! An operator runs this command *because* something looks wrong, and a
//! cluster that has stopped answering is one of the things that can be
//! wrong. Erroring out on the first unreachable read would make the
//! command useless in exactly that case, so every cluster-side section
//! degrades to a labelled line and the local half always prints. The
//! precedent is `node status`, which labels each field `[api]` or
//! `[ssh]` by the source that answered.
//!
//! Exit status stays 0 even when sections are unhealthy. `doctor` owns
//! the "exit non-zero on FAIL" contract; a second, differently-shaped
//! failing contract on a command people put in a shell prompt is a
//! promise this does not make.

use cli_core::{CliError, Result, Tier};
use cli_state::State;
use serde_json::Value;
use tracing::debug;

use crate::commands::app_rollup::{
    print_pinned_applications, print_problem_applications, ClusterApplications,
};
use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_get_json, kubectl_get_json_cluster_wide,
};
use crate::commands::migration::pending_plan_rows;
use crate::commands::platform::{
    backup_health_lines, render_conditions_table, unhealthy_condition_rows, version_summary_line,
    PLATFORMSTACK_NAME, PLATFORMSTACK_NAMESPACE,
};
use crate::commands::state_paths::resolve_state_paths;

/// What the PlatformStack read produced. Three outcomes, not two: an
/// unreachable cluster and a cluster with no PlatformStack are
/// different problems with different next steps, and collapsing them
/// into "not found" sends an operator whose network is down to
/// re-run `cluster-bootstrap`.
pub(crate) enum PlatformRead<'a> {
    Found(&'a Value),
    /// The cluster answered and has no PlatformStack — bootstrap has
    /// not finished.
    Absent,
    /// The cluster could not be reached, or refused the read.
    Unreachable(String),
}

pub fn run() -> Result<()> {
    let resolved = resolve_state_paths(None)?;
    let state = State::load_or_default(&resolved.paths)?;
    // The target NAME, never the state. `?state` was inherited from the
    // skeleton this command replaced, where the whole struct was the
    // output; here it prints the entire store — cluster name, tier,
    // provider, the Hetzner block — above a report that says all of it
    // again, more legibly. At `debug` rather than `info` for the same
    // reason: this is a read-only command whose output IS the answer.
    debug!(target = %resolved.target_name, "status invoked");

    // The target's own configuration, for the fields the state file does
    // not carry. Best-effort: an unreadable target is not a reason to
    // withhold the rest of the report.
    let configured_tier = cli_core::target::load_target(&resolved.store, &resolved.target_name)
        .ok()
        .and_then(|t| t.config.default_tier);

    for line in target_header_lines(&resolved.target_name, &state, configured_tier.as_deref()) {
        println!("{line}");
    }

    // The local half is already on screen, so from here every failure is
    // reported in place rather than returned. `?` on any of these would
    // throw away output the reader can act on.
    let kc = match ensure_kubeconfig_tempfile() {
        Ok(kc) => kc,
        Err(e) => {
            println!();
            for line in platform_lines(PlatformRead::Unreachable(e.to_string())) {
                println!("{line}");
            }
            println!(
                "{}",
                cli_core::style::warn(
                    "  applications and migration plans not checked — no cluster connection"
                )
            );
            return Ok(());
        }
    };

    let stack = kubectl_get_json(
        "platformstack",
        Some(PLATFORMSTACK_NAME),
        Some(PLATFORMSTACK_NAMESPACE),
        kc.path(),
    );
    println!();
    let read = match &stack {
        Ok(Some(json)) => PlatformRead::Found(json),
        Ok(None) => PlatformRead::Absent,
        Err(e) => PlatformRead::Unreachable(e.to_string()),
    };
    for line in platform_lines(read) {
        println!("{line}");
    }
    let now = chrono::Utc::now();

    // Backups get their own section, off the same PlatformStack read: a
    // backup that cannot run is the failure nobody notices until the day
    // they need the backup, so it is never folded into the condition table.
    if let Ok(Some(json)) = &stack {
        println!();
        for line in backup_health_lines(json, now) {
            println!("{line}");
        }
    }

    // ONE cluster-wide application read, shared by both roll-ups, so the
    // two sections describe one instant.
    let apps = ClusterApplications::read(kc.path());
    if let Ok(items) = &apps.apps {
        print_pinned_applications(items);
    }
    print_problem_applications(apps.apps.as_deref(), apps.argo.as_deref(), &now);

    let plans = kubectl_get_json_cluster_wide("migrationplan", None, kc.path()).map(|json| {
        json.as_ref()
            .and_then(|v| v.get("items"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    });
    println!();
    for line in pending_plan_lines(plans.as_deref()) {
        println!("{line}");
    }

    Ok(())
}

/// The local half: which target this is and what the CLI last recorded
/// about its cluster. Printed unconditionally and first, because it is
/// the only part that is true even when nothing else can be read — and
/// because an operator with several targets needs to know which one the
/// rest of the output is about before reading it.
pub(crate) fn target_header_lines(
    target: &str,
    state: &State,
    configured_tier: Option<&str>,
) -> Vec<String> {
    let text = |v: Option<String>| v.unwrap_or_else(|| "(unset)".to_string());
    // The tier lives in TWO places and the state file is the one that is
    // usually empty. `State.tier` is written by `apprafter init`; a
    // cluster stood up the documented way — `target add` then `up` —
    // never goes through `init`, so its state carries no tier while its
    // target config carries `default_tier` and `platform status` prints
    // "tier 1" off the PlatformStack. Reading only the state reported
    // `(unset)` for a cluster whose tier three other surfaces knew.
    let tier = state
        .tier
        .as_ref()
        .map(Tier::to_string)
        .or_else(|| configured_tier.map(str::to_string));
    vec![
        format!("Target: {target}"),
        format!("  cluster:  {}", text(state.cluster_name.clone())),
        format!("  tier:     {}", text(tier)),
        format!("  provider: {}", text(state.provider.clone())),
    ]
}

/// The platform's own line, plus any condition a reader should look at.
///
/// Only the UNHEALTHY conditions, unlike `platform status`, which prints
/// the whole table. This surface answers "is anything wrong", and a
/// healthy condition on it is a row the reader has to scan past to find
/// out that the answer is no.
pub(crate) fn platform_lines(read: PlatformRead) -> Vec<String> {
    match read {
        PlatformRead::Unreachable(why) => vec![cli_core::style::warn(&format!(
            "Platform: could not read the cluster ({why})"
        ))],
        PlatformRead::Absent => vec![cli_core::style::warn(&format!(
            "Platform: no PlatformStack in {PLATFORMSTACK_NAMESPACE} — \
             run `apprafter cluster-bootstrap` to finish bringing the cluster up"
        ))],
        PlatformRead::Found(json) => {
            let mut lines = vec![version_summary_line(json)];
            let unhealthy = unhealthy_condition_rows(json);
            if !unhealthy.is_empty() {
                lines.push(cli_core::style::warn(&format!(
                    "  {} condition(s) not healthy:",
                    unhealthy.len()
                )));
                for row in render_conditions_table(&unhealthy).lines() {
                    lines.push(cli_core::style::warn(&format!("  {row}")));
                }
            }
            lines
        }
    }
}

/// The MigrationPlans still waiting for a human.
///
/// Speaks on a read failure, and speaks when there is nothing pending,
/// for the reason the problem roll-up does: this section is part of the
/// answer to "is anything wrong", and an absent section cannot be told
/// apart from a check that did not run.
pub(crate) fn pending_plan_lines(plans: std::result::Result<&[Value], &CliError>) -> Vec<String> {
    let plans = match plans {
        Ok(p) => p,
        Err(e) => {
            return vec![cli_core::style::warn(&format!(
                "Migration plans: could not read ({e}) — approval state unknown."
            ))]
        }
    };
    let rows = pending_plan_rows(plans);
    if rows.is_empty() {
        return vec!["Migration plans: none awaiting approval.".to_string()];
    }
    let mut lines = vec![cli_core::style::warn(&format!(
        "Migration plans awaiting approval ({}) — rollouts are paused until each is decided:",
        rows.len()
    ))];
    lines.extend(
        rows.iter()
            .map(|r| cli_core::style::warn(&format!("  {r}"))),
    );
    lines.push("  run `apprafter migration approve <name>` to let one through".to_string());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state() -> State {
        State {
            cluster_name: Some("apprafter-prod".to_string()),
            tier: Some(Tier::Solo),
            provider: Some("hetzner-cloud".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn the_header_names_the_target_the_rest_of_the_output_is_about() {
        let lines = target_header_lines("prod", &state(), None);
        assert!(lines[0].contains("prod"), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("apprafter-prod")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("hetzner-cloud")),
            "{lines:?}"
        );
    }

    #[test]
    fn the_header_renders_a_target_that_has_never_been_applied() {
        // `target add` writes a target long before `apply` writes a cluster
        // name. The header must still render — this is the state a reader is
        // in the first time they run the command.
        let lines = target_header_lines("fresh", &State::default(), None);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("(unset)")), "{lines:?}");
    }

    #[test]
    fn the_tier_falls_back_to_the_target_when_the_state_file_has_none() {
        // The live regression. `State.tier` is written by `apprafter
        // init`; a cluster stood up the documented way — `target add`
        // then `up` — never runs it, so the header read `(unset)` on a
        // cluster whose tier `target show` and `platform status` both
        // printed.
        let lines = target_header_lines("dev", &State::default(), Some("solo"));
        let tier = lines.iter().find(|l| l.contains("tier:")).unwrap();
        assert!(tier.contains("solo"), "{lines:?}");
    }

    #[test]
    fn the_state_file_still_wins_when_it_has_a_tier() {
        // The target's is a DEFAULT for the next provision; the state's
        // is what was actually provisioned. When they disagree, the
        // header must report what is running.
        let lines = target_header_lines("dev", &state(), Some("prod"));
        let tier = lines.iter().find(|l| l.contains("tier:")).unwrap();
        assert!(tier.contains("solo"), "{lines:?}");
        assert!(!tier.contains("prod"), "{lines:?}");
    }

    #[test]
    fn an_unreachable_cluster_says_so_rather_than_reporting_health() {
        // The failure this defends against: rendering "could not read" as
        // anything a reader could mistake for "nothing is wrong".
        let lines = platform_lines(PlatformRead::Unreachable("i/o timeout".to_string()));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("could not read"), "{lines:?}");
        assert!(lines[0].contains("i/o timeout"), "{lines:?}");
    }

    #[test]
    fn a_cluster_without_a_platformstack_is_told_apart_from_an_unreachable_one() {
        // Different problem, different next step. Collapsing the two sends an
        // operator whose network is down to re-run `cluster-bootstrap`.
        let lines = platform_lines(PlatformRead::Absent);
        assert!(lines[0].contains("cluster-bootstrap"), "{lines:?}");
        assert!(!lines[0].contains("could not read"), "{lines:?}");
    }

    #[test]
    fn a_healthy_platform_is_one_line() {
        let stack = json!({ "status": {
            "currentVersion": "0.2.59", "availableVersion": "0.2.59",
            "conditions": [
                { "type": "Synced", "status": "True", "reason": "Ok", "message": "" },
                { "type": "UpgradeAvailable", "status": "False", "reason": "", "message": "" },
            ]
        }});
        let lines = platform_lines(PlatformRead::Found(&stack));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("0.2.59"), "{lines:?}");
    }

    #[test]
    fn an_unhealthy_condition_reaches_the_reader() {
        let stack = json!({ "status": {
            "currentVersion": "0.2.19",
            "conditions": [
                { "type": "YankedVersion", "status": "True",
                  "reason": "Yanked", "message": "needs.redis is broken in 0.2.19" },
            ]
        }});
        let lines = platform_lines(PlatformRead::Found(&stack));
        assert!(lines.len() > 1, "{lines:?}");
        let rendered = lines.join("\n");
        assert!(rendered.contains("YankedVersion"), "{rendered}");
        assert!(rendered.contains("not healthy"), "{rendered}");
    }

    #[test]
    fn no_pending_plans_still_prints_a_line() {
        // Same asymmetry the problem roll-up documents: an absent section
        // cannot be told apart from a check that did not run, and this
        // section is part of the answer to "is anything wrong".
        let lines = pending_plan_lines(Ok(&[]));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("none awaiting approval"), "{lines:?}");
    }

    #[test]
    fn a_pending_plan_is_named_and_says_what_it_is_blocking() {
        let plans = vec![json!({
            "metadata": { "name": "web-9f2", "namespace": "demo" },
            "spec": { "risks": { "classification": "data-migration" } }
        })];
        let lines = pending_plan_lines(Ok(&plans));
        let rendered = lines.join("\n");
        assert!(rendered.contains("demo/web-9f2"), "{rendered}");
        assert!(rendered.contains("paused"), "{rendered}");
        assert!(rendered.contains("migration approve"), "{rendered}");
    }

    #[test]
    fn an_unreadable_plan_list_is_loud_not_silent() {
        let err = CliError::Other("forbidden".to_string());
        let lines = pending_plan_lines(Err(&err));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("could not read"), "{lines:?}");
        assert!(
            !lines[0].contains("none"),
            "a failed read must never render as nothing pending: {lines:?}"
        );
    }
}
