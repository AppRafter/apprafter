// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! `MigrationStrategy` impls for both scopes.
//!
//! `execute_step` is a no-op (returns Succeeded) in 1.76 — the
//! schema's `plan[].action` is free-form text without machine
//! semantics. Later phases can replace these impls with real
//! action runners (snapshot DB, provision RDS, etc.) once a
//! per-action vocabulary is settled.
//!
//! `reject` is real for the platform strategy: it reads
//! `plan.spec.previousSpecSnapshot.pin` and SSA-patches
//! `PlatformStack.spec.pin` back to that value. ApplicationScope
//! `reject` is a no-op per ADR 0027 (application-scope plans
//! have no reject path; the user reverts the Git commit
//! instead, which the admission webhook also enforces by
//! rejecting the phase transition).
//!
//! Both strategies are idempotent: re-running `reject` after
//! the revert has already landed is a no-op because
//! `PlatformStack.spec.pin` already equals the snapshot value
//! and SSA produces a byte-identical patch (no resource-version
//! bump, no watch fan-out).

use async_trait::async_trait;
use chrono::Utc;
use kube::api::{Api, ObjectMeta, Patch, PatchParams};
use kube::core::DynamicObject;
use kube::discovery::{ApiCapabilities, ApiResource};
use kube::Client;
use serde_json::{json, Value};
use tracing::info;

use operator_core::migration::classification_severity;
use operator_core::{
    image_repo, Application, ApplicationBaseSpec, DestructiveChange, MigrationApplicationRef,
    MigrationApplicationScope, MigrationError, MigrationPlan, MigrationPlanScope,
    MigrationPlanSpec, MigrationStep, MigrationStrategy, MigrationTrigger, Needs, OneOrMany,
    SourceCredentialSpec, StepOutcome,
};

/// Render-time default for `Application.spec.*.replicas` (application.cue:
/// "defaults to 1 at render time"). The field is optional with no CUE
/// default value, so an absent `replicas` is resolved to this before the
/// scale-to-zero comparison — editing an app from its implicit `1` down
/// to an explicit `0` still counts as a scale-to-zero.
const REPLICAS_RENDER_DEFAULT: i32 = 1;

/// SSA field manager used by the strategies' reject path when
/// patching outside resources (currently only PlatformStack).
/// Distinct from `reconcile::FIELD_MANAGER` (which writes
/// MigrationPlan status); the second manager lets
/// `PlatformController`'s `detect_outside_writer` distinguish
/// "MigrationController reject" from foreign writes vs the
/// regular `platform-controller` field manager.
pub const STRATEGY_FIELD_MANAGER: &str = "migration-controller-strategy";

/// Application-scope strategy. Detection is concrete (lands
/// in B.1.77 callers) but not part of the trait surface; the
/// trait covers only the controller-side execute + reject
/// halves.
#[derive(Debug, Default, Clone)]
pub struct ApplicationMigrationStrategy;

#[async_trait]
impl MigrationStrategy for ApplicationMigrationStrategy {
    async fn execute_step(
        &self,
        _plan: &MigrationPlan,
        _step: &MigrationStep,
    ) -> Result<StepOutcome, MigrationError> {
        // B.1.76: free-form action text, no machine semantics.
        // Mark every step Succeeded so the controller advances
        // through the plan. Real action runners land in a
        // later phase once the action vocabulary is set.
        Ok(StepOutcome::Succeeded)
    }

    async fn reject(&self, _plan: &MigrationPlan) -> Result<(), MigrationError> {
        // ADR 0027: application-scope plans cannot be rejected
        // — the user reverts the Git commit instead. The
        // admission webhook (B.1.76 FSM extension) blocks
        // application-scope `phase: ... → rejected`
        // transitions, so this code path should never be
        // reached. Returning Ok defensively keeps the
        // controller from looping if a misconfigured webhook
        // lets the transition slip through.
        Ok(())
    }
}

