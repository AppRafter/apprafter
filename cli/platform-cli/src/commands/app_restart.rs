// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `apprafter app restart` — the explicit roll (ADR 0064).
//!
//! An environment value that references a secret renders to
//! `valueFrom.secretKeyRef`. Kubernetes reads those once at pod start
//! and does not roll a Deployment when the Secret behind them changes,
//! and nothing in the operator compensates. The platform SEES the drift
//! — `status.envConfig.digest` moves and `app status` marks the affected
//! pods `← old config` — but until this verb there was nothing that
//! ACTED on it, and the documented remedy was a raw `kubectl rollout
//! restart`.
//!
//! **The roll is explicit, never automatic.** The platform cannot know
//! where a developer's editing sequence ends: a timer firing between a
//! first and a second seal deploys a state that was never an intended
//! one. ADR 0064 §Alternatives records the automatic variant, and why it
//! is the failure mode rather than the fix.
//!
//! # What it writes, and where
//!
//! Two annotations under one dedicated field manager
//! ([`APPRAFTER_CLI_RESTART_FIELD_MANAGER`]), carrying the same
//! timestamp:
//!
//! * the **roll** goes on the rendered `Deployment`'s POD TEMPLATE. A
//!   changed pod template is what makes Kubernetes replace the pods; the
//!   apply is partial, so it owns that one annotation key and nothing
//!   else — not `replicas`, not the operator's image.
//! * the **record** goes on the AppRafter `Application` CR, the way ADR
//!   0059's pin does. The split is not stylistic: the operator's one-time
//!   selector delete-and-recreate drops the Deployment's annotations,
//!   which is harmless for a roll and fatal for a record.
//!
//! Neither write is at risk of being reverted, and both halves of that
//! were measured (ADR 0064 §Risks). The operator's server-side apply owns
//! no `f:annotations` at all — its renderer emits none — and
//! `metadata.annotations` is a granular map, so ownership is per key.
//! Argo CD does not manage the Deployment either: the operator sets an
//! `ownerReference` and never stamps a tracking label, and gitops-engine
//! excludes owned objects from the managed set. **Both exclusions stop
//! holding for a registration whose git renders a Deployment directly**,
//! which is why that case is a refusal here rather than a warning.
//!
//! # It lives in its own module
//!
//! `app.rs` is already the largest file in the workspace and holds the
//! shared addressing vocabulary the whole `app` surface reads
//! (`workload_for`, `WorkloadChoice`, the refusal renderers). This verb
//! consumes that vocabulary and adds none of its own, so it sits beside
//! `app_open.rs` — the existing precedent for one verb, one module —
//! rather than growing the file every other verb has to read.

use std::io::{self, IsTerminal};
use std::path::Path;

use cli_core::{CliError, Result};
use cli_providers::k8s::kubectl::APPRAFTER_CLI_RESTART_FIELD_MANAGER;
use serde_json::{json, Value};

use crate::commands::app::{
    resolve_app_for_command, unknown_workload_message, unplaceable_workload_message, workload_for,
    PlacedWorkload, WorkloadChoice, WorkloadDemand,
};
use crate::commands::app_open::apprafter_app_refs;
use crate::commands::k8s_helpers::{
    ensure_kubeconfig_tempfile, kubectl_apply_server_side, kubectl_get_json,
};
use crate::commands::restore::PRE_RESTORE_REPLICAS_ANNOTATION;

/// The annotation this verb writes, on both objects.
///
/// One key for both because they record the same fact at two
/// granularities — "this workload was restarted at T" — and a reader who
/// finds it on a pod template must be able to find the same string on the
/// CR without learning a second spelling.
///
/// Deliberately NOT `kubectl.kubernetes.io/restartedAt`: that key is
/// `kubectl rollout restart`'s, and reusing it would make an AppRafter
/// restart indistinguishable from a hand-run one in exactly the audit
/// that wants to tell them apart.
pub(crate) const RESTARTED_AT_ANNOTATION: &str = "apprafter.io/restarted-at";

/// The `status.phase` the operator writes while a MigrationPlan gates a
/// destructive edit (`operator_core::PHASE_AWAITING_MIGRATION_APPROVAL`).
///
/// Re-declared rather than imported: the CLI does not depend on the
/// operator crates, and this is read off a decoded CR as a plain string
/// either way. [`paused_notice_line`] is pinned against the literal so
/// the two cannot drift silently in the direction that matters — a
/// renamed phase makes the notice disappear, never makes the restart
/// wrong.
const PHASE_AWAITING_MIGRATION_APPROVAL: &str = "AwaitingMigrationApproval";

/// The condition type carried alongside that phase; its `message` names
/// the gating plan and its namespace.
const COND_MIGRATION_PENDING: &str = "MigrationPending";

/// One workload, and everything the two reads say about restarting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkloadRestart {
    pub name: String,
    pub namespace: String,
    /// The rendered Deployment's `spec.replicas`.
    ///
    /// Read off the DEPLOYMENT, not off the CR's `spec.base.replicas`.
    /// The CR's number is the declared one and has to be unified with
    /// `spec.environments[<env>]` before it means anything; the
    /// Deployment's is the resolved one, written by the operator out of
    /// whatever unification it performed, and it is the count of pods a
    /// restart actually replaces. Reimplementing the unification here to
    /// answer "how many pods" would be a second implementation of it,
    /// and the two would disagree on exactly the env-override manifests
    /// nobody tests by hand.
    pub replicas: i64,
    /// The `MigrationPending` condition's message when the workload is
    /// paused on a plan — surfaced verbatim rather than parsed, so a
    /// reworded operator message degrades to a less specific notice and
    /// never to a wrong one.
    pub paused_on: Option<String>,
    /// The raw [`PRE_RESTORE_REPLICAS_ANNOTATION`] value when a restore
    /// recorded one. Its presence means a `restore --data-only` is in
    /// flight: the annotation is written in the same patch as the
    /// scale-to-zero and removed by the resume.
    pub pre_restore_replicas: Option<String>,
}

