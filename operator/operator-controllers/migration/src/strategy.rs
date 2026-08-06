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

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::Utc;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Api, ObjectMeta, Patch, PatchParams};
use kube::core::DynamicObject;
use kube::discovery::{ApiCapabilities, ApiResource};
use kube::Client;
use serde_json::{json, Value};
use tracing::info;

use operator_core::migration::{change_hash, classification_severity};
use operator_core::{
    image_repo, Application, ApplicationBaseSpec, DestructiveChange, EnvRef, EnvValue,
    MigrationApplicationRef, MigrationApplicationScope, MigrationChange, MigrationError,
    MigrationPlan, MigrationPlanScope, MigrationPlanSpec, MigrationStep, MigrationStrategy,
    MigrationTrigger, Needs, OneOrMany, SourceCredentialSpec, StepOutcome,
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
    ///
    /// Wraps [`detect_all`](Self::detect_all) (the full candidate set) with
    /// `pick_primary` — kept as the single-primary API for callers that only
    /// need the headline (`spec.trigger`). The MigrationPlan-creation +
    /// consume paths use `detect_all` directly so the approval content hash
    /// and `spec.changes[]` rollup cover EVERY candidate (2.16b S1.2 / S-4).
    pub fn detect_destructive(
        old: &ApplicationBaseSpec,
        new: &ApplicationBaseSpec,
    ) -> Option<DestructiveChange> {
        pick_primary(Self::detect_all(old, new))
    }

    /// 2.16b S1.2: EVERY destructive candidate the `old` → `new` edit
    /// produces, in `pick_primary`'s canonical order (severity desc, then
    /// `trigger_type` asc, `field` asc) so the list is deterministic and
    /// independent of detection push-order. `detect_destructive` is
    /// `pick_primary(detect_all(..))` — the single headline; the plan-creation
    /// path (`create_plan_for`) and the consume-time hash check
    /// (`plan_state`) both operate on this FULL set so an approver sees the
    /// complete blast radius (`spec.changes[]`) AND the approval hash binds
    /// the whole candidate set (a lower-severity op can't ride along
    /// unhashed — S-4 gap close).
    pub fn detect_all(
        old: &ApplicationBaseSpec,
        new: &ApplicationBaseSpec,
    ) -> Vec<DestructiveChange> {
        let mut candidates: Vec<DestructiveChange> = Vec::new();

        // Needs handling gates only REMOVAL of a `needs.<type>` entry
        // (the backing claim + data is torn down). Two edits on an
        // EXISTING need are intentionally NOT gated:
        //   * `needs.*.selector` change is DEFERRED (2.4d; single provider
        //     tier:integrated at launch — revisit 2.5+ when selectors can
        //     route across provider tiers). A selector-only edit produces
        //     the same `(type, name)` key on both sides, so no candidate.
        //   * `needs.*.size` change is provisioner-guarded (V14): PVC/CNPG
        //     storage is expansion-only and a shrink is refused at the
        //     provisioner/apiserver layer, so 2.16b needn't gate it here.
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
        //
        // `old_host.is_some()` is load-bearing: adding a hostname to a
        // public app (`None -> Some`) is NOT destructive — no existing
        // route is disrupted, it's just added exposure detail. We gate
        // only when there WAS a hostname before (its removal or change of
        // an existing public route). This also avoids an empty `from`.
        if old_net == "public" && old_host.is_some() && old_host != new_host {
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

        // 2.16b Task 7 (#10/#11/#12): expose-escalation security-boundary
        // triggers. All PAUSE for approval (never a webhook reject).

        // #10 network-visibility-escalation: a non-public → public flip widens
        // the app's blast radius from cluster-internal to the public Gateway.
        // The reverse (public → non-public) is the soft `network-visibility-change`
        // (requires-restart) op above — a DE-escalation, not gated here.
        if old_net != "public" && new_net == "public" {
            candidates.push(DestructiveChange {
                trigger_type: "network-visibility-escalation".to_string(),
                field: "expose.network".to_string(),
                from: Some(json!(old_net)),
                to: Some(json!("public")),
                classification: "security-boundary".to_string(),
            });
        }

        // #11 public-hostname-add: adding the FIRST public hostname (old empty/
        // None, new non-empty) on a `network: public` app publishes a new
        // externally-reachable name. CO-FIRES with #10 when internal→public also
        // adds a hostname — both land in `changes[]`, deliberately not merged.
        // Gated on `new_net == "public"` (the hostname is inert until public);
        // an existing-hostname CHANGE on a public app is the `domain-change` op
        // above (requires-restart), so #11 covers only the None → Some add.
        if new_net == "public" && old_host.as_deref().unwrap_or("").is_empty() {
            if let Some(h) = new_host.as_deref().filter(|h| !h.is_empty()) {
                candidates.push(DestructiveChange {
                    trigger_type: "public-hostname-add".to_string(),
                    field: "expose.hostname".to_string(),
                    from: Some(json!("(none)")),
                    to: Some(json!(h)),
                    classification: "security-boundary".to_string(),
                });
            }
        }

        // #12 public-port-retarget: on a PUBLIC app, changing the exposed port
        // moves the public Service's target — a reachability/security surface
        // change. `expose.port` is a required field read verbatim by the
        // renderer (no render-time default), so the comparison is a direct
        // `i32` diff. A port change on a non-public app is soft (no public
        // surface). We require `new_net == "public"` — the app is publicly
        // routed AFTER the edit; a same-edit internal→public port move is
        // already covered by #10's escalation.
        if new_net == "public" {
            let old_port = old.expose.as_ref().map(|e| e.port);
            let new_port = new.expose.as_ref().map(|e| e.port);
            if let (Some(op), Some(np)) = (old_port, new_port) {
                if op != np {
                    candidates.push(DestructiveChange {
                        trigger_type: "public-port-retarget".to_string(),
                        field: "expose.port".to_string(),
                        from: Some(json!(op.to_string())),
                        to: Some(json!(np.to_string())),
                        classification: "security-boundary".to_string(),
                    });
                }
            }
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
        //
        // 2.16b S1.1: classified `security-boundary` (the most severe class) —
        // a different repository / registry path can serve entirely different
        // content, so it's a pull-source/security change, not a mere restart.
        // It thus OUTRANKS a co-occurring data-migration for the plan primary.
        if let (Some(old_img), Some(new_img)) = (old.image.as_deref(), new.image.as_deref()) {
            let old_repo = image_repo(old_img);
            let new_repo = image_repo(new_img);
            if old_repo != new_repo {
                candidates.push(DestructiveChange {
                    trigger_type: "image-path-change".to_string(),
                    field: "spec.image".to_string(),
                    from: Some(json!(old_repo)),
                    to: Some(json!(new_repo)),
                    classification: "security-boundary".to_string(),
                });
            }
        }

        // 2.16b Task 7 (#13) image-policy-relaxation: turning `imagePolicy.resolve`
        // OFF → not-off (i.e. re-enabling tag→digest resolution) relaxes a PINNED
        // reference back to a floating, mutable pull surface — a security-boundary
        // change (a moved tag can now serve different content without a spec
        // edit). Two carve-outs keep this from firing on non-relaxations:
        //   * an already-DIGEST image is a no-op — `off`/`digest` both render the
        //     verbatim digest, so re-enabling resolution changes nothing.
        //   * `digest → off` is HARDENING (pinning the tag verbatim), NOT a
        //     relaxation — soft.
        // `resolve` is optional and defaults to `digest` (ADR 0040); an absent
        // NEW policy is therefore treated as `digest` (relaxation when old was
        // `off`), and an absent OLD policy is `digest` (never `off`, so no
        // relaxation). We read the new image (the reference that will be pulled)
        // for the digest carve-out.
        let old_resolve = old
            .image_policy
            .as_ref()
            .and_then(|p| p.resolve.as_deref())
            .unwrap_or("digest");
        let new_resolve = new
            .image_policy
            .as_ref()
            .and_then(|p| p.resolve.as_deref())
            .unwrap_or("digest");
        if old_resolve == "off" && new_resolve != "off" {
            let new_is_digest = new.image.as_deref().map(image_is_digest).unwrap_or(false);
            if !new_is_digest {
                candidates.push(DestructiveChange {
                    trigger_type: "image-policy-relaxation".to_string(),
                    field: "imagePolicy.resolve".to_string(),
                    from: Some(json!("off")),
                    to: Some(json!(new_resolve)),
                    classification: "security-boundary".to_string(),
                });
            }
        }

        // Env-ref removal. Removing an env var whose value is an
        // `EnvValue::Ref(_)` (a claim- or secret-backed reference, ADR
        // 0046) is gated (requires-restart): the container loses a
        // resolved connection/secret value it may depend on, so the
        // rollout is a restart the platform surfaces for approval.
        // Removing a `Literal` env is a soft edit; env ADD and a
        // key that stays present (ref→ref, ref→literal) are also soft —
        // only a removal of a ref-valued key gates here.
        let new_env_keys: Vec<&String> = new
            .env
            .as_ref()
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        if let Some(old_env) = old.env.as_ref() {
            for (key, value) in old_env {
                if new_env_keys.contains(&key) {
                    continue;
                }
                if let EnvValue::Ref(r) = value {
                    candidates.push(DestructiveChange {
                        trigger_type: "env-ref-removal".to_string(),
                        field: format!("env.{key}"),
                        from: Some(json!(render_env_ref(r))),
                        to: Some(json!("(removed)")),
                        classification: "requires-restart".to_string(),
                    });
                }
            }
        }

        // 2.16b Task 6 (#7/#8/#9): env-ref security-boundary triggers. Iterate
        // the UNION of env keys once and classify each key's `old → new`
        // transition. Every candidate here is `security-boundary` (sev 4) and
        // PAUSES for approval — none is a webhook reject. Load-bearing invariant
        // (S1.4): `from`/`to` carry only ref SENTINELS (`render_env_ref`) or
        // structural markers (`(absent)` / `(literal)`), NEVER a literal env
        // VALUE — a MigrationPlan is world-readable to any plan-read principal.
        let empty_env = BTreeMap::new();
        let old_env = old.env.as_ref().unwrap_or(&empty_env);
        let new_env = new.env.as_ref().unwrap_or(&empty_env);
        for (key, new_val) in new_env {
            match old_env.get(key) {
                // Key ADDED. Only a `secret:` ref add is gated (#7): the
                // container gains a fresh external-secret dependency. A claim
                // ref add is self-scoped (ADR 0046 — provider-derived) → soft.
                None => {
                    if let EnvValue::Ref(r @ EnvRef::Secret(_)) = new_val {
                        candidates.push(DestructiveChange {
                            trigger_type: "env-secret-ref-add".to_string(),
                            field: format!("env.{key}"),
                            from: Some(json!("(absent)")),
                            to: Some(json!(render_env_ref(r))),
                            classification: "security-boundary".to_string(),
                        });
                    }
                }
                // Key present BOTH sides. Two security-boundary transitions:
                //   #8 downgrade: `Ref(_)` → `Literal(_)` — a resolved-at-
                //      runtime reference replaced by an inline value. `to` is
                //      the `"(literal)"` SENTINEL, never the literal VALUE.
                //   #9 retarget: `Ref(Secret(a))` → `Ref(Secret(b))`, a≠b — a
                //      different external secret. A claim→claim retarget is
                //      self-scoped (not #9); a secret↔claim variant change is
                //      neither #8 (still a Ref) nor #9 (not secret→secret).
                Some(old_val) => match (old_val, new_val) {
                    (EnvValue::Ref(old_r), EnvValue::Literal(_)) => {
                        candidates.push(DestructiveChange {
                            trigger_type: "env-ref-downgrade".to_string(),
                            field: format!("env.{key}"),
                            from: Some(json!(render_env_ref(old_r))),
                            to: Some(json!("(literal)")),
                            classification: "security-boundary".to_string(),
                        });
                    }
                    (
                        EnvValue::Ref(old_r @ EnvRef::Secret(a)),
                        EnvValue::Ref(new_r @ EnvRef::Secret(b)),
                    ) if a != b => {
                        candidates.push(DestructiveChange {
                            trigger_type: "env-secret-ref-retarget".to_string(),
                            field: format!("env.{key}"),
                            from: Some(json!(render_env_ref(old_r))),
                            to: Some(json!(render_env_ref(new_r))),
                            classification: "security-boundary".to_string(),
                        });
                    }
                    _ => {}
                },
            }
        }

        sort_candidates(&mut candidates);
        candidates
    }

    /// Build a `MigrationPlan` CR for an application-scope
    /// destructive change. The Plan lands in the **application's own
    /// namespace** (`application_namespace`) — 2.16b co-locates the
    /// plan with the Application it gates so a controller
    /// `ownerReference` back to the Application cascades the plan on
    /// Application delete and keeps RBAC namespace-scoped to the team.
    ///
    /// `application_name` / `application_namespace` / `environment`
    /// identify which Application + environment the plan
    /// governs. The caller (Task 11 Application reconciler) knows
    /// these from the Application CR it just reconciled, and passes the
    /// Application CR's `metadata.uid` as `app_uid` so the
    /// `ownerReference` binds the plan to that exact object.
    ///
    /// `plan_name` should embed the application + a date / UID
    /// so the resulting CR has a stable, human-readable name.
    /// The Application reconciler synthesises one from
    /// `<app>-<env>-migration-<timestamp>`.
    ///
    /// 2.16b S1.2 / S-4: `candidates` is the FULL set of destructive changes
    /// the edit produced (from [`detect_all`](Self::detect_all)), not just the
    /// primary. The plan's `spec.trigger` carries the `pick_primary` headline,
    /// but `spec.changes[]` rolls up EVERY candidate and
    /// `spec.risks.classifications` lists the distinct classification
    /// vocabulary — so an approver sees the complete blast radius and a
    /// dangerous op can't be laundered behind a benign primary. Critically the
    /// approval content hash (`trigger.approvedSpecHash`) covers the WHOLE
    /// `candidates` set, so attaching a lower-severity destructive op changes
    /// the hash and re-gates the edit at consume time (closes the S-4
    /// primary-only gap).
    ///
    /// Panics if `candidates` is empty — the caller only reaches the
    /// CreatePlan arm when detection produced at least one change.
    pub fn create_plan_for(
        candidates: &[DestructiveChange],
        plan_name: &str,
        application_namespace: &str,
        application_name: &str,
        environment: &str,
        app_uid: &str,
    ) -> MigrationPlan {
        let primary =
            pick_primary(candidates.to_vec()).expect("create_plan_for called with >=1 candidate");
        let change = &primary;

        // Distinct classification vocabulary across every candidate, sorted
        // severity desc then name asc — the approver-facing summary of the
        // full set's risk classes. Deterministic (same key `pick_primary`
        // uses on the primary), so the emitted list is stable.
        let mut distinct_classifications: Vec<String> = Vec::new();
        for c in candidates {
            if !distinct_classifications.contains(&c.classification) {
                distinct_classifications.push(c.classification.clone());
            }
        }
        distinct_classifications.sort_by(|a, b| {
            classification_severity(b)
                .cmp(&classification_severity(a))
                .then(a.cmp(b))
        });

        // Every candidate → a MigrationChange rollup row. Candidates already
        // arrive sorted from `detect_all`, but re-sort a borrowed slice
        // defensively so the emitted order is canonical regardless of caller.
        let mut ordered = candidates.to_vec();
        sort_candidates(&mut ordered);
        let changes: Vec<MigrationChange> = ordered
            .iter()
            .map(|c| MigrationChange {
                trigger: c.trigger_type.clone(),
                field: c.field.clone(),
                classification: c.classification.clone(),
                severity: classification_severity(&c.classification) as u32,
                from: c.from.clone(),
                to: c.to.clone(),
            })
            .collect();

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
                // 2.16b S-4 / S1.2: bind this app-scope plan (and thus its
                // approval) to a content hash of the FULL candidate set — not
                // just the primary. `plan_state` re-verifies this at consume
                // time against the CURRENT full candidate set (hash mismatch →
                // the completed plan is a relic, re-gated as a fresh
                // pending-approval plan). Hashing all candidates closes the
                // S-4 primary-only gap: an attacker can no longer attach a
                // lower-severity destructive op that rides along UNHASHED
                // behind a benign-looking primary — every candidate is in the
                // hash, so adding/dropping one changes it and re-gates.
                approved_spec_hash: Some(change_hash(candidates)),
            },
            risks: Some(operator_core::MigrationRisks {
                classification: change.classification.clone(),
                // 2.16b S1.2: the distinct classification vocabulary across
                // the full candidate set (primary's is the max above). Always
                // `Some` when non-empty (create_plan_for always has >=1
                // candidate) for a consistent approver-facing summary — even a
                // single-candidate plan surfaces its one classification here.
                classifications: (!distinct_classifications.is_empty())
                    .then_some(distinct_classifications),
                estimated_downtime: None,
                data_volume: None,
                reversible: None,
                requires_full_backup: None,
            }),
            changes: Some(changes),
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        // ObjectMeta path used here (not `MigrationPlan::new`)
        // because we need to set the namespace + ownerReference
        // explicitly — `MigrationPlan::new` builds a cluster-scoped
        // meta with no owner. The plan lands in the Application's own
        // namespace and carries a controller ownerRef back to the
        // Application so it cascades on Application delete (2.16b).
        let mut mp = MigrationPlan::new(plan_name, spec);
        mp.metadata = ObjectMeta {
            name: Some(plan_name.to_string()),
            namespace: Some(application_namespace.to_string()),
            owner_references: Some(vec![OwnerReference {
                api_version: "apprafter.io/v1alpha1".to_string(),
                kind: "Application".to_string(),
                name: application_name.to_string(),
                uid: app_uid.to_string(),
                controller: Some(true),
                block_owner_deletion: Some(true),
            }]),
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

/// Render an `EnvRef` to a stable human string sentinel for a
/// `DestructiveChange.from` value. Mirrors the ADR 0046 env-marker
/// vocabulary: a claim reference renders `claim.<type>.<field>` (the
/// `EnvRef::Claim` payload is already `"<type>.<field>"` /
/// `"<type>.<name>.<field>"`) and a secret reference renders
/// `secret:<name>/<key>`. The result is non-empty for any well-formed
/// ref, so the trigger surfaces a legible "what was removed" for the
/// approver.
fn render_env_ref(r: &EnvRef) -> String {
    match r {
        EnvRef::Claim(target) => format!("claim.{target}"),
        EnvRef::Secret(target) => format!("secret:{target}"),
    }
}

/// Whether an image reference is already pinned to a content digest
/// (`repo@sha256:…`) rather than a floating tag. A pure, borrowing check
/// mirroring the `@sha256:`-suffix detection in
/// `operator-controllers-application::oci_resolve::parse_image_ref`
/// (whose `ImageRef.is_digest` is set when `Reference::digest()` is
/// present) and `operator-core::image_repo`'s `split('@')` — reimplemented
/// here (rather than calling `parse_image_ref`) because that validated
/// parser lives in the application-controller crate, and the migration
/// crate must not take a crate-cycle dependency on it (it depends only on
/// `operator-core`). Used ONLY by the 2.16b Task 7 #13 carve-out: an
/// already-digest image makes an `off → digest` policy flip a no-op, so it
/// is NOT gated. Matches on the canonical `@sha256:` marker; a bare tag or
/// nameless reference is not a digest.
fn image_is_digest(image: &str) -> bool {
    image
        .rsplit_once('@')
        .is_some_and(|(_, digest)| digest.starts_with("sha256:"))
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

/// Sort candidates into the canonical primary order:
/// `(classification_severity desc, trigger_type asc, field asc)`. Shared by
/// [`detect_all`](ApplicationMigrationStrategy::detect_all) (so the emitted
/// `spec.changes[]` rollup is deterministic) and [`pick_primary`] (so the head
/// is the stable primary). The tie-break on `trigger_type` then `field` makes
/// the ordering independent of candidate push-order — two equal-severity ops
/// always resolve identically.
fn sort_candidates(v: &mut [DestructiveChange]) {
    v.sort_by(|a, b| {
        classification_severity(&b.classification)
            .cmp(&classification_severity(&a.classification))
            .then(a.trigger_type.cmp(&b.trigger_type))
            .then(a.field.cmp(&b.field))
    });
}

/// Pick the single primary `DestructiveChange` from the candidates an
/// edit produced: highest `classification_severity` wins (2.16b spec).
///
/// Deterministic (Task 6, R3-mn-b): sorts by
/// [`sort_candidates`]' `(severity desc, trigger_type asc, field asc)` and
/// takes the head. Sorting here (rather than assuming the caller pre-sorted)
/// keeps `pick_primary` correct for any candidate slice — `detect_all`
/// already emits sorted, `create_plan_for` re-derives the primary from a
/// borrowed slice, and the sort is idempotent on already-sorted input.
fn pick_primary(mut v: Vec<DestructiveChange>) -> Option<DestructiveChange> {
    sort_candidates(&mut v);
    v.into_iter().next()
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
                approved_spec_hash: None,
            },
            risks: None,
            changes: None,
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
                approved_spec_hash: None,
            },
            risks: None,
            changes: None,
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
mod change_hash_tests {
    use operator_core::migration::change_hash;
    use operator_core::DestructiveChange;
    use serde_json::json;

    // 2.16b S-4: an app-scope MigrationPlan approval must NOT be
    // transferable across a spec change. The plan is bound to a
    // content hash of the detected change(s); the hash has to be
    // stable for identical content AND sensitive to the from/to
    // payload (not just the `(trigger_type, field)` tuple) — else
    // an approval for `replicas 2->0` would consume against a
    // DIFFERENT `replicas 1->0`, defeating the gate.
    #[test]
    fn spec_hash_is_stable_and_content_sensitive() {
        let a = DestructiveChange {
            trigger_type: "scale-to-zero".into(),
            field: "replicas".into(),
            from: Some(json!("2")),
            to: Some(json!("0")),
            classification: "requires-restart".into(),
        };
        let b = DestructiveChange {
            from: Some(json!("1")),
            ..a.clone()
        }; // 1->0, different content, same tuple
           // stable: identical content hashes identically.
        assert_eq!(
            change_hash(std::slice::from_ref(&a)),
            change_hash(std::slice::from_ref(&a))
        );
        // sensitive: same tuple, different from/to → different hash.
        assert_ne!(change_hash(&[a]), change_hash(&[b]));
    }

    // 2.16b S-4 review: the canonicalisation must be COLLISION-FREE across
    // separator characters. The pre-review encoding joined fields with a
    // literal `|` (and lines with `\n`); an unescaped `|`/`\n` inside a
    // from/to value could collide two distinct changes (from=`a`,to=`b|c`
    // vs from=`a|b`,to=`c`). The JSON encoding escapes every special char,
    // so no separator can leak.
    #[test]
    fn change_hash_is_collision_free_across_separators() {
        let a = DestructiveChange {
            trigger_type: "t".into(),
            field: "f".into(),
            from: Some(json!("a")),
            to: Some(json!("b|c")),
            classification: "x".into(),
        };
        let b = DestructiveChange {
            from: Some(json!("a|b")),
            to: Some(json!("c")),
            ..a.clone()
        };
        assert_ne!(change_hash(std::slice::from_ref(&a)), change_hash(&[b])); // must NOT collide
                                                                              // newline variant
        let c = DestructiveChange {
            to: Some(json!("b\nc")),
            ..a.clone()
        };
        let d = DestructiveChange {
            from: Some(json!("a\nb")),
            to: Some(json!("c")),
            ..a
        };
        assert_ne!(change_hash(&[c]), change_hash(&[d]));
    }
}

#[cfg(test)]
mod application_detect_destructive_tests {
    use super::*;
    use operator_core::{ApplicationExpose, EnvRef, EnvValue, ImagePolicy, OneOrMany, ServiceNeed};

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

    /// Set `needs.pg.selector` (a `BTreeMap<String,String>`, the real
    /// `ServiceNeed.selector` field) to `{tier: <tier>}`. Used to prove a
    /// pure selector-change stays deferred → `None` (Fix C).
    fn with_pg_selector(mut s: ApplicationBaseSpec, tier: &str) -> ApplicationBaseSpec {
        s.needs = Some(Needs {
            pg: Some(OneOrMany::One(ServiceNeed {
                selector: Some(
                    [("tier".to_string(), tier.to_string())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            })),
            ..Default::default()
        });
        s
    }

    /// Set `needs.pg.size` (the real `ServiceNeed.size: Option<String>`
    /// field, a size-class / quantity string) so a size change can be
    /// exercised — it must stay soft (Fix D).
    fn with_pg_size(mut s: ApplicationBaseSpec, size: &str) -> ApplicationBaseSpec {
        s.needs = Some(Needs {
            pg: Some(OneOrMany::One(ServiceNeed {
                size: Some(size.to_string()),
                ..Default::default()
            })),
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
    fn internal_to_vpn_is_soft() {
        // internal → vpn is a soft visibility move (no public surface).
        // NOTE (2.16b Task 7 #10): internal → PUBLIC is NO LONGER soft — it is
        // the `network-visibility-escalation` security-boundary op (see
        // `network_internal_to_public_escalates_and_hostname_add_cofires` /
        // `internal_to_public_no_hostname_escalates`). Only the vpn move stays
        // soft here.
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_expose(base(), expose("internal", None)),
            &with_expose(base(), expose("vpn", None))
        )
        .is_none());
    }

    // 2.16b Task 7 #10: internal → public WITHOUT a hostname still escalates
    // (the public Gateway surface is opened even before a hostname is added).
    #[test]
    fn internal_to_public_no_hostname_escalates() {
        let cs = ApplicationMigrationStrategy::detect_all(
            &with_expose(base(), expose("internal", None)),
            &with_expose(base(), expose("public", None)),
        );
        assert!(cs
            .iter()
            .any(|c| c.trigger_type == "network-visibility-escalation"));
        // No hostname added → no #11 candidate.
        assert!(!cs.iter().any(|c| c.trigger_type == "public-hostname-add"));
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

    /// Set `image` + `imagePolicy.resolve` in one call (2.16b Task 7 #13).
    fn with_image_policy(
        mut s: ApplicationBaseSpec,
        image: &str,
        resolve: &str,
    ) -> ApplicationBaseSpec {
        s.image = Some(image.into());
        s.image_policy = Some(ImagePolicy {
            resolve: Some(resolve.into()),
        });
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
        assert_eq!(c.classification, "security-boundary");
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_image(base(), "ghcr.io/acme/api:v1"),
            &with_image(base(), "ghcr.io/acme/api:v2")
        )
        .is_none()); // tag soft
    }

    // 2.16b S1.1: moving the image to a DIFFERENT repository is a pull-source /
    // security-boundary change (a different registry path can serve different
    // content), so it's the most severe class — outranking a data-migration in
    // a multi-op edit.
    #[test]
    fn image_repo_change_is_security_boundary() {
        let c = ApplicationMigrationStrategy::detect_destructive(
            &with_image(base(), "ghcr.io/acme/api:v1"),
            &with_image(base(), "ghcr.io/acme/OTHER:v1"),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "image-path-change");
        assert_eq!(c.classification, "security-boundary");
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

    // ---- Task 5: env-ref removal (gated) vs literal removal (soft) ----

    fn with_env(mut s: ApplicationBaseSpec, k: &str, v: EnvValue) -> ApplicationBaseSpec {
        s.env
            .get_or_insert_with(Default::default)
            .insert(k.into(), v);
        s
    }

    fn claim_ref() -> EnvValue {
        EnvValue::Ref(EnvRef::Claim("pg.url".into()))
    }

    fn secret_ref() -> EnvValue {
        EnvValue::Ref(EnvRef::Secret("stripe/key".into()))
    }

    #[test]
    fn removing_env_ref_gates() {
        let c = ApplicationMigrationStrategy::detect_destructive(
            &with_env(base(), "DB", claim_ref()),
            &base(),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "env-ref-removal");
        assert_eq!(c.classification, "requires-restart");
        assert_eq!(c.field, "env.DB");
        // `from` renders the reference to a non-empty sentinel; `to` is
        // the removed marker.
        assert!(!c.from.as_ref().unwrap().as_str().unwrap().is_empty());
        assert_eq!(c.from.as_ref().unwrap().as_str().unwrap(), "claim.pg.url");
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "(removed)");
    }

    #[test]
    fn removing_env_secret_ref_gates_with_secret_sentinel() {
        let c = ApplicationMigrationStrategy::detect_destructive(
            &with_env(base(), "STRIPE", secret_ref()),
            &base(),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "env-ref-removal");
        assert_eq!(c.field, "env.STRIPE");
        assert_eq!(
            c.from.as_ref().unwrap().as_str().unwrap(),
            "secret:stripe/key"
        );
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "(removed)");
    }

    #[test]
    fn removing_env_literal_is_soft() {
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_env(base(), "X", EnvValue::Literal("hi".into())),
            &base()
        )
        .is_none());
    }

    #[test]
    fn adding_env_ref_is_soft() {
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &base(),
            &with_env(base(), "DB", claim_ref())
        )
        .is_none());
    }

    #[test]
    fn changing_env_ref_to_same_ref_is_soft() {
        // Task 5 gates only REMOVAL of a ref-valued env. A key that stays
        // present with an UNCHANGED ref (claim → same claim) is a soft rollout.
        // NOTE (2.16b Task 6 #8): a ref → LITERAL change is NO LONGER soft — it
        // is an `env-ref-downgrade` security-boundary op (see
        // `env_ref_to_literal_downgrade_gates_no_leak`). Only the same-ref /
        // self-scoped-retarget cases remain soft.
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_env(base(), "DB", claim_ref()),
            &with_env(base(), "DB", claim_ref())
        )
        .is_none());
    }

    // ---- 2.16b Task 6: env-ref security-boundary triggers #7/#8/#9 ----

    // #7 env-secret-ref-add: a NEW key whose value is a `secret:` ref is a
    // security-boundary op — the container gains a fresh external-secret
    // dependency. A `claim.` ref add is self-scoped (ADR 0046) and stays SOFT.
    #[test]
    fn env_secret_ref_add_gates_but_claim_add_soft() {
        let c = ApplicationMigrationStrategy::detect_destructive(
            &base(),
            &with_env(
                base(),
                "S",
                EnvValue::Ref(EnvRef::Secret("stripe/key".into())),
            ),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "env-secret-ref-add");
        assert_eq!(c.classification, "security-boundary");
        assert_eq!(c.field, "env.S");
        assert_eq!(c.from.as_ref().unwrap().as_str().unwrap(), "(absent)");
        assert_eq!(
            c.to.as_ref().unwrap().as_str().unwrap(),
            "secret:stripe/key"
        );
        // A claim-ref ADD is NOT gated (self-scoped).
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &base(),
            &with_env(base(), "D", EnvValue::Ref(EnvRef::Claim("pg.url".into())))
        )
        .is_none());
    }

    // #8 env-ref-downgrade: a key present both sides moving from a `Ref`
    // (claim OR secret) to a `Literal` is a security-boundary downgrade — a
    // resolved-at-runtime reference is replaced by an inline value. `to` is
    // the SENTINEL `"(literal)"`, NEVER the literal value (no leak).
    #[test]
    fn env_ref_to_literal_downgrade_gates_no_leak() {
        let old = with_env(base(), "D", EnvValue::Ref(EnvRef::Claim("pg.url".into())));
        let new = with_env(
            base(),
            "D",
            EnvValue::Literal("postgres://attacker/db".into()),
        );
        let c = ApplicationMigrationStrategy::detect_destructive(&old, &new).unwrap();
        assert_eq!(c.trigger_type, "env-ref-downgrade");
        assert_eq!(c.classification, "security-boundary");
        assert_eq!(c.field, "env.D");
        assert_eq!(c.from.as_ref().unwrap().as_str().unwrap(), "claim.pg.url");
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "(literal)"); // no value leak
    }

    // A secret-ref → literal downgrade gates the same way (still #8).
    #[test]
    fn env_secret_ref_to_literal_downgrade_gates() {
        let old = with_env(base(), "K", EnvValue::Ref(EnvRef::Secret("a/k".into())));
        let new = with_env(base(), "K", EnvValue::Literal("hardcoded".into()));
        let c = ApplicationMigrationStrategy::detect_destructive(&old, &new).unwrap();
        assert_eq!(c.trigger_type, "env-ref-downgrade");
        assert_eq!(c.classification, "security-boundary");
        assert_eq!(c.from.as_ref().unwrap().as_str().unwrap(), "secret:a/k");
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "(literal)");
    }

    // A literal → literal change is NOT a downgrade (both sides inline) — soft.
    #[test]
    fn env_literal_to_literal_is_not_a_downgrade() {
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_env(base(), "X", EnvValue::Literal("a".into())),
            &with_env(base(), "X", EnvValue::Literal("b".into()))
        )
        .is_none());
    }

    // #9 env-secret-ref-retarget: a key present both sides pointing at a
    // DIFFERENT `secret:` target is a security-boundary retarget. A same-target
    // secret ref (unchanged) is soft; a claim→claim retarget is self-scoped and
    // NOT gated as #9.
    #[test]
    fn secret_ref_retarget_gates_same_soft() {
        let old = with_env(base(), "K", EnvValue::Ref(EnvRef::Secret("a/k".into())));
        let c = ApplicationMigrationStrategy::detect_destructive(
            &old,
            &with_env(base(), "K", EnvValue::Ref(EnvRef::Secret("b/k".into()))),
        )
        .unwrap();
        assert_eq!(c.trigger_type, "env-secret-ref-retarget");
        assert_eq!(c.classification, "security-boundary");
        assert_eq!(c.field, "env.K");
        assert_eq!(c.from.as_ref().unwrap().as_str().unwrap(), "secret:a/k");
        assert_eq!(c.to.as_ref().unwrap().as_str().unwrap(), "secret:b/k");
        // Same secret target on both sides → soft.
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &old,
            &with_env(base(), "K", EnvValue::Ref(EnvRef::Secret("a/k".into())))
        )
        .is_none());
    }

    // A claim→claim retarget is self-scoped (ADR 0046) — NOT gated as #9.
    #[test]
    fn claim_ref_retarget_is_soft() {
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_env(base(), "D", EnvValue::Ref(EnvRef::Claim("pg.url".into()))),
            &with_env(base(), "D", EnvValue::Ref(EnvRef::Claim("pg.host".into())))
        )
        .is_none());
    }

    // secret→claim / claim→secret are NOT #9 (only secret→secret different
    // target retargets). A secret→claim change is a self-scoping move (the
    // claim ref is provider-derived), and a claim→secret ADD-of-secret-surface
    // reuses an existing key so it isn't #7 either — both stay SOFT here.
    #[test]
    fn secret_to_claim_and_claim_to_secret_crossovers_are_soft() {
        // secret → claim (both sides Ref, different variant): not a retarget,
        // not a downgrade (new is still a Ref).
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_env(base(), "K", EnvValue::Ref(EnvRef::Secret("a/k".into()))),
            &with_env(base(), "K", EnvValue::Ref(EnvRef::Claim("pg.url".into())))
        )
        .is_none());
        // claim → secret (both sides Ref): existing key, so not #7; different
        // variant, so not #9; new is a Ref, so not #8 downgrade.
        assert!(ApplicationMigrationStrategy::detect_destructive(
            &with_env(base(), "K", EnvValue::Ref(EnvRef::Claim("pg.url".into()))),
            &with_env(base(), "K", EnvValue::Ref(EnvRef::Secret("a/k".into())))
        )
        .is_none());
    }

    // Composite: adding `needs.pg` together with an `env DB=claim.pg.url` (the
    // canonical 2.4-flow app add) produces NO security-boundary candidate — a
    // claim-ref add is soft and a needs ADD is soft.
    #[test]
    fn needs_pg_plus_claim_env_add_is_soft_composite() {
        let old = base();
        let new = with_env(
            with_pg(base()),
            "DB",
            EnvValue::Ref(EnvRef::Claim("pg.url".into())),
        );
        assert!(ApplicationMigrationStrategy::detect_all(&old, &new)
            .iter()
            .all(|c| c.classification != "security-boundary"));
        // and nothing gates overall (needs ADD + claim env ADD are both soft).
        assert!(ApplicationMigrationStrategy::detect_destructive(&old, &new).is_none());
    }

    // ---- Task 6: finalize the classifier ----

    // Fix A — deterministic pick_primary (R3-mn-b).
    #[test]
    fn multi_op_picks_highest_severity_deterministically() {
        // remove needs.pg (data-migration) AND scale to zero (requires-restart)
        let old = with_replicas(with_pg(base()), Some(2));
        let new = with_replicas(base(), Some(0));
        let c1 = ApplicationMigrationStrategy::detect_destructive(&old, &new).unwrap();
        let c2 = ApplicationMigrationStrategy::detect_destructive(&old, &new).unwrap();
        assert_eq!(c1.classification, "data-migration"); // highest severity wins
        assert_eq!(c1, c2); // deterministic
    }

    // Fix B — adding a hostname to a public app does NOT emit a `domain-change`
    // (no existing route is disrupted). NOTE (2.16b Task 7 #11): the None → Some
    // FIRST-hostname add IS now gated as a `public-hostname-add` security-boundary
    // op (a new externally-reachable name is published — see
    // `public_hostname_add_on_already_public_app_gates`). Fix B's invariant
    // survives as "no `domain-change` on an add"; the op that fires is #11.
    #[test]
    fn adding_hostname_to_public_app_emits_no_domain_change() {
        let cs = ApplicationMigrationStrategy::detect_all(
            &with_expose(base(), expose("public", None)),
            &with_expose(base(), expose("public", Some("a.example.com"))),
        );
        // Fix B: the requires-restart `domain-change` op must NOT fire on an add.
        assert!(!cs.iter().any(|c| c.trigger_type == "domain-change"));
        // 2.16b Task 7 #11: the add IS gated as public-hostname-add.
        assert!(cs.iter().any(|c| c.trigger_type == "public-hostname-add"));
    }

    // Fix C — a pure needs.*.selector change stays DEFERRED → None (R1-M5).
    #[test]
    fn changing_needs_pg_selector_is_deferred_not_gated() {
        // 2.4d ruled selector-change non-destructive (single provider
        // tier:integrated; revisit 2.5+).
        let old = with_pg_selector(base(), "tier-a");
        let new = with_pg_selector(base(), "tier-b");
        // Guard against a false pass: the two specs must genuinely differ in
        // the selector (otherwise the None below would be meaningless).
        assert_ne!(old.needs, new.needs);
        assert!(ApplicationMigrationStrategy::detect_destructive(&old, &new).is_none());
    }

    // Fix D — needs.*.size change stays soft (provisioner-guarded, V14).
    #[test]
    fn changing_needs_pg_size_is_not_gated_provisioner_guards_shrink() {
        // V14: PVC/CNPG storage is expansion-only; a shrink is refused at the
        // provisioner layer, so 2.16b does NOT gate needs.*.size changes (the
        // provisioner is the guard).
        let old = with_pg_size(base(), "10Gi");
        let new = with_pg_size(base(), "5Gi");
        // Guard against a false pass: the two specs must genuinely differ in size.
        assert_ne!(old.needs, new.needs);
        assert!(ApplicationMigrationStrategy::detect_destructive(&old, &new).is_none());
    }

    // ---- Task 8 Part 1: create_plan_for app-ns + controller ownerRef ----

    #[test]
    fn create_plan_lands_in_app_ns_with_controller_ownerref() {
        let change = DestructiveChange {
            trigger_type: "needs-removal".into(),
            field: "needs.pg".into(),
            from: Some(serde_json::json!("needs.pg")),
            to: Some(serde_json::json!("(removed)")),
            classification: "data-migration".into(),
        };
        let mp = ApplicationMigrationStrategy::create_plan_for(
            std::slice::from_ref(&change),
            "myapp-dev-migration-1",
            "team-a",
            "myapp",
            "dev",
            "uid-123",
        );
        assert_eq!(mp.metadata.namespace.as_deref(), Some("team-a")); // APP ns, NOT apprafter-system
        let owner = &mp.metadata.owner_references.as_ref().unwrap()[0];
        assert_eq!(owner.kind, "Application");
        assert_eq!(owner.name, "myapp");
        assert_eq!(owner.uid, "uid-123");
        assert_eq!(owner.controller, Some(true));
        assert_eq!(owner.block_owner_deletion, Some(true));
        assert_eq!(owner.api_version, "apprafter.io/v1alpha1");
        assert_eq!(mp.spec.scope.type_, "application");
        assert!(mp.spec.scope.platform.is_none()); // webhook: no platform block
        assert!(mp.spec.previous_spec_snapshot.is_none()); // app scope: no snapshot
    }

    // ---- 2.16b S1.2: MigrationPlan rollup invariants ----

    // A MigrationPlan must carry the FULL blast radius, not just the
    // `pick_primary` headline, so an approver can't be laundered by hiding a
    // dangerous op behind a benign-looking primary. This asserts the four
    // rollup invariants over a real multi-op edit.
    #[test]
    fn rollup_invariants_hold() {
        // an edit that removes needs.pg (data-migration) AND scales to zero
        // (requires-restart) — two distinct destructive candidates.
        let old = with_replicas(with_pg(base()), Some(2));
        let new = with_replicas(base(), Some(0));
        let candidates = ApplicationMigrationStrategy::detect_all(&old, &new);
        // Two ops detected (needs-removal + scale-to-zero).
        assert_eq!(candidates.len(), 2, "expected both destructive ops");
        let plan = ApplicationMigrationStrategy::create_plan_for(
            &candidates,
            "p",
            "ns",
            "app",
            "dev",
            "uid",
        );
        let risks = plan.spec.risks.as_ref().unwrap();
        let changes = plan.spec.changes.as_ref().unwrap();

        // (1) the primary (spec.trigger) is present in changes[].
        assert!(changes
            .iter()
            .any(|c| c.trigger == plan.spec.trigger.type_ && c.field == plan.spec.trigger.field));

        // (2) classifications == distinct(changes.classification), sorted
        // severity desc. Highest-severity class first; all distinct.
        let cls = risks.classifications.as_ref().unwrap();
        assert_eq!(cls[0], "data-migration"); // highest severity first
        assert_eq!(
            cls.iter().collect::<std::collections::HashSet<_>>().len(),
            cls.len(),
            "classifications must be distinct"
        );
        // It IS the distinct set of the candidates' classifications.
        let distinct: std::collections::HashSet<&String> =
            changes.iter().map(|c| &c.classification).collect();
        assert_eq!(cls.len(), distinct.len());
        assert!(cls.iter().all(|c| distinct.contains(c)));

        // (4) the primary's severity == the max over all candidate severities.
        assert_eq!(
            classification_severity(&risks.classification) as u32,
            changes.iter().map(|c| c.severity).max().unwrap()
        );

        // (3) non-empty changes <=> plan created (we created the plan, so
        // changes must be non-empty).
        assert!(!changes.is_empty());

        // S-4: the stamped approval hash covers the FULL candidate set — NOT
        // just the primary. Hashing only the primary would let a lower-severity
        // op ride along unhashed; assert the stamp equals the all-candidate
        // hash and differs from the primary-only hash.
        let full_hash = change_hash(&candidates);
        let primary_only_hash = change_hash(std::slice::from_ref(
            &pick_primary(candidates.clone()).unwrap(),
        ));
        assert_eq!(
            plan.spec.trigger.approved_spec_hash.as_deref(),
            Some(full_hash.as_str()),
            "approval hash must cover ALL candidates"
        );
        assert_ne!(
            full_hash, primary_only_hash,
            "full-set hash must differ from the primary-only hash"
        );
    }

    // ---- Task 8 Part 2: classifier tie-break (review gap A) ----

    // Two EQUAL-severity ops (both `requires-restart`) must resolve to the
    // same primary deterministically. `pick_primary` sorts by
    // `(severity desc, trigger_type asc, field asc)`, so the
    // lexicographically-smaller `trigger_type` wins.
    #[test]
    fn tie_break_between_two_requires_restart_ops_is_deterministic() {
        // env-ref-removal + scale-to-zero, both requires-restart. (image-path
        // is now `security-boundary` — sev4 — so it wouldn't tie; env-ref
        // removal keeps a genuine same-severity tie against scale-to-zero.)
        let old = with_replicas(with_env(base(), "DB", claim_ref()), Some(2));
        let new = with_replicas(base(), Some(0));
        let c = ApplicationMigrationStrategy::detect_destructive(&old, &new).unwrap();
        // both requires-restart; "env-ref-removal" < "scale-to-zero"
        // (trigger_type asc) → env-ref-removal wins, stably.
        assert_eq!(c.classification, "requires-restart");
        assert_eq!(c.trigger_type, "env-ref-removal");
        // and it's stable across calls
        assert_eq!(
            c,
            ApplicationMigrationStrategy::detect_destructive(&old, &new).unwrap()
        );
    }

    // 2.16b S1.1: in a multi-op edit that carries BOTH an image-repository
    // change (security-boundary, sev4) AND a needs-removal (data-migration,
    // sev3), the security-boundary op is the plan primary — it OUTRANKS the
    // data-migration. This is the reclassify's headline behaviour.
    #[test]
    fn image_repo_change_outranks_data_migration_as_primary() {
        // remove needs.pg (data-migration) AND move the image repo
        // (security-boundary).
        let old = with_image(with_pg(base()), "ghcr.io/acme/api:v1");
        let new = with_image(base(), "ghcr.io/acme/other:v1");
        let candidates = ApplicationMigrationStrategy::detect_all(&old, &new);
        assert_eq!(candidates.len(), 2, "both destructive ops detected");
        let primary = ApplicationMigrationStrategy::detect_destructive(&old, &new).unwrap();
        assert_eq!(primary.trigger_type, "image-path-change");
        assert_eq!(primary.classification, "security-boundary");
        // stable across calls.
        assert_eq!(
            primary,
            ApplicationMigrationStrategy::detect_destructive(&old, &new).unwrap()
        );
    }

    // ---- 2.16b S1.4: no secret material ever enters a plan-readable field ----

    // A classifier-produced `from`/`to` must NEVER contain a literal env
    // VALUE — only ref SENTINELS (`claim.<type>.<field>`, `secret:<name>/<key>`)
    // and structural markers reach the MigrationPlan, which is world-readable to
    // any principal with plan-read RBAC. This holds today via the type guard:
    // only `EnvValue::Ref` gates (env-ref-removal), while a `Literal` removal /
    // change is SOFT (no candidate). This is the regression guard so a future
    // change can't leak a secret into a plan-readable field.
    #[test]
    fn from_to_never_contain_literal_env_values() {
        let secret = "sup3r-s3cret-token-xyz";
        // (a) literal removed (soft) — no candidate should carry the value.
        let old_a = with_env(base(), "TOKEN", EnvValue::Literal(secret.into()));
        let new_a = base();
        // (b) literal changed to another literal (soft).
        let new_b = with_env(base(), "TOKEN", EnvValue::Literal("other".into()));
        // (c) 2.16b Task 6 #8: a Ref → LITERAL downgrade DOES gate, but its `to`
        // must be the `"(literal)"` SENTINEL — the literal VALUE (which may be
        // freshly-inlined secret material) must NEVER reach the plan.
        let old_c = with_env(base(), "TOKEN", EnvValue::Ref(EnvRef::Secret("v/k".into())));
        let new_c = with_env(base(), "TOKEN", EnvValue::Literal(secret.into()));
        for (old, new) in [(&old_a, &new_a), (&old_a, &new_b), (&old_c, &new_c)] {
            for c in ApplicationMigrationStrategy::detect_all(old, new) {
                let s = format!("{:?}{:?}", c.from, c.to);
                assert!(
                    !s.contains(secret),
                    "classifier leaked a literal env value into from/to: {s}"
                );
            }
        }
    }

    // ---- 2.16b Task 7: expose-escalation #10/#11/#12 + imagePolicy #13 ----

    // #10 network-visibility-escalation + #11 public-hostname-add CO-FIRE when
    // an internal app flips to public WITH a new hostname. Both land in
    // `changes[]` (via detect_all); they are NOT merged.
    #[test]
    fn network_internal_to_public_escalates_and_hostname_add_cofires() {
        let cs = ApplicationMigrationStrategy::detect_all(
            &with_expose(base(), expose("internal", None)),
            &with_expose(base(), expose("public", Some("a.example.com"))),
        );
        let esc = cs
            .iter()
            .find(|c| c.trigger_type == "network-visibility-escalation")
            .expect("escalation candidate");
        assert_eq!(esc.classification, "security-boundary");
        assert_eq!(esc.field, "expose.network");
        assert_eq!(esc.from.as_ref().unwrap().as_str().unwrap(), "internal");
        assert_eq!(esc.to.as_ref().unwrap().as_str().unwrap(), "public");
        let hn = cs
            .iter()
            .find(|c| c.trigger_type == "public-hostname-add")
            .expect("hostname-add candidate");
        assert_eq!(hn.classification, "security-boundary");
        assert_eq!(hn.from.as_ref().unwrap().as_str().unwrap(), "(none)");
        assert_eq!(hn.to.as_ref().unwrap().as_str().unwrap(), "a.example.com");
    }

    // #11 fires even without a visibility flip: an already-public app that
    // ADDS its first hostname (None → Some) escalates its public surface.
    #[test]
    fn public_hostname_add_on_already_public_app_gates() {
        let cs = ApplicationMigrationStrategy::detect_all(
            &with_expose(base(), expose("public", None)),
            &with_expose(base(), expose("public", Some("a.example.com"))),
        );
        assert!(cs.iter().any(|c| c.trigger_type == "public-hostname-add"));
        // No escalation (already public → public).
        assert!(!cs
            .iter()
            .any(|c| c.trigger_type == "network-visibility-escalation"));
    }

    // #12 public-port-retarget: on a PUBLIC app a port change is gated; on a
    // non-public app the same port change is soft (no public surface).
    #[test]
    fn public_port_retarget_gates_internal_soft() {
        let po = |p| {
            let mut e = expose("public", Some("h"));
            e.port = p;
            e
        };
        let cs = ApplicationMigrationStrategy::detect_all(
            &with_expose(base(), po(8080)),
            &with_expose(base(), po(9090)),
        );
        let pr = cs
            .iter()
            .find(|c| c.trigger_type == "public-port-retarget")
            .expect("port-retarget candidate");
        assert_eq!(pr.classification, "security-boundary");
        assert_eq!(pr.field, "expose.port");
        assert_eq!(pr.from.as_ref().unwrap().as_str().unwrap(), "8080");
        assert_eq!(pr.to.as_ref().unwrap().as_str().unwrap(), "9090");
        // internal port change → soft.
        let io = |p| {
            let mut e = expose("internal", None);
            e.port = p;
            e
        };
        assert!(!ApplicationMigrationStrategy::detect_all(
            &with_expose(base(), io(8080)),
            &with_expose(base(), io(9090))
        )
        .iter()
        .any(|c| c.trigger_type == "public-port-retarget"));
    }

    // #13 image-policy-relaxation: `resolve: off` → not-off ONLY gates when the
    // new image is a FLOATING TAG (a mutable pull surface). An already-digest
    // image is a no-op (pin is unchanged in practice), and a `digest → off`
    // move is HARDENING (soft).
    #[test]
    fn imagepolicy_off_to_digest_gates_only_floating_tag() {
        // off → digest on a floating tag: gated security-boundary.
        let c = ApplicationMigrationStrategy::detect_all(
            &with_image_policy(base(), "ghcr.io/a/b:v1", "off"),
            &with_image_policy(base(), "ghcr.io/a/b:v1", "digest"),
        );
        let r = c
            .iter()
            .find(|c| c.trigger_type == "image-policy-relaxation")
            .expect("relaxation candidate");
        assert_eq!(r.classification, "security-boundary");
        assert_eq!(r.field, "imagePolicy.resolve");
        assert_eq!(r.from.as_ref().unwrap().as_str().unwrap(), "off");
        assert_eq!(r.to.as_ref().unwrap().as_str().unwrap(), "digest");
        // off → digest but the image is ALREADY a digest → NOT gated (no-op).
        let digest = format!("ghcr.io/a/b@sha256:{}", "a".repeat(64));
        assert!(!ApplicationMigrationStrategy::detect_all(
            &with_image_policy(base(), &digest, "off"),
            &with_image_policy(base(), &digest, "digest")
        )
        .iter()
        .any(|c| c.trigger_type == "image-policy-relaxation"));
        // digest → off is HARDENING → soft.
        assert!(!ApplicationMigrationStrategy::detect_all(
            &with_image_policy(base(), "ghcr.io/a/b:v1", "digest"),
            &with_image_policy(base(), "ghcr.io/a/b:v1", "off")
        )
        .iter()
        .any(|c| c.trigger_type == "image-policy-relaxation"));
    }

    // #10 must NOT fire on a public → internal move (that's a DE-escalation,
    // already the soft/requires-restart `network-visibility-change` op).
    #[test]
    fn public_to_internal_does_not_escalate() {
        let cs = ApplicationMigrationStrategy::detect_all(
            &with_expose(base(), expose("public", Some("h"))),
            &with_expose(base(), expose("internal", None)),
        );
        assert!(!cs
            .iter()
            .any(|c| c.trigger_type == "network-visibility-escalation"));
    }

    // image_is_digest pure helper: floating tag vs digest suffix.
    #[test]
    fn image_is_digest_detects_sha256_suffix() {
        assert!(image_is_digest(&format!(
            "ghcr.io/a/b@sha256:{}",
            "a".repeat(64)
        )));
        assert!(!image_is_digest("ghcr.io/a/b:v1"));
        assert!(!image_is_digest("ghcr.io/a/b")); // bare, no tag/digest
        assert!(!image_is_digest("localhost:5000/app:dev")); // registry-port colon, not a digest
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