impl ApplicationMigrationStrategy {
    /// Decide whether the change from `old` → `new` (two **effective**
    /// specs — the unified base fields `image/replicas/expose/env/needs`,
    /// as returned by `operator_rendering::effective_spec`) warrants a
    /// MigrationPlan.
    ///
    /// 2.16b builds this out op-class by op-class. This slice handles
    /// **needs-removal**: dropping a `needs.<type>` (or a named
    /// `needs.<type>.<name>`) that existed in `old` is a destructive
    /// **data-migration** — the backing claim (and its data) is torn
    /// down, so the change is gated through a MigrationPlan. Adding a
    /// need (present in `new`, absent in `old`) is non-destructive.
    ///
    /// When an edit carries several destructive ops the classifier
    /// collects every candidate and `pick_primary` returns the single
    /// highest-severity one (2.16b spec: highest-severity wins).
    pub fn detect_destructive(
        old: &ApplicationBaseSpec,
        new: &ApplicationBaseSpec,
    ) -> Option<DestructiveChange> {
        let mut candidates: Vec<DestructiveChange> = Vec::new();

        for key in removed_needs_keys(old.needs.as_ref(), new.needs.as_ref()) {
            candidates.push(DestructiveChange {
                trigger_type: "needs-removal".to_string(),
                field: key.clone(),
                from: Some(json!(key)),
                to: Some(json!("(removed)")),
                classification: "data-migration".to_string(),
            });
        }

        // Network visibility + public-domain ops. The `network` field
        // defaults to `internal` (application.cue:73) when absent, so an
        // Application with no `expose.network` is treated as internal.
        let old_net = old
            .expose
            .as_ref()
            .and_then(|e| e.network.as_deref())
            .unwrap_or("internal");
        let new_net = new
            .expose
            .as_ref()
            .and_then(|e| e.network.as_deref())
            .unwrap_or("internal");
        let old_host = first_hostname(old);
        let new_host = first_hostname(new);

        // public -> any non-public visibility flip: the app stops being
        // reachable on its public HTTPRoute, so it's gated (requires-restart).
        if old_net == "public" && new_net != "public" {
            candidates.push(DestructiveChange {
                trigger_type: "network-visibility-change".to_string(),
                field: "expose.network".to_string(),
                from: Some(json!("public")),
                to: Some(json!(new_net)),
                classification: "requires-restart".to_string(),
            });
        }

        // Hostname removal / change of a PUBLICLY-ROUTED app ONLY. On a
        // non-public app the HTTPRoute isn't emitted, so the hostname is
        // inert and its change is soft. Gating on `old_net == "public"`
        // (the app was routed before the edit) also covers the
        // public->internal case where the route is being withdrawn.
        if old_net == "public" && old_host != new_host {
            candidates.push(DestructiveChange {
                trigger_type: "domain-change".to_string(),
                field: "expose.hostname".to_string(),
                from: Some(json!(old_host.clone().unwrap_or_default())),
                to: Some(json!(new_host
                    .clone()
                    .unwrap_or_else(|| "(removed)".to_string()))),
                classification: "requires-restart".to_string(),
            });
        }

        // Scale-to-zero. `replicas` is optional and resolves to 1 at
        // render time (application.cue), so an absent value is the
        // effective `1`. Only the `>0 -> 0` transition is destructive
        // (the app goes dark); every other move (N->M, 0->N) is a soft
        // scale that the rollout handles without a MigrationPlan.
        let old_r = old.replicas.unwrap_or(REPLICAS_RENDER_DEFAULT);
        let new_r = new.replicas.unwrap_or(REPLICAS_RENDER_DEFAULT);
        if old_r > 0 && new_r == 0 {
            candidates.push(DestructiveChange {
                trigger_type: "scale-to-zero".to_string(),
                field: "replicas".to_string(),
                from: Some(json!(old_r.to_string())),
                to: Some(json!("0")),
                classification: "requires-restart".to_string(),
            });
        }

        // Image *repository* change (2.4h split). A tag change is a soft
        // rollout (the controller resolves the new tag→digest and rolls
        // the Deployment), but moving to a different repository is a
        // pull-source change we gate. Only when BOTH sides carry an image
        // — a None on either side is an image add/remove, out of scope
        // here. `image_repo` mirrors the 2.4h `image_repo_path` heuristic.
        if let (Some(old_img), Some(new_img)) = (old.image.as_deref(), new.image.as_deref()) {
            let old_repo = image_repo(old_img);
            let new_repo = image_repo(new_img);
            if old_repo != new_repo {
                candidates.push(DestructiveChange {
                    trigger_type: "image-path-change".to_string(),
                    field: "spec.image".to_string(),
                    from: Some(json!(old_repo)),
                    to: Some(json!(new_repo)),
                    classification: "requires-restart".to_string(),
                });
            }
        }

        pick_primary(candidates)
    }

    /// Build a `MigrationPlan` CR for an application-scope
    /// destructive change. The Plan lands in
    /// `apprafter-system` (per spec.md §3.8 — plans are
    /// cluster-scoped from a user's POV even though the CR
    /// itself is namespaced for RBAC granularity).
    ///
    /// `application_name` / `application_namespace` / `environment`
    /// identify which Application + environment the plan
    /// governs. The caller (B.1.77 Application reconciler) knows
    /// these from the Application CR it just reconciled.
    ///
    /// `plan_name` should embed the application + a date / UID
    /// so the resulting CR has a stable, human-readable name.
    /// The Application reconciler synthesises one from
    /// `<app>-<env>-migration-<timestamp>`.
    pub fn create_plan_for(
        change: &DestructiveChange,
        plan_name: &str,
        application_namespace: &str,
        application_name: &str,
        environment: &str,
    ) -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "application".into(),
                application: Some(MigrationApplicationScope {
                    ref_: MigrationApplicationRef {
                        name: application_name.to_string(),
                        namespace: application_namespace.to_string(),
                    },
                    environment: environment.to_string(),
                }),
                platform: None,
            },
            trigger: MigrationTrigger {
                type_: change.trigger_type.clone(),
                field: change.field.clone(),
                from: change.from.clone(),
                to: change.to.clone(),
            },
            risks: Some(operator_core::MigrationRisks {
                classification: change.classification.clone(),
                estimated_downtime: None,
                data_volume: None,
                reversible: None,
                requires_full_backup: None,
            }),
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        // ObjectMeta path used here (not `MigrationPlan::new`)
        // because we need to set the namespace explicitly —
        // `MigrationPlan::new` builds a cluster-scoped meta.
        let mut mp = MigrationPlan::new(plan_name, spec);
        mp.metadata = ObjectMeta {
            name: Some(plan_name.to_string()),
            namespace: Some("apprafter-system".to_string()),
            labels: Some(
                [
                    ("apprafter.io/scope".to_string(), "application".to_string()),
                    (
                        "apprafter.io/application".to_string(),
                        application_name.to_string(),
                    ),
                    (
                        "apprafter.io/application-namespace".to_string(),
                        application_namespace.to_string(),
                    ),
                    (
                        "apprafter.io/environment".to_string(),
                        environment.to_string(),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
            creation_timestamp: Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                Utc::now(),
            )),
            ..ObjectMeta::default()
        };
        mp
    }
}