impl WorkloadRestart {
    /// Whether restarting it would replace no pods.
    ///
    /// TWO conditions, because they are true at different moments and
    /// the window between them is exactly where a false success gets
    /// printed. `replicas == 0` is the settled state. The annotation
    /// covers the window BEFORE the operator has reconciled a
    /// scale-to-zero it has already been told about: the CR says zero,
    /// the Deployment still says three, and a restart issued there rolls
    /// pods that are about to be deleted out from under it.
    fn replaces_nothing(&self) -> bool {
        self.replicas == 0 || self.pre_restore_replicas.is_some()
    }
}

/// `apprafter app restart <application> [--workload <name>] [--env <e>]
/// [--yes]` (ADR 0064).
///
/// ADR 0062 §Addressing: `name` is the REGISTRATION. A bundle is
/// deployed, synced and removed together, so a restart of the
/// application is a restart of every workload in it; `--workload`
/// narrows to one. It prompts by default and refuses on a non-TTY
/// without `--yes` — it replaces running pods, and does not get a free
/// pass for being "safe".
pub fn restart(name: &str, env: Option<String>, yes: bool, workload: Option<String>) -> Result<()> {
    let kc = ensure_kubeconfig_tempfile()?;
    let (app, _argo_name) = resolve_app_for_command(name, env.as_deref(), kc.path())?;
    let refs = apprafter_app_refs(&app);

    let targets: Vec<PlacedWorkload> =
        match workload_for(&refs, workload.as_deref(), WorkloadDemand::WriteEvery) {
            WorkloadChoice::One(w) => vec![w],
            WorkloadChoice::Every(ws) => ws,
            WorkloadChoice::NoWorkloads => {
                return Err(CliError::Other(no_workload_refusal(name, &app)));
            }
            WorkloadChoice::Unknown { asked, available } => {
                return Err(CliError::Other(unknown_workload_message(
                    name, &asked, &available,
                )));
            }
            WorkloadChoice::Unplaceable(w) => {
                return Err(CliError::Other(unplaceable_workload_message(name, &w)));
            }
            WorkloadChoice::Ask(_) | WorkloadChoice::Refuse(_) => {
                unreachable!("WorkloadDemand::WriteEvery neither asks nor refuses")
            }
        };

    let mut planned = Vec::with_capacity(targets.len());
    for target in &targets {
        planned.push(read_workload(target, kc.path())?);
    }

    // Before the prompt, not after: `--yes` skips the prompt, and a
    // refusal a reader only sees when they did not pass `--yes` is not
    // one.
    if let Some(msg) = zero_replica_refusal(name, &planned, env.as_deref()) {
        return Err(CliError::Other(msg));
    }

    for line in paused_notice_lines(&planned) {
        println!("{}", cli_core::style::warn(&line));
    }

    if !yes {
        if !io::stdin().is_terminal() {
            return Err(CliError::Other(
                "non-interactive shell — pass `--yes` to skip the confirmation prompt".into(),
            ));
        }
        for line in restart_prompt_lines(name, &planned) {
            println!("{line}");
        }
        let confirmed = inquire::Confirm::new("Confirm?")
            .with_default(false)
            .prompt()
            .map_err(|e| CliError::Other(format!("confirmation prompt: {e}")))?;
        if !confirmed {
            println!("Cancelled.");
            return Ok(());
        }
    }

    // ONE timestamp for the whole invocation. Two workloads restarted by
    // one command were restarted together, and a per-workload clock would
    // record them as two events a few hundred milliseconds apart — which
    // is what a reader correlating a rollout with a credential rotation
    // then has to untangle.
    let at = chrono::Utc::now().to_rfc3339();
    for w in &planned {
        // The roll first. If the record write fails, a rolled workload
        // with no record is a missing audit line; the other order would
        // leave a record of a restart that never happened.
        apply(
            &restart_roll_manifest(&w.name, &w.namespace, &at),
            kc.path(),
        )?;
        apply(
            &restart_record_manifest(&w.name, &w.namespace, &at),
            kc.path(),
        )?;
    }

    for line in restart_success_lines(name, &planned) {
        println!("{line}");
    }
    Ok(())
}

/// Server-side apply one partial manifest under the restart manager.
fn apply(body: &Value, kubeconfig: &Path) -> Result<()> {
    kubectl_apply_server_side(
        &serde_json::to_string(body).unwrap_or_default(),
        APPRAFTER_CLI_RESTART_FIELD_MANAGER,
        kubeconfig,
    )
}

/// Read one workload's Deployment and CR, and fold them into the plan.
///
/// The Deployment is REQUIRED. It is the object the roll is written to,
/// and a server-side apply of this partial body against an absent one
/// would try to CREATE a Deployment with no selector and no template —
/// the apiserver rejects that, but with a validation error about
/// `spec.selector` that says nothing about what the reader did.
///
/// The CR is optional. It carries only explanation — the pause notice
/// and the restore-in-flight cause — so a workload whose CR cannot be
/// read still restarts, with a less specific message. (Absent in
/// practice means the CR was deleted, which cascades the Deployment with
/// it, so this arm is reached mainly by an RBAC narrowing.)
fn read_workload(target: &PlacedWorkload, kubeconfig: &Path) -> Result<WorkloadRestart> {
    let deployment = kubectl_get_json(
        "deployment",
        Some(&target.name),
        Some(&target.namespace),
        kubeconfig,
    )?
    .ok_or_else(|| CliError::Other(missing_deployment_message(&target.name, &target.namespace)))?;
    let cr = kubectl_get_json(
        "application.apprafter.io",
        Some(&target.name),
        Some(&target.namespace),
        kubeconfig,
    )?;
    Ok(plan_workload(target, &deployment, cr.as_ref()))
}

/// Pure — fold one workload's two objects into its restart plan.
pub(crate) fn plan_workload(
    target: &PlacedWorkload,
    deployment: &Value,
    cr: Option<&Value>,
) -> WorkloadRestart {
    WorkloadRestart {
        name: target.name.clone(),
        namespace: target.namespace.clone(),
        // `spec.replicas` is optional in the API and defaults to 1, so an
        // absent field is ONE pod, never zero. Defaulting the other way
        // would turn every Deployment written without the field into a
        // spurious refusal.
        replicas: deployment
            .pointer("/spec/replicas")
            .and_then(Value::as_i64)
            .unwrap_or(1),
        paused_on: cr.and_then(paused_on),
        pre_restore_replicas: cr.and_then(pre_restore_replicas),
    }
}

/// Pure — the gating plan a CR is paused on, if it is paused.
///
/// Gated on the PHASE, and only then enriched by the condition: the
/// phase is what the operator's early return keys on, so it is the fact
/// that matters, and a condition the operator has not yet rewritten
/// must not be able to report a pause that has ended.
fn paused_on(cr: &Value) -> Option<String> {
    if cr.pointer("/status/phase").and_then(Value::as_str)? != PHASE_AWAITING_MIGRATION_APPROVAL {
        return None;
    }
    let detail = cr
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .and_then(|cs| {
            cs.iter().find(|c| {
                c.get("type").and_then(Value::as_str) == Some(COND_MIGRATION_PENDING)
                    && c.get("status").and_then(Value::as_str) == Some("True")
            })
        })
        .and_then(|c| c.get("message").and_then(Value::as_str))
        .filter(|m| !m.trim().is_empty())
        .unwrap_or("a MigrationPlan is awaiting approval");
    Some(detail.to_string())
}

/// Pure — the replica count a `restore --data-only` recorded before it
/// scaled this application to zero.
///
/// Addressed key by key rather than by JSON pointer: the annotation key
/// holds a `/`, which a pointer reads as a path separator.
fn pre_restore_replicas(cr: &Value) -> Option<String> {
    let raw = cr
        .get("metadata")?
        .get("annotations")?
        .get(PRE_RESTORE_REPLICAS_ANNOTATION)?;
    // Annotations are `map[string]string`, so anything else did not come
    // from the API. Reported as its JSON rather than coerced, exactly as
    // `restore::recorded_replicas` reports the same shape.
    Some(match raw.as_str() {
        Some(text) => text.to_string(),
        None => raw.to_string(),
    })
}

/// Pure — what a registration deploying no AppRafter workload is told
/// (ADR 0064 §"Three behaviours worth stating").
///
/// Two states reach this, and they must not render alike.
///
/// A registration whose `status.resources[]` holds objects but no
/// `apprafter.io/Application` renders its workloads directly — raw YAML,
/// Helm or Kustomize. That is the ONE boundary where both of the
/// exclusions this verb relies on stop holding: the Deployment is in
/// Argo CD's `targetObjs`, Argo CD owns it, and `selfHeal` reverts the
/// pod-template annotation on the next reconcile. The restart would
/// report success and then be undone, so it is refused rather than
/// warned about.
///
/// A registration whose `status.resources[]` is empty has simply not
/// synced. Telling its operator that their manifest renders a Deployment
/// directly would be a false statement about a manifest the cluster has
/// not read yet.
pub(crate) fn no_workload_refusal(application: &str, argo_app: &Value) -> String {
    let synced = argo_app
        .pointer("/status/resources")
        .and_then(Value::as_array)
        .is_some_and(|rs| !rs.is_empty());
    if !synced {
        return format!(
            "Application '{application}' has not synced yet, so it deploys no workload to \
             restart. Wait for the first sync — `apprafter app status {application}` — then \
             retry."
        );
    }
    format!(
        "Application '{application}' renders its workloads directly: Argo CD tracks its \
         resources but none of them is an `apprafter.io/Application`, so this is a raw-YAML, \
         Helm or Kustomize registration.\n\
         `app restart` will not touch it. Argo CD owns those objects, and the pod-template \
         annotation that triggers a rolling update would be reverted by the next self-heal — \
         the restart would report success and then be undone.\n\
         Roll it with whatever tooling renders it, or move the repository onto an AppRafter \
         manifest (`apprafter app scaffold`) so the platform owns the workload."
    )
}

/// Pure — what a workload with nothing to replace is told (ADR 0064
/// §"Three behaviours worth stating").
///
/// `None` lets the restart proceed.
///
/// **The whole invocation is refused, not the zero-replica workloads.**
/// A partial restart reported as "restarted 3 workloads" is the false
/// claim this refusal exists to prevent, one level up: the reader asked
/// for the application and would be told the application had rolled. The
/// live siblings are named with the command that restarts them, so the
/// refusal costs one re-run rather than a diagnosis.
pub(crate) fn zero_replica_refusal(
    application: &str,
    planned: &[WorkloadRestart],
    env: Option<&str>,
) -> Option<String> {
    let idle: Vec<&WorkloadRestart> = planned.iter().filter(|w| w.replaces_nothing()).collect();
    if idle.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for w in &idle {
        out.push(format!(
            "Workload '{}' of application '{application}' is scaled to {} replicas, so \
             restarting it would replace no pods — and reporting a restart over a workload \
             that never moved would be a false claim.",
            w.name, w.replicas
        ));
        match &w.pre_restore_replicas {
            Some(recorded) => out.push(format!(
                "  A restore is in flight: `apprafter restore --data-only` scaled it to zero \
                 and recorded {recorded} replica(s) to come back to in the \
                 `{PRE_RESTORE_REPLICAS_ANNOTATION}` annotation. Let the restore finish — it \
                 scales the application back up — then retry."
            )),
            None => out.push(
                "  If a `restore --data-only` is running it will scale the application back up \
                 when it finishes. Otherwise this zero is declared: set `replicas` above zero \
                 in the manifest and let Argo CD sync it, then retry."
                    .to_string(),
            ),
        }
    }
    let live: Vec<&WorkloadRestart> = planned.iter().filter(|w| !w.replaces_nothing()).collect();
    if !live.is_empty() {
        out.push(String::new());
        out.push(match live.len() {
            1 => format!("The other workload of '{application}' is running. To restart it alone:"),
            n => format!(
                "The other {n} workloads of '{application}' are running. To restart one of them \
                 alone:"
            ),
        });
        out.extend(live.iter().map(|w| {
            format!(
                "  apprafter app restart {application} --workload {}{}",
                w.name,
                env_suffix(env)
            )
        }));
    }
    Some(out.join("\n"))
}