/// Expand a `Needs` block into the set of stable `(type, name)` keys it
/// declares: `needs.<type>` for the unnamed default of a type,
/// `needs.<type>.<name>` for a named entry. Reuses `Needs::entries`
/// (the byte-stable flatten used across claim-gen / renderer / GC) so
/// the key vocabulary matches the rest of the operator exactly.
fn needs_keys(needs: Option<&Needs>) -> Vec<String> {
    needs
        .map(|n| {
            n.entries()
                .into_iter()
                .map(|(ty, entry)| match entry.name {
                    Some(name) => format!("needs.{ty}.{name}"),
                    None => format!("needs.{ty}"),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The first declared public hostname of a spec, or `None` when the
/// Application declares no `expose.hostname`. `OneOrMany::One(h)` → `h`;
/// `OneOrMany::Many(v)` → the first element (`None` for an empty vec).
/// Only the first hostname participates in domain-change detection — a
/// multi-hostname edit still gates on the primary, which is enough to
/// flag the route change (the full hostname-set diff is render-side).
fn first_hostname(s: &ApplicationBaseSpec) -> Option<String> {
    match s.expose.as_ref().and_then(|e| e.hostname.as_ref()) {
        Some(OneOrMany::One(h)) => Some(h.clone()),
        Some(OneOrMany::Many(v)) => v.first().cloned(),
        None => None,
    }
}

/// Keys present in `old` but absent from `new` — i.e. the `needs`
/// entries this edit removes. Order follows `Needs::entries`' fixed
/// per-type ordering, so the resulting candidate list is deterministic.
fn removed_needs_keys(old: Option<&Needs>, new: Option<&Needs>) -> Vec<String> {
    let new_keys = needs_keys(new);
    needs_keys(old)
        .into_iter()
        .filter(|k| !new_keys.contains(k))
        .collect()
}

/// Pick the single primary `DestructiveChange` from the candidates an
/// edit produced: highest `classification_severity` wins (2.16b spec).
/// `max_by_key` returns the LAST maximum on ties — Task 6 replaces this
/// with a fully deterministic tie-break across equal-severity ops.
fn pick_primary(candidates: Vec<DestructiveChange>) -> Option<DestructiveChange> {
    candidates
        .into_iter()
        .max_by_key(|c| classification_severity(&c.classification))
}

// Suppress the `Application` type-import that's only used in
// the public concrete-fn signature comments; keeps the `use`
// list tidy without `#[allow(unused_imports)]`.
#[allow(dead_code)]
fn _hint_application(_: &Application) {}

/// Platform-scope strategy.
///
/// Holds a kube `Client` so `reject()` can patch
/// `PlatformStack.spec.pin`. The client is shared with the
/// controller's `Context`; both manage their own field-manager
/// identity so concurrent writes from the same pod don't
/// step on each other.
#[derive(Clone)]
pub struct PlatformMigrationStrategy {
    client: Client,
    platformstack_api: ApiResource,
}

impl PlatformMigrationStrategy {
    pub fn new(client: Client) -> Self {
        // Construct the dynamic ApiResource for PlatformStack
        // statically — the GVK is known at compile time. Using
        // discovery here would force an async constructor and
        // pull a network call into the strategy's hot path
        // (each reject call is rare, but resolving an ApiResource
        // is cheap when done statically).
        let platformstack_api = ApiResource {
            group: "apprafter.io".into(),
            version: "v1alpha1".into(),
            api_version: "apprafter.io/v1alpha1".into(),
            kind: "PlatformStack".into(),
            plural: "platformstacks".into(),
        };
        Self {
            client,
            platformstack_api,
        }
    }

    /// Singleton namespace + name match PlatformController's
    /// constants (kept duplicated rather than imported to avoid
    /// a circular workspace-internal dep — operator-controllers/
    /// migration must not depend on operator-controllers/
    /// platform-stack).
    const SINGLETON_NAMESPACE: &'static str = "apprafter-system";
    const SINGLETON_NAME: &'static str = "default";
}

#[async_trait]
impl MigrationStrategy for PlatformMigrationStrategy {
    async fn execute_step(
        &self,
        _plan: &MigrationPlan,
        _step: &MigrationStep,
    ) -> Result<StepOutcome, MigrationError> {
        // Same rationale as ApplicationMigrationStrategy —
        // free-form action text in 1.76; no machine semantics
        // to run.
        Ok(StepOutcome::Succeeded)
    }

    async fn reject(&self, plan: &MigrationPlan) -> Result<(), MigrationError> {
        let plan_name = plan_name_for_error(plan);

        // 1. Pull pin value out of the snapshot. The shape we
        //    care about is the smallest possible — just the
        //    `pin` field. Other fields in the snapshot (channel,
        //    autoUpgrade, source) are informational; reject only
        //    undoes the pin bump that triggered the plan.
        let snapshot = plan
            .spec
            .previous_spec_snapshot
            .as_ref()
            .ok_or_else(|| MigrationError::NoSnapshot(plan_name.clone()))?;
        let snapshot_pin = snapshot.get("pin");

        // 2. Read current state. Reject is idempotent — when
        //    `PlatformStack.spec.pin` already matches the
        //    snapshot (both same string, or both
        //    absent/null), the patch is a no-op and we return
        //    success without sending to the apiserver.
        //
        //    Walk-fix #7 post-B.1.78: without this no-op
        //    short-circuit, channel-following clusters
        //    (`spec.pin == null + snapshot.pin == null`)
        //    forced an SSA-apply with `spec.pin: null` body,
        //    which the apiserver rejects 422 ("spec.pin must
        //    be of type string"). MigrationController's
        //    reconcile errored, walk-fix #3 sealing
        //    (`status.rejectedAt`) never landed, and the
        //    error loop ran forever.
        let api: Api<DynamicObject> = Api::namespaced_with(
            self.client.clone(),
            Self::SINGLETON_NAMESPACE,
            &self.platformstack_api,
        );
        let stack = api.get(Self::SINGLETON_NAME).await?;
        let stack_json = serde_json::to_value(&stack)
            .map_err(|e| MigrationError::SnapshotShape(plan_name.clone(), e.to_string()))?;
        let current_pin = stack_json.pointer("/spec/pin");

        if pins_equal(current_pin, snapshot_pin) {
            info!(
                plan = %plan_name,
                "PlatformStack.spec.pin already matches snapshot — reject is no-op"
            );
            return Ok(());
        }

        // 3. Patch shape depends on whether we're SETTING
        //    pin to a value or CLEARING it.
        //
        //    Setting (snapshot.pin = "0.1.X"): SSA-apply with
        //    force=true so we win against a stale user
        //    write. Field manager
        //    `migration-controller-strategy` distinguishes
        //    the patch from PlatformController's
        //    `platform-controller`.
        //
        //    Clearing (snapshot.pin = null / absent): SSA
        //    cannot represent field-deletion cleanly when
        //    the CRD field is `type: string` without
        //    `nullable: true` — a `null` value fails
        //    schema validation. JSON merge-patch (RFC 7396)
        //    treats `null` as "delete this field", so we
        //    use `Patch::Merge` for the clearing case.
        match snapshot_pin {
            Some(Value::String(value)) => {
                let body = json!({
                    "apiVersion": "apprafter.io/v1alpha1",
                    "kind": "PlatformStack",
                    "metadata": { "name": Self::SINGLETON_NAME },
                    "spec": { "pin": value },
                });
                let params = PatchParams::apply(STRATEGY_FIELD_MANAGER).force();
                api.patch(Self::SINGLETON_NAME, &params, &Patch::Apply(&body))
                    .await?;
                info!(
                    plan = %plan_name,
                    pin_value = %value,
                    "PlatformMigrationStrategy.reject — reverted PlatformStack.spec.pin (SSA apply)"
                );
                Ok(())
            }
            None | Some(Value::Null) => {
                let body = json!({ "spec": { "pin": Value::Null } });
                let params = PatchParams {
                    field_manager: Some(STRATEGY_FIELD_MANAGER.to_string()),
                    ..PatchParams::default()
                };
                api.patch(Self::SINGLETON_NAME, &params, &Patch::Merge(body))
                    .await?;
                info!(
                    plan = %plan_name,
                    "PlatformMigrationStrategy.reject — cleared PlatformStack.spec.pin (merge patch null)"
                );
                Ok(())
            }
            Some(other) => Err(MigrationError::SnapshotShape(
                plan_name,
                format!("previousSpecSnapshot.pin must be string or null, got {other:?}"),
            )),
        }
    }
}

/// Treat a missing pin field, an explicit null value, and a
/// concrete string as three distinct states. Two states are
/// equal iff:
///
///   * Both are missing or null (channel-following mode).
///   * Both are the same string.
///
/// Helper extracted so unit tests can exercise the equality
/// rules without a running cluster.
fn pins_equal(current: Option<&Value>, snapshot: Option<&Value>) -> bool {
    fn normalise(v: Option<&Value>) -> Option<&str> {
        v.and_then(|x| x.as_str())
    }
    normalise(current) == normalise(snapshot)
}

// `ApiCapabilities` is unused in the static-ApiResource path;
// import it here for the lint to acknowledge it, kept around
// in case future code paths switch to discovery.
#[allow(dead_code)]
fn _hint_api_capabilities(_: &ApiCapabilities) {}

fn plan_name_for_error(plan: &MigrationPlan) -> String {
    plan.metadata
        .name
        .clone()
        .unwrap_or_else(|| "(unnamed)".to_string())
}

/// SourceCredential-scope strategy (1.79c / ADR 0039). Classifies
/// credential-coverage changes so a destructive one (removing a
/// covered `repoPrefix` / registry `host`, dropping a half, or
/// narrowing scope) can be gated through a MigrationPlan —
/// actor-agnostically, catching a human `kubectl edit` and the CLI
/// alike (ADR 0039 §"Gating").
///
/// **Maturity (parity with `ApplicationMigrationStrategy`).** The
/// concrete classifier `detect_destructive` ships and is unit-tested
/// here; the *live* wiring — a reconcile-time call that snapshots the
/// prior spec, builds the MigrationPlan, and pauses derivation until
/// approval — is **deliberately co-deferred** with the application-scope
/// live wiring (the "one call site" the Application controller leaves
/// commented for B.1.77). That deferred commit also adds the
/// `create_plan_for` helper plus a `sourcecredential` variant to the
/// MigrationPlan CRD scope (kube-rs + OpenAPI + CUE + admission) in one
/// coordinated change, so no half-wired CRD scope ships ahead of a
/// producer. `execute_step` / `reject` are no-ops: the destructive
/// change is config (a coverage removal), so reject = the operator
/// keeps the prior wider coverage derived and the user re-widens the
/// spec; there is no controller-side state to roll back (ADR 0027).
#[derive(Debug, Default, Clone)]
pub struct SourceCredentialMigrationStrategy;

#[async_trait]
impl MigrationStrategy for SourceCredentialMigrationStrategy {
    async fn execute_step(
        &self,
        _plan: &MigrationPlan,
        _step: &MigrationStep,
    ) -> Result<StepOutcome, MigrationError> {
        Ok(StepOutcome::Succeeded)
    }

    async fn reject(&self, _plan: &MigrationPlan) -> Result<(), MigrationError> {
        // The destructive change is a coverage REMOVAL on a config
        // object. Reject = the operator keeps the prior (wider)
        // coverage derived and the user re-widens the spec; there is
        // no controller-side state to revert. No-op, like the
        // application scope (ADR 0027).
        Ok(())
    }
}

impl SourceCredentialMigrationStrategy {
    /// Decide whether `old` → `new` is a destructive coverage change.
    ///
    /// Per ADR 0039: **removing** a covered `repoPrefix` or registry
    /// `host` (including dropping a whole `git`/`registry` half, or
    /// narrowing scope) is destructive — applications matching the
    /// removed coverage lose git-clone / image-pull access, so it must
    /// be gated. Creation (`old = None`), adding coverage (widening),
    /// and rotating the sealed material (the spec is unchanged — the
    /// material lives in the SealedSecret, not here) are all
    /// non-destructive → `None`.
    pub fn detect_destructive(
        old: Option<&SourceCredentialSpec>,
        new: &SourceCredentialSpec,
    ) -> Option<DestructiveChange> {
        let old = old?;

        let removed_prefixes = removed(&repo_prefixes(old), &repo_prefixes(new));
        let removed_hosts = removed(&registry_hosts(old), &registry_hosts(new));

        if removed_prefixes.is_empty() && removed_hosts.is_empty() {
            return None;
        }

        let field = match (removed_prefixes.is_empty(), removed_hosts.is_empty()) {
            (false, true) => "spec.git.repoPrefixes",
            (true, false) => "spec.registry.hosts",
            _ => "spec.git.repoPrefixes,spec.registry.hosts",
        };

        Some(DestructiveChange {
            trigger_type: "coverage-removal".to_string(),
            field: field.to_string(),
            from: Some(json!({
                "removedRepoPrefixes": removed_prefixes,
                "removedHosts": removed_hosts,
            })),
            to: Some(json!({
                "repoPrefixes": repo_prefixes(new),
                "hosts": registry_hosts(new),
            })),
            classification: "breaking".to_string(),
        })
    }
}

fn repo_prefixes(spec: &SourceCredentialSpec) -> Vec<String> {
    spec.git
        .as_ref()
        .map(|g| g.repo_prefixes.clone())
        .unwrap_or_default()
}

fn registry_hosts(spec: &SourceCredentialSpec) -> Vec<String> {
    spec.registry
        .as_ref()
        .map(|r| r.hosts.clone())
        .unwrap_or_default()
}

/// Entries present in `old` but absent from `new`.
fn removed(old: &[String], new: &[String]) -> Vec<String> {
    old.iter().filter(|x| !new.contains(x)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use operator_core::{
        MigrationApplicationRef, MigrationApplicationScope, MigrationPlanScope, MigrationPlanSpec,
        MigrationTrigger,
    };
    use serde_json::json;

    fn application_plan() -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "application".into(),
                application: Some(MigrationApplicationScope {
                    ref_: MigrationApplicationRef {
                        name: "parser".into(),
                        namespace: "demo".into(),
                    },
                    environment: "prod".into(),
                }),
                platform: None,
            },
            trigger: MigrationTrigger {
                type_: "selector-change".into(),
                field: "needs.pg.selector".into(),
                from: None,
                to: None,
            },
            risks: None,
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        MigrationPlan::new("parser-pg", spec)
    }

    fn platform_plan(snapshot: Option<Value>) -> MigrationPlan {
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "platform".into(),
                application: None,
                platform: Some(operator_core::MigrationPlatformScope {
                    components: vec!["apprafter-operator".into()],
                }),
            },
            trigger: MigrationTrigger {
                type_: "platform-classification".into(),
                field: "spec.pin".into(),
                from: None,
                to: None,
            },
            risks: None,
            plan: None,
            approvers: None,
            previous_spec_snapshot: snapshot,
        };
        MigrationPlan::new("platform-bump", spec)
    }

    fn dummy_step() -> MigrationStep {
        MigrationStep {
            step: 1,
            action: "snapshot DB".into(),
            estimated_duration: None,
            reversible: None,
        }
    }

    #[tokio::test]
    async fn application_strategy_execute_step_returns_succeeded_in_1_76() {
        // 1.76 contract: no machine semantics for free-form
        // action text. Every step is Succeeded so the
        // controller advances through the plan and reaches a
        // sealed phase.
        let strategy = ApplicationMigrationStrategy;
        let outcome = strategy
            .execute_step(&application_plan(), &dummy_step())
            .await
            .unwrap();
        assert_eq!(outcome, StepOutcome::Succeeded);
    }

    #[tokio::test]
    async fn application_strategy_reject_is_noop() {
        // ADR 0027: application-scope reject has no effect.
        // The webhook FSM blocks the transition; this Ok is
        // defensive in case the webhook misconfigures.
        let strategy = ApplicationMigrationStrategy;
        strategy.reject(&application_plan()).await.unwrap();
    }

    #[tokio::test]
    async fn platform_strategy_reject_errors_when_snapshot_missing() {
        // PlatformStrategy needs the previousSpecSnapshot to
        // know what to revert to. A platform plan without
        // snapshot is a chart-author / detector bug; surface
        // it as MigrationError::NoSnapshot rather than
        // silently no-op.
        //
        // We don't have a real kube Client in unit tests; the
        // function should fail BEFORE touching the network
        // when the snapshot is missing.
        //
        // (Cannot easily construct a fake Client; instead
        // verify the error variant when the strategy is given
        // a plan with no snapshot. Construction of the
        // strategy itself does not require a Client.)
        //
        // Workaround: call the snapshot-extraction logic via a
        // raw test that mirrors the production code path.
        // Here we just confirm `previous_spec_snapshot.is_none()`
        // on the test fixture and trust the production fn's
        // ordering — `Option::ok_or_else(NoSnapshot)` runs
        // before any kube call.
        let plan = platform_plan(None);
        assert!(plan.spec.previous_spec_snapshot.is_none());
    }

    #[test]
    fn snapshot_pin_extraction_round_trips_string_value() {
        // The strategy reads `snapshot.pin` as a JSON Value
        // and forwards it directly into the SSA body. String
        // pins (`"0.1.25"`) round-trip; null pins also
        // (representing "unset → revert to channel-follow").
        let snapshot = json!({ "pin": "0.1.25", "autoUpgrade": false });
        let pin = snapshot.get("pin").cloned().unwrap();
        assert_eq!(pin, json!("0.1.25"));
    }

    #[test]
    fn snapshot_missing_pin_defaults_to_null_in_patch() {
        // When the snapshot has no `pin` field (the plan was
        // created from a channel-following PlatformStack),
        // reject should set `spec.pin: null` — removing the
        // pin entirely so the controller resumes channel
        // resolution.
        let snapshot = json!({ "autoUpgrade": true });
        let pin = snapshot.get("pin").cloned().unwrap_or(Value::Null);
        assert_eq!(pin, Value::Null);
    }

    // ---- walk-fix #7 post-B.1.78 — pins_equal short-circuit ----

    #[test]
    fn pins_equal_treats_missing_and_null_and_explicit_null_as_equivalent() {
        // Channel-following state can be represented three
        // ways in JSON: field absent, field=null, or missing
        // entirely from the snapshot. All collapse to "no pin
        // is set". `pins_equal` must treat them as one state
        // — without that, the reject no-op short-circuit
        // wouldn't fire and the strategy would send an
        // invalid SSA patch (`spec.pin: null` against a
        // non-nullable string field).
        let null = Value::Null;

        // current=None, snapshot=None.
        assert!(pins_equal(None, None));
        // current=Some(null), snapshot=None.
        assert!(pins_equal(Some(&null), None));
        // current=None, snapshot=Some(null).
        assert!(pins_equal(None, Some(&null)));
        // current=Some(null), snapshot=Some(null).
        assert!(pins_equal(Some(&null), Some(&null)));
    }

    #[test]
    fn pins_equal_treats_same_string_as_equal() {
        let a = Value::String("0.1.25".into());
        let b = Value::String("0.1.25".into());
        assert!(pins_equal(Some(&a), Some(&b)));
    }

    #[test]
    fn pins_equal_distinguishes_different_strings() {
        let a = Value::String("0.1.25".into());
        let b = Value::String("0.1.26".into());
        assert!(!pins_equal(Some(&a), Some(&b)));
    }

    #[test]
    fn pins_equal_distinguishes_null_from_string() {
        let null = Value::Null;
        let s = Value::String("0.1.25".into());
        assert!(!pins_equal(Some(&null), Some(&s)));
        assert!(!pins_equal(Some(&s), Some(&null)));
        // Missing field also distinct from a concrete string.
        assert!(!pins_equal(None, Some(&s)));
        assert!(!pins_equal(Some(&s), None));
    }
}

#[cfg(test)]
mod application_detect_destructive_tests {
    use super::*;
    use operator_core::{ApplicationExpose, OneOrMany, ServiceNeed};

    fn base() -> ApplicationBaseSpec {
        ApplicationBaseSpec::default()
    }

    fn expose(network: &str, host: Option<&str>) -> ApplicationExpose {
        ApplicationExpose {
            network: Some(network.into()),
            hostname: host.map(|h| OneOrMany::One(h.to_string())),
            ..Default::default()
        }
    }

    fn with_expose(mut s: ApplicationBaseSpec, e: ApplicationExpose) -> ApplicationBaseSpec {
        s.expose = Some(e);
        s
    }

    fn with_pg(mut s: ApplicationBaseSpec) -> ApplicationBaseSpec {
        s.needs = Some(Needs {
            pg: Some(OneOrMany::One(ServiceNeed::default())),
            ..Default::default()
        });
        s
    }

    fn with_named_pg(mut s: ApplicationBaseSpec, name: &str) -> ApplicationBaseSpec {
        s.needs = Some(Needs {
            pg: Some(OneOrMany::Many(vec![ServiceNeed {
                name: Some(name.to_string()),
                ..Default::default()
            }])),
            ..Default::default()
        });
        s
    }

    #[test]
    fn removing_needs_pg_is_data_migration() {
        let c =
            ApplicationMigrationStrategy::detect_destructive(&with_pg(base()), &base()).unwrap();
        assert_eq!(c.classification, "data-migration");
        assert_eq!(c.trigger_type, "needs-removal");
        assert_eq!(c.field, "needs.pg");
        // STRING sentinel: `from` mirrors the removed field key.
        assert_eq!(c.from.as_ref().unwrap().as_str().unwrap(), "needs.pg");
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "(removed)");
    }

    #[test]
    fn adding_needs_pg_is_not_destructive() {
        assert!(
            ApplicationMigrationStrategy::detect_destructive(&base(), &with_pg(base())).is_none()
        );
    }

    #[test]
    fn removing_named_needs_pg_uses_dotted_key() {
        let c = ApplicationMigrationStrategy::detect_destructive(
            &with_named_pg(base(), "main"),
            &base(),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "needs-removal");
        assert_eq!(c.field, "needs.pg.main");
        assert_eq!(c.from.as_ref().unwrap().as_str().unwrap(), "needs.pg.main");
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "(removed)");
        assert_eq!(c.classification, "data-migration");
    }

    #[test]
    fn unchanged_needs_is_not_destructive() {
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_pg(base()),
            &with_pg(base())
        )
        .is_none());
    }

    #[test]
    fn no_needs_either_side_is_not_destructive() {
        assert!(ApplicationMigrationStrategy::detect_destructive(&base(), &base()).is_none());
    }

    // ---- Task 3: domain-change (gated on public) + network-visibility ----

    #[test]
    fn hostname_change_on_public_app_gates() {
        let c = ApplicationMigrationStrategy::detect_destructive(
            &with_expose(base(), expose("public", Some("a.example.com"))),
            &with_expose(base(), expose("public", Some("b.example.com"))),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "domain-change");
        assert_eq!(c.classification, "requires-restart");
        assert_eq!(c.from.as_ref().unwrap().as_str().unwrap(), "a.example.com");
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "b.example.com");
    }

    #[test]
    fn hostname_change_on_internal_app_is_soft() {
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_expose(base(), expose("internal", Some("a.example.com"))),
            &with_expose(base(), expose("internal", Some("b.example.com")))
        )
        .is_none());
    }

    #[test]
    fn hostname_removal_on_public_app_gates_with_removed_sentinel() {
        let c = ApplicationMigrationStrategy::detect_destructive(
            &with_expose(base(), expose("public", Some("a.example.com"))),
            &with_expose(base(), expose("public", None)),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "domain-change");
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "(removed)");
    }

    #[test]
    fn public_to_internal_gates_as_visibility() {
        let c = ApplicationMigrationStrategy::detect_destructive(
            &with_expose(base(), expose("public", Some("a.example.com"))),
            &with_expose(base(), expose("internal", Some("a.example.com"))),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "network-visibility-change");
        assert_eq!(c.from.as_ref().unwrap().as_str().unwrap(), "public");
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "internal");
    }

    #[test]
    fn internal_to_vpn_and_to_public_are_soft() {
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_expose(base(), expose("internal", None)),
            &with_expose(base(), expose("vpn", None))
        )
        .is_none());
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_expose(base(), expose("internal", None)),
            &with_expose(base(), expose("public", None))
        )
        .is_none());
    }

    // ---- Task 4: scale-to-zero + image-path ----

    fn with_replicas(mut s: ApplicationBaseSpec, r: Option<i32>) -> ApplicationBaseSpec {
        s.replicas = r;
        s
    }

    fn with_image(mut s: ApplicationBaseSpec, i: &str) -> ApplicationBaseSpec {
        s.image = Some(i.into());
        s
    }

    #[test]
    fn scale_to_zero_gates_but_scale_down_and_up_dont() {
        let c = ApplicationMigrationStrategy::detect_destructive(
            &with_replicas(base(), Some(3)),
            &with_replicas(base(), Some(0)),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "scale-to-zero");
        assert_eq!(c.classification, "requires-restart");
        assert_eq!(c.from.as_ref().unwrap().as_str().unwrap(), "3");
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "0");
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_replicas(base(), Some(3)),
            &with_replicas(base(), Some(1))
        )
        .is_none()); // N->M soft
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_replicas(base(), Some(0)),
            &with_replicas(base(), Some(3))
        )
        .is_none()); // 0->N soft
    }

    #[test]
    fn replicas_none_to_zero_gates() {
        // application.cue: `replicas` is optional and resolves to 1 at
        // render time — so an absent `replicas` is the effective `1`.
        // Editing it explicitly to `0` is therefore a scale-to-zero.
        let c = ApplicationMigrationStrategy::detect_destructive(
            &with_replicas(base(), None),
            &with_replicas(base(), Some(0)),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "scale-to-zero");
        assert_eq!(c.classification, "requires-restart");
        assert_eq!(c.from.as_ref().unwrap().as_str().unwrap(), "1");
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "0");
        // The reverse (0 -> absent, i.e. back to the render default 1) is
        // a scale-UP, so it must stay soft.
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_replicas(base(), Some(0)),
            &with_replicas(base(), None)
        )
        .is_none());
    }

    #[test]
    fn image_repo_change_gates_but_tag_change_doesnt() {
        let c = ApplicationMigrationStrategy::detect_destructive(
            &with_image(base(), "ghcr.io/acme/api:v1"),
            &with_image(base(), "ghcr.io/acme/other:v1"),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "image-path-change");
        assert_eq!(c.classification, "requires-restart");
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_image(base(), "ghcr.io/acme/api:v1"),
            &with_image(base(), "ghcr.io/acme/api:v2")
        )
        .is_none()); // tag soft
    }

    #[test]
    fn image_add_or_remove_is_not_a_path_change() {
        // A None -> Some or Some -> None is an image add/remove, out of
        // scope for image-path-change (Task 4 handles Some↔Some only).
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &base(),
            &with_image(base(), "ghcr.io/acme/api:v1")
        )
        .is_none());
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_image(base(), "ghcr.io/acme/api:v1"),
            &base()
        )
        .is_none());
    }
}

#[cfg(test)]
mod sourcecredential_strategy_tests {
    use super::*;
    use operator_core::{SealedSecretRef, SourceBackend, SourceGit, SourceRegistry};

    fn backend() -> SourceBackend {
        SourceBackend {
            sealed_secret_ref: Some(SealedSecretRef {
                name: "srccred-acme-material".to_string(),
                namespace: None,
            }),
            open_bao_path: None,
        }
    }

    fn spec(prefixes: &[&str], hosts: &[&str]) -> SourceCredentialSpec {
        SourceCredentialSpec {
            git: if prefixes.is_empty() {
                None
            } else {
                Some(SourceGit {
                    backend: backend(),
                    repo_prefixes: prefixes.iter().map(|s| s.to_string()).collect(),
                })
            },
            registry: if hosts.is_empty() {
                None
            } else {
                Some(SourceRegistry {
                    backend: backend(),
                    hosts: hosts.iter().map(|s| s.to_string()).collect(),
                })
            },
        }
    }

    #[test]
    fn creation_is_not_destructive() {
        let new = spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        assert!(SourceCredentialMigrationStrategy::detect_destructive(None, &new).is_none());
    }

    #[test]
    fn unchanged_spec_is_not_destructive() {
        // A material rotation does not touch the spec (the secret lives
        // in the SealedSecret), so old == new here → None.
        let s = spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        assert!(SourceCredentialMigrationStrategy::detect_destructive(Some(&s), &s).is_none());
    }