/// Pure — the one line a paused workload gets (ADR 0064 §"Three
/// behaviours worth stating").
///
/// A NOTICE, not a refusal, and the distinction is the decision. The
/// roll re-applies the pod template that is ALREADY applied, so it
/// cannot push the gated change through — which is the only thing the
/// pause asks of it. Refusing would make a credential cutover impossible
/// on any application waiting for an unrelated approval, which is
/// precisely when a workload is most likely to be serving a stale value.
pub(crate) fn paused_notice_lines(planned: &[WorkloadRestart]) -> Vec<String> {
    planned.iter().filter_map(paused_notice_line).collect()
}

fn paused_notice_line(w: &WorkloadRestart) -> Option<String> {
    let detail = w.paused_on.as_deref()?;
    Some(format!(
        "'{}' is paused: {detail}. The restart is not blocked by it — it replaces the pods \
         with the pod template that is already applied, so it cannot push the gated change \
         through. Approve or revert the plan to move that change.",
        w.name
    ))
}

/// Pure — the confirmation preamble.
///
/// It names every workload it will touch and the pods it will replace,
/// because that is the disclosure the reader is agreeing to and `--yes`
/// is the only other way past it.
///
/// The last line is the ADR's entire mitigation for the misuse it
/// accepts: readers will reach for `restart` as a remedy for failing
/// probes — the thing the verb's original rejection was about — and
/// nothing in the design prevents it. Only the output can bias against
/// it, so it says what the restart does not fix, in the place a reader
/// under pressure is already looking.
pub(crate) fn restart_prompt_lines(application: &str, planned: &[WorkloadRestart]) -> Vec<String> {
    let pods: i64 = planned.iter().map(|w| w.replicas).sum();
    let mut out = Vec::new();
    if planned.len() == 1 {
        let w = &planned[0];
        out.push(format!(
            "Restart application '{application}'? This replaces the {} running pod(s) of \
             workload '{}' in namespace {}.",
            w.replicas, w.name, w.namespace
        ));
    } else {
        out.push(format!(
            "Restart application '{application}'? A manifest package is a bundle, so this \
             replaces the {pods} running pod(s) of all {} workloads it deploys:",
            planned.len()
        ));
        out.extend(planned.iter().map(|w| {
            format!(
                "  {} — {} pod(s) in namespace {}",
                w.name, w.replicas, w.namespace
            )
        }));
        out.push(format!(
            "  To restart one of them instead, pass `apprafter app restart {application} \
             --workload <name>`."
        ));
    }
    out.push(
        "  The pod template does not change: the new pods start from the spec that is already \
         applied, so they pick up Secret and ConfigMap values that changed since the old pods \
         started."
            .to_string(),
    );
    out.push(
        "  It is not a remedy for failing probes — a pod that crashes on this template will \
         crash again on it."
            .to_string(),
    );
    out
}

/// Pure — what a completed restart reports.
///
/// "restarting", not "restarted": the apply returns as soon as the
/// apiserver accepts the annotation, and the pods are replaced by a
/// controller over the seconds that follow. Claiming the past tense
/// would be the same false-success shape the zero-replica refusal
/// exists to prevent, at the other end of the command.
pub(crate) fn restart_success_lines(application: &str, planned: &[WorkloadRestart]) -> Vec<String> {
    let pods: i64 = planned.iter().map(|w| w.replicas).sum();
    let mut out = Vec::new();
    if planned.len() == 1 {
        out.push(format!(
            "✓ '{}' restarting — Kubernetes replaces its {} pod(s) as the new ones become \
             ready.",
            planned[0].name, planned[0].replicas
        ));
    } else {
        out.push(format!(
            "✓ Application '{application}' restarting — {} workloads, {pods} pod(s). \
             Kubernetes replaces them as the new ones become ready.",
            planned.len()
        ));
        out.extend(
            planned
                .iter()
                .map(|w| format!("  {} ({} pod(s))", w.name, w.replicas)),
        );
    }
    out.push(format!(
        "  Watch the rollout with `apprafter app status {application}`."
    ));
    out
}

/// Pure — what a workload whose Deployment is missing is told.
fn missing_deployment_message(workload: &str, namespace: &str) -> String {
    format!(
        "Workload '{workload}' has no Deployment in namespace {namespace}, so there is nothing \
         to roll. Argo CD tracks the workload, so the operator has not rendered it yet or its \
         reconcile is failing — check `apprafter app status` before retrying."
    )
}

/// Pure — the `--env` the caller typed, echoed into a quoted command.
fn env_suffix(env: Option<&str>) -> String {
    env.map(|e| format!(" --env {e}")).unwrap_or_default()
}

/// Pure — the partial `Deployment` whose apply rolls the pods.
///
/// **It names the pod-template annotation and nothing else.** That is
/// what makes the write safe under server-side apply: the manager it is
/// applied by owns exactly `f:spec.f:template.f:metadata.f:annotations`
/// → one key, so the operator's `replicas` and image are neither claimed
/// nor contested. A body carrying a second field would make this manager
/// an owner of that field too, and the repository's "omission prunes"
/// lesson would then apply to it on the next write.
pub(crate) fn restart_roll_manifest(name: &str, namespace: &str, at: &str) -> Value {
    let mut annotations = serde_json::Map::new();
    annotations.insert(RESTARTED_AT_ANNOTATION.to_string(), json!(at));
    json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": name, "namespace": namespace },
        "spec": {
            "template": {
                "metadata": { "annotations": Value::Object(annotations) }
            }
        },
    })
}