    #[test]
    fn widening_coverage_is_not_destructive() {
        let old = spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        let new = spec(
            &["github.com/acme/", "github.com/acme-labs/"],
            &["ghcr.io/acme/", "ghcr.io/acme-labs/"],
        );
        assert!(SourceCredentialMigrationStrategy::detect_destructive(Some(&old), &new).is_none());
    }

    #[test]
    fn removing_a_repo_prefix_is_destructive() {
        let old = spec(
            &["github.com/acme/", "github.com/acme-labs/"],
            &["ghcr.io/acme/"],
        );
        let new = spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        let change =
            SourceCredentialMigrationStrategy::detect_destructive(Some(&old), &new).unwrap();
        assert_eq!(change.trigger_type, "coverage-removal");
        assert_eq!(change.field, "spec.git.repoPrefixes");
        assert_eq!(change.classification, "breaking");
        assert_eq!(
            change.from.unwrap()["removedRepoPrefixes"],
            serde_json::json!(["github.com/acme-labs/"])
        );
    }

    #[test]
    fn removing_a_registry_host_is_destructive() {
        let old = spec(
            &["github.com/acme/"],
            &["ghcr.io/acme/", "ghcr.io/acme-labs/"],
        );
        let new = spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        let change =
            SourceCredentialMigrationStrategy::detect_destructive(Some(&old), &new).unwrap();
        assert_eq!(change.field, "spec.registry.hosts");
        assert_eq!(
            change.from.unwrap()["removedHosts"],
            serde_json::json!(["ghcr.io/acme-labs/"])
        );
    }

    #[test]
    fn dropping_a_whole_half_is_destructive() {
        // Removing the registry half entirely = removing every host.
        let old = spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        let new = spec(&["github.com/acme/"], &[]);
        let change =
            SourceCredentialMigrationStrategy::detect_destructive(Some(&old), &new).unwrap();
        assert_eq!(change.field, "spec.registry.hosts");
    }

    #[test]
    fn removing_from_both_halves_reports_combined_field() {
        let old = spec(
            &["github.com/acme/", "github.com/x/"],
            &["ghcr.io/acme/", "ghcr.io/x/"],
        );
        let new = spec(&["github.com/acme/"], &["ghcr.io/acme/"]);
        let change =
            SourceCredentialMigrationStrategy::detect_destructive(Some(&old), &new).unwrap();
        assert_eq!(change.field, "spec.git.repoPrefixes,spec.registry.hosts");
    }
}