/// Pure — the partial `Application` CR carrying the durable record.
///
/// Same shape and same reasoning as ADR 0059's `pin_manifest`: nothing
/// but the annotation. It exists separately from the roll because the
/// operator's one-time selector delete-and-recreate drops the
/// Deployment's annotations — harmless for a roll that has already
/// happened, fatal for the only record that it did.
pub(crate) fn restart_record_manifest(name: &str, namespace: &str, at: &str) -> Value {
    let mut annotations = serde_json::Map::new();
    annotations.insert(RESTARTED_AT_ANNOTATION.to_string(), json!(at));
    json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "Application",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "annotations": Value::Object(annotations),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(name: &str, namespace: &str) -> PlacedWorkload {
        PlacedWorkload {
            name: name.to_string(),
            namespace: namespace.to_string(),
        }
    }

    fn workload(name: &str, replicas: i64) -> WorkloadRestart {
        WorkloadRestart {
            name: name.to_string(),
            namespace: "shop".to_string(),
            replicas,
            paused_on: None,
            pre_restore_replicas: None,
        }
    }

    /// A rendered Deployment as the apiserver returns it.
    fn deployment(replicas: Option<i64>) -> Value {
        let mut spec = serde_json::Map::new();
        if let Some(n) = replicas {
            spec.insert("replicas".into(), json!(n));
        }
        json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": { "name": "api", "namespace": "shop" },
            "spec": Value::Object(spec),
        })
    }

    // ------------------------------------------------- behaviour 1: paused

    #[test]
    fn a_paused_migration_plan_does_not_block_the_roll() {
        // ADR 0064 §"Three behaviours worth stating", first bullet. The
        // roll re-applies the pod template that is ALREADY applied, so
        // it cannot push the gated change through — which is the only
        // thing the pause asks of it. A refusal here would make a
        // credential cutover impossible on any application waiting for
        // an unrelated approval, which is exactly when a workload is
        // most likely to be serving a value that was rotated out from
        // under it.
        let cr = json!({
            "metadata": { "name": "api", "namespace": "shop" },
            "status": {
                "phase": "AwaitingMigrationApproval",
                "conditions": [
                    { "type": "Ready", "status": "False", "reason": "MigrationPending",
                      "message": "paused awaiting approval of MigrationPlan shop/api-migration-1" },
                    { "type": "MigrationPending", "status": "True",
                      "reason": "MigrationPlanPending",
                      "message": "MigrationPlan shop/api-migration-1 is awaiting approval" }
                ]
            }
        });
        let planned = vec![plan_workload(
            &placed("api", "shop"),
            &deployment(Some(2)),
            Some(&cr),
        )];

        // It is a NOTICE…
        let notices = paused_notice_lines(&planned);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].contains("MigrationPlan shop/api-migration-1 is awaiting approval"),
            "the notice must name the gating plan so `kubectl describe` lands: {}",
            notices[0]
        );
        assert!(
            notices[0].contains("not blocked"),
            "the notice must say the restart proceeds, or it reads as a refusal: {}",
            notices[0]
        );
        assert!(
            notices[0].contains("already applied"),
            "…and why it may: the template is unchanged, so the gated change cannot ride \
             along: {}",
            notices[0]
        );

        // …and NOT a refusal. This is the assertion that fails if the
        // pause is ever promoted to one.
        assert_eq!(
            zero_replica_refusal("shop", &planned, None),
            None,
            "a paused MigrationPlan must not refuse the roll",
        );
    }

    #[test]
    fn a_workload_that_is_not_paused_gets_no_notice() {
        // The notice must not fire on every restart — it would then be
        // noise a reader learns to skip, on the one line that matters
        // when it is true.
        let cr = json!({
            "metadata": { "name": "api", "namespace": "shop" },
            "status": { "phase": "Ready" }
        });
        let planned = vec![plan_workload(
            &placed("api", "shop"),
            &deployment(Some(2)),
            Some(&cr),
        )];
        assert!(paused_notice_lines(&planned).is_empty());
    }

    #[test]
    fn a_pause_is_read_off_the_phase_not_off_a_stale_condition() {
        // The operator's early return keys on the PHASE, so the phase is
        // the fact. A `MigrationPending=True` condition it has not
        // rewritten yet must not be able to report a pause that ended —
        // that would print a notice describing an approval the reader
        // already gave.
        let cr = json!({
            "metadata": { "name": "api", "namespace": "shop" },
            "status": {
                "phase": "Ready",
                "conditions": [
                    { "type": "MigrationPending", "status": "True",
                      "message": "MigrationPlan shop/api-migration-1 is awaiting approval" }
                ]
            }
        });
        assert_eq!(paused_on(&cr), None);
    }

    #[test]
    fn a_pause_without_a_readable_condition_still_says_so() {
        // `MigrationFailed` reaches the same phase with a different
        // condition type, and a CR read under a narrowed RBAC may carry
        // the phase and no conditions at all. Silence there would be the
        // worse failure: the reader is not told the application is
        // halted.
        let cr = json!({
            "metadata": { "name": "api", "namespace": "shop" },
            "status": { "phase": "AwaitingMigrationApproval" }
        });
        let detail = paused_on(&cr).expect("the phase alone is enough");
        assert!(detail.contains("MigrationPlan"), "{detail}");
    }

    // -------------------------------------------- behaviour 2: zero replicas

    #[test]
    fn zero_replicas_is_refused_and_the_refusal_names_the_cause() {
        // ADR 0064 §"Three behaviours worth stating", second bullet.
        // `restore --data-only` sets `spec.base.replicas: 0` and records
        // the count to come back to. Rolling zero pods and printing
        // "restarted" is a false claim, and the reader has no other
        // signal that nothing moved — so it is refused, and the refusal
        // names the cause rather than leaving them to find it.
        let cr = json!({
            "metadata": {
                "name": "api",
                "namespace": "shop",
                "annotations": { "apprafter.io/pre-restore-replicas": "3" }
            },
            "spec": { "base": { "replicas": 0 } }
        });
        let planned = vec![plan_workload(
            &placed("api", "shop"),
            &deployment(Some(0)),
            Some(&cr),
        )];
        let msg = zero_replica_refusal("shop", &planned, None).expect("must refuse");
        assert!(msg.contains("scaled to 0 replicas"), "{msg}");
        assert!(msg.contains("false claim"), "{msg}");
        assert!(
            msg.contains("A restore is in flight"),
            "the refusal must name a restore as the cause when one recorded a count: {msg}"
        );
        assert!(
            msg.contains("recorded 3 replica(s)"),
            "…and the count it will come back to, so the reader can tell a stuck restore \
             from a finished one: {msg}"
        );
        assert!(
            msg.contains(PRE_RESTORE_REPLICAS_ANNOTATION),
            "…and the annotation holding it, which is readable by hand: {msg}"
        );
    }

    #[test]
    fn zero_replicas_without_a_restore_record_still_names_a_restore_as_possible() {
        // The annotation is written in the SAME patch as the
        // scale-to-zero, so its absence does not prove no restore is
        // running — a run interrupted before its first write leaves
        // neither. The reader is told both possibilities and how to tell
        // them apart, because "scaled to zero" on its own sends them
        // looking at the manifest for a number somebody else moved.
        let planned = vec![workload("api", 0)];
        let msg = zero_replica_refusal("shop", &planned, None).expect("must refuse");
        assert!(msg.contains("restore --data-only"), "{msg}");
        assert!(msg.contains("`replicas` above zero"), "{msg}");
    }

    #[test]
    fn a_restore_record_refuses_even_while_the_deployment_still_reports_pods() {
        // The window the second condition exists for. `restore
        // --data-only` patches the CR to zero and annotates it in one
        // write; until the operator reconciles, the Deployment still
        // says 3. A restart issued there rolls pods that are about to be
        // deleted out from under it and reports a success the cluster
        // then contradicts.
        let cr = json!({
            "metadata": {
                "name": "api",
                "namespace": "shop",
                "annotations": { "apprafter.io/pre-restore-replicas": "3" }
            }
        });
        let planned = vec![plan_workload(
            &placed("api", "shop"),
            &deployment(Some(3)),
            Some(&cr),
        )];
        let msg = zero_replica_refusal("shop", &planned, None)
            .expect("an in-flight restore refuses regardless of the live count");
        assert!(msg.contains("A restore is in flight"), "{msg}");
    }

    #[test]
    fn one_idle_workload_refuses_the_whole_bundle_and_names_the_live_ones() {
        // The reader asked for the APPLICATION, so a partial roll
        // reported as "application restarted" is the same false claim
        // one level up. The refusal therefore covers the invocation —
        // and pays the cost of that by quoting the command that
        // restarts each live sibling, carrying the `--env` the caller
        // already typed (without it the retry re-resolves to a
        // different deployment, or to none).
        let planned = vec![
            workload("api", 0),
            workload("web", 2),
            workload("worker", 1),
        ];
        let msg = zero_replica_refusal("shop", &planned, Some("prod")).expect("must refuse");
        assert!(msg.contains("Workload 'api'"), "{msg}");
        assert!(msg.contains("The other 2 workloads"), "{msg}");
        assert!(
            msg.contains("apprafter app restart shop --workload web --env prod"),
            "{msg}"
        );
        assert!(
            msg.contains("apprafter app restart shop --workload worker --env prod"),
            "{msg}"
        );
        // ADR 0062 §Addressing: the positional stays the application in
        // every quoted command, at every bundle size.
        assert!(!msg.contains("app restart web"), "{msg}");
    }

    #[test]
    fn a_running_bundle_is_not_refused() {
        // The guard must not be able to pass vacuously: with every
        // workload serving, `restart` proceeds.
        let planned = vec![workload("api", 1), workload("web", 4)];
        assert_eq!(zero_replica_refusal("shop", &planned, None), None);
    }

    // --------------------------------- behaviour 3: a git-rendered Deployment

    #[test]
    fn a_registration_that_renders_a_deployment_directly_is_refused() {
        // ADR 0064 §"Three behaviours worth stating", third bullet, and
        // §Risks. For an AppRafter registration the Deployment is
        // excluded from Argo CD's managed set twice over — the operator
        // sets an ownerReference, and `make_labels` never writes a
        // tracking label. A raw-YAML, Helm or Kustomize registration is
        // the one boundary where BOTH exclusions stop holding: the
        // Deployment is in `targetObjs`, Argo CD owns it, and self-heal
        // reverts the annotation. The restart would report success and
        // then be undone, so it is refused rather than warned about.
        //
        // The discriminator is the one ADR 0064 names: no
        // `apprafter.io/Application` in `status.resources[]`. The
        // fixture is the shape `app_open`'s own raw-YAML test pins,
        // including the `argoproj.io` Application an app-of-apps child
        // renders — a Deployment called `shop` must not be mistaken for
        // the workload, and neither must a `kind: Application` from the
        // wrong group.
        let app = json!({
            "metadata": { "name": "shop" },
            "spec": { "destination": { "namespace": "shop" } },
            "status": { "resources": [
                { "group": "", "kind": "Namespace", "name": "shop", "version": "v1" },
                { "group": "apps", "kind": "Deployment", "name": "shop",
                  "namespace": "shop", "version": "v1" },
                { "group": "", "kind": "Service", "name": "shop",
                  "namespace": "shop", "version": "v1" },
                { "group": "argoproj.io", "kind": "Application", "name": "child",
                  "namespace": "argocd", "version": "v1alpha1" }
            ]}
        });
        // The premise: this really is the state the refusal keys on.
        assert!(
            apprafter_app_refs(&app).is_empty(),
            "the fixture must reach the no-workload arm at all",
        );

        let msg = no_workload_refusal("shop", &app);
        assert!(msg.contains("renders its workloads directly"), "{msg}");
        assert!(
            msg.contains("self-heal"),
            "the refusal must say WHY, or it reads as an arbitrary restriction: {msg}"
        );
        assert!(
            msg.contains("report success and then be undone"),
            "…and what the alternative would have cost: {msg}"
        );
        assert!(
            !msg.contains("has not synced"),
            "a synced raw-YAML registration must not be told to wait for a sync: {msg}"
        );
    }

    #[test]
    fn a_registration_that_has_not_synced_is_told_that_instead() {
        // The near miss, and the reason the refusal branches. Both
        // states reach the same arm of `workload_for`, but telling an
        // operator seconds after `app add` that their manifest renders a
        // Deployment directly is a false statement about a manifest the
        // cluster has not read yet.
        for app in [
            json!({ "metadata": { "name": "shop" } }),
            json!({ "metadata": { "name": "shop" }, "status": { "resources": [] } }),
        ] {
            let msg = no_workload_refusal("shop", &app);
            assert!(msg.contains("has not synced yet"), "{msg}");
            assert!(!msg.contains("renders its workloads directly"), "{msg}");
        }
    }

    // ------------------------------------------------------------ the writes

    #[test]
    fn the_roll_manifest_carries_the_pod_template_annotation_and_nothing_else() {
        // What makes the write safe under server-side apply. A partial
        // body claims ownership of exactly the fields it names, so this
        // one owns `f:spec.f:template.f:metadata.f:annotations` → one
        // key. Naming `replicas` or the image here would make this
        // manager an owner of them, put it in conflict with the operator
        // on every reconcile, and bring the repository's
        // "omission prunes" lesson down on whichever field was dropped
        // next.
        let body = restart_roll_manifest("api", "shop", "2026-09-14T10:00:00+00:00");
        assert_eq!(body["apiVersion"], "apps/v1");
        assert_eq!(body["kind"], "Deployment");
        assert_eq!(body["metadata"]["name"], "api");
        assert_eq!(body["metadata"]["namespace"], "shop");
        assert_eq!(
            body["spec"]["template"]["metadata"]["annotations"][RESTARTED_AT_ANNOTATION],
            "2026-09-14T10:00:00+00:00"
        );

        // Nothing else, asserted by walking the body rather than by
        // listing the fields it must not have: a future field nobody
        // thought to forbid is exactly the one that would slip through a
        // deny-list.
        let spec = body["spec"].as_object().expect("spec is an object");
        assert_eq!(
            spec.keys().collect::<Vec<_>>(),
            vec!["template"],
            "{spec:?}"
        );
        let template = body["spec"]["template"]
            .as_object()
            .expect("template is an object");
        assert_eq!(
            template.keys().collect::<Vec<_>>(),
            vec!["metadata"],
            "the pod template must carry no `spec` — that is the operator's: {template:?}"
        );
        let meta = body["spec"]["template"]["metadata"]
            .as_object()
            .expect("template metadata is an object");
        assert_eq!(
            meta.keys().collect::<Vec<_>>(),
            vec!["annotations"],
            "no labels: the selector is immutable and the labels are the operator's: {meta:?}"
        );
        let anns = body["spec"]["template"]["metadata"]["annotations"]
            .as_object()
            .expect("annotations is an object");
        assert_eq!(anns.len(), 1, "{anns:?}");
    }

    #[test]
    fn the_record_manifest_carries_the_cr_annotation_and_nothing_else() {
        // The ADR 0059 shape. The record lives on the CR because the
        // operator's one-time selector delete-and-recreate drops the
        // Deployment's annotations — harmless for a roll that already
        // happened, fatal for the only evidence that it did.
        let body = restart_record_manifest("api", "shop", "2026-09-14T10:00:00+00:00");
        assert_eq!(body["apiVersion"], "apprafter.io/v1alpha1");
        assert_eq!(body["kind"], "Application");
        assert_eq!(
            body["metadata"]["annotations"][RESTARTED_AT_ANNOTATION],
            "2026-09-14T10:00:00+00:00"
        );
        assert!(
            body.get("spec").is_none(),
            "a `spec` here would make the restart manager an owner of it: {body}"
        );
        let meta = body["metadata"].as_object().expect("metadata is an object");
        let mut keys: Vec<&String> = meta.keys().collect();
        keys.sort();
        assert_eq!(keys, vec!["annotations", "name", "namespace"], "{meta:?}");
    }

    #[test]
    fn the_two_writes_record_one_timestamp_under_one_key() {
        // A reader who finds the annotation on a pod template must find
        // the same string on the CR without learning a second spelling,
        // and the two objects must agree on WHEN — they record one event.
        let at = "2026-09-14T10:00:00+00:00";
        assert_eq!(
            restart_roll_manifest("api", "shop", at)["spec"]["template"]["metadata"]["annotations"]
                [RESTARTED_AT_ANNOTATION],
            restart_record_manifest("api", "shop", at)["metadata"]["annotations"]
                [RESTARTED_AT_ANNOTATION],
        );
        // Not `kubectl`'s key: an AppRafter restart must stay
        // distinguishable from a hand-run `kubectl rollout restart` in
        // the audit that wants to tell them apart.
        assert_ne!(RESTARTED_AT_ANNOTATION, "kubectl.kubernetes.io/restartedAt");
        assert!(RESTARTED_AT_ANNOTATION.starts_with("apprafter.io/"));
    }

    #[test]
    fn the_restart_manager_is_its_own_and_is_not_argo_shaped() {
        // It must NOT be the pin manager: `app unpin` re-applies that
        // one's body with the keys omitted and server-side apply prunes
        // whatever it owned, so a restart record filed under it would be
        // deleted by the next un-pin.
        use cli_providers::k8s::kubectl::{
            APPRAFTER_CLI_EGRESS_FIELD_MANAGER, APPRAFTER_CLI_FIELD_MANAGER,
            APPRAFTER_CLI_PIN_FIELD_MANAGER,
        };
        for other in [
            APPRAFTER_CLI_FIELD_MANAGER,
            APPRAFTER_CLI_EGRESS_FIELD_MANAGER,
            APPRAFTER_CLI_PIN_FIELD_MANAGER,
        ] {
            assert_ne!(APPRAFTER_CLI_RESTART_FIELD_MANAGER, other);
        }
        // …and not Argo-CD-shaped, or an ownership guard would mistake
        // our own previous write for a git owner.
        for needle in ["argocd", "argo-cd", "application-controller"] {
            assert!(!APPRAFTER_CLI_RESTART_FIELD_MANAGER.contains(needle));
        }
    }

    // ------------------------------------------------------------- the plan

    #[test]
    fn the_replica_count_comes_off_the_deployment_and_defaults_to_one() {
        // The resolved count, not the declared one: `spec.base.replicas`
        // on the CR still has to be unified with
        // `spec.environments[<env>]`, and a second implementation of
        // that unification here would disagree with the operator's on
        // exactly the env-override manifests nobody tests by hand.
        let cr = json!({
            "metadata": { "name": "api", "namespace": "shop" },
            "spec": { "base": { "replicas": 0 },
                      "environments": { "prod": { "replicas": 4 } } },
            "status": { "phase": "Ready" }
        });
        let planned = plan_workload(&placed("api", "shop"), &deployment(Some(4)), Some(&cr));
        assert_eq!(planned.replicas, 4, "the CR's base 0 is not the live count");
        assert_eq!(zero_replica_refusal("shop", &[planned], None), None);

        // `spec.replicas` is optional and the apiserver defaults it to
        // 1. Defaulting it to 0 here would turn every Deployment written
        // without the field into a spurious refusal.
        let defaulted = plan_workload(&placed("api", "shop"), &deployment(None), None);
        assert_eq!(defaulted.replicas, 1);
        assert_eq!(zero_replica_refusal("shop", &[defaulted], None), None);
    }

    #[test]
    fn a_workload_whose_cr_cannot_be_read_still_plans() {
        // The CR carries explanation only — the pause notice and the
        // restore cause. Making it fatal would take a restart away from
        // a reader whose RBAC narrowed, for a message.
        let planned = plan_workload(&placed("api", "shop"), &deployment(Some(2)), None);
        assert_eq!(planned.name, "api");
        assert_eq!(planned.namespace, "shop");
        assert_eq!(planned.replicas, 2);
        assert_eq!(planned.paused_on, None);
        assert_eq!(planned.pre_restore_replicas, None);
    }

    #[test]
    fn a_pre_restore_annotation_that_is_not_a_count_is_reported_not_coerced() {
        // Same rule `restore::recorded_replicas` applies to the same
        // key: annotations are `map[string]string`, so anything else did
        // not come from the API and must not be silently read as a
        // number.
        let cr = json!({
            "metadata": {
                "name": "api", "namespace": "shop",
                "annotations": { "apprafter.io/pre-restore-replicas": 3 }
            }
        });
        assert_eq!(pre_restore_replicas(&cr).as_deref(), Some("3"));
        let cr = json!({
            "metadata": {
                "name": "api", "namespace": "shop",
                "annotations": { "apprafter.io/pre-restore-replicas": "three" }
            }
        });
        assert_eq!(pre_restore_replicas(&cr).as_deref(), Some("three"));
    }

    // ---------------------------------------------------------- the renders

    #[test]
    fn the_prompt_names_every_workload_and_what_the_restart_does_not_fix() {
        // It replaces running pods, so the reader is shown exactly what
        // it will touch before they agree — and the last line is ADR
        // 0064's whole mitigation for the misuse it accepts: readers
        // will reach for `restart` as a remedy for failing probes, and
        // only the output can bias against it.
        let lines = restart_prompt_lines("shop", &[workload("api", 3), workload("web", 1)]);
        let msg = lines.join("\n");
        assert!(msg.contains("Restart application 'shop'?"), "{msg}");
        assert!(msg.contains("4 running pod(s)"), "{msg}");
        assert!(msg.contains("all 2 workloads"), "{msg}");
        assert!(msg.contains("api — 3 pod(s) in namespace shop"), "{msg}");
        assert!(msg.contains("web — 1 pod(s) in namespace shop"), "{msg}");
        assert!(
            msg.contains("apprafter app restart shop --workload <name>"),
            "a bundle prompt must name the way to narrow it: {msg}"
        );
        assert!(
            msg.contains("not a remedy for failing probes"),
            "the probe-misuse bias is the only mitigation the design has: {msg}"
        );
        assert!(
            msg.contains("pod template does not change"),
            "…and the reader must know a restart cannot deploy anything: {msg}"
        );

        // A single-workload bundle — today's entire fleet — gets no
        // bundle list and no `--workload` hint, because there is nothing
        // to narrow.
        let solo = restart_prompt_lines("blog", &[workload("blog", 2)]).join("\n");
        assert!(solo.contains("workload 'blog' in namespace shop"), "{solo}");
        assert!(!solo.contains("--workload <name>"), "{solo}");
        assert!(solo.contains("not a remedy for failing probes"), "{solo}");
    }

    #[test]
    fn the_success_line_does_not_claim_the_past_tense() {
        // The apply returns as soon as the apiserver accepts the
        // annotation; the pods are replaced by a controller over the
        // seconds that follow. "restarted" would be the same
        // false-success shape the zero-replica refusal exists to
        // prevent, at the other end of the command.
        for lines in [
            restart_success_lines("blog", &[workload("blog", 2)]),
            restart_success_lines("shop", &[workload("api", 3), workload("web", 1)]),
        ] {
            let msg = lines.join("\n");
            assert!(msg.contains("restarting"), "{msg}");
            assert!(!msg.contains("restarted"), "{msg}");
            assert!(
                msg.contains("apprafter app status"),
                "the reader needs somewhere to watch it finish: {msg}"
            );
        }

        // A bundle says how many moved. "the workload", singular, over
        // three of them is the defect ADR 0062 found in `rollback`'s
        // success line.
        let bundle =
            restart_success_lines("shop", &[workload("api", 3), workload("web", 1)]).join("\n");
        assert!(bundle.contains("2 workloads, 4 pod(s)"), "{bundle}");
        assert!(bundle.contains("api (3 pod(s))"), "{bundle}");
        assert!(bundle.contains("web (1 pod(s))"), "{bundle}");
    }

    #[test]
    fn a_missing_deployment_is_not_reported_as_an_unsynced_application() {
        // Argo CD tracks the workload, so the registration HAS synced —
        // the operator has not rendered the Deployment, or its reconcile
        // is failing. Telling the reader to wait for a sync that already
        // happened sends them to watch the wrong thing.
        let msg = missing_deployment_message("api", "shop");
        assert!(msg.contains("no Deployment in namespace shop"), "{msg}");
        assert!(!msg.contains("not synced"), "{msg}");
        assert!(msg.contains("apprafter app status"), "{msg}");
    }
}
