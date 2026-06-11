// SPDX-License-Identifier: FSL-1.1-Apache-2.0
//! Main reconcile loop for `PlatformStack/default`.
//!
//! The controller watches the singleton `PlatformStack` CR in
//! `apprafter-system`, computes the desired
//! `Application.spec.source.{targetRevision, helm.valuesObject}`,
//! and SSA-patches the parent `platform` Argo CD Application in
//! the `argocd` namespace with field manager
//! `platform-controller`. Argo CD propagates the change to
//! children via its own reconcile cycle.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use k8s_openapi::api::core::v1::{ConfigMap, ObjectReference};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Api, ApiResource, DynamicObject, Patch, PatchParams};
use kube::core::GroupVersionKind;
use kube::runtime::controller::{Action, Controller};
use kube::runtime::events::{Event as KubeEvent, EventType, Recorder, Reporter};
use kube::runtime::reflector::ObjectRef;
use kube::runtime::watcher;
use kube::{Client, Resource, ResourceExt};
use semver::Version;
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{debug, info, warn};

use kube::api::PostParams;
use operator_core::{
    Metrics, MigrationPlan, MigrationPlanScope, MigrationPlanSpec, MigrationPlatformScope,
    MigrationRisks, MigrationTrigger, PlatformStack, PlatformStackCondition, PlatformStackStatus,
    PlatformStackVersionHistoryEntry,
};

use crate::compatibility::{
    fetch_compatibility_doc, fetch_compatibility_doc_with_self_version,
    fetch_path_max_change_class, ChangeClass, CompatError, CompatibilityDoc,
};
use crate::desired::{build as build_desired, DesiredSource};
use crate::oci::{channel_matches, tags_in_channel, Channel};
use crate::status::{
    append_version_history, condition, upsert_condition, COND_MIGRATION_PENDING, COND_READY,
    COND_SYNCED, COND_UNAUTHORIZED_SOURCE_MODIFICATION, COND_UPGRADE_AVAILABLE,
    COND_UPSTREAM_REACHABLE, COND_YANKED_VERSION,
};
use crate::{FIELD_MANAGER, SINGLETON_NAME, SINGLETON_NAMESPACE};

const PARENT_APPLICATION_NAME: &str = "platform";
const PARENT_APPLICATION_NAMESPACE: &str = "argocd";

/// Namespace where platform-scope MigrationPlans live. Mirrors
/// `MIGRATION_PLAN_NAMESPACE` from
/// `operator-controllers/application` — duplicated rather than
/// imported to avoid a circular workspace-internal dep between
/// the two controller crates.
const MIGRATION_PLAN_NAMESPACE: &str = "apprafter-system";

/// Name of the chart-emitted anchor `ConfigMap` in
/// `apprafter-system` (ADR 0048). Platform `MigrationPlan`s
/// carry a same-namespace `ownerReference` to it so Argo CD's
/// ownerRef walk pulls the plan into the platform-stack root
/// Application's resource tree. A cross-namespace ownerRef would
/// make k8s GC silently delete the plan, so the anchor MUST live
/// in `MIGRATION_PLAN_NAMESPACE`.
const PLATFORM_MIGRATION_ANCHOR: &str = "platform-migration-anchor";

/// Reporter identity stamped onto every Kubernetes Event this
/// controller publishes. Shows up in `kubectl describe
/// platformstack default` under the event's `Reporter` field
/// (and in `kubectl get events -o wide` under `REPORTING
/// INSTANCE`). Walk-fix #6 v0.1.119 → v0.1.120.
const EVENT_REPORTER_CONTROLLER: &str = "platform-controller";

/// Build a per-reconcile `Recorder` targeting the singleton
/// PlatformStack. Each event lands in the same namespace as
/// the resource (`apprafter-system`); the `reference` carries
/// the typed identity so `kubectl describe platformstack
/// default` surfaces it in the Events section.
///
/// `Recorder::new` is cheap — internally just wires an Api +
/// reference + reporter. Constructing per-event keeps the
/// reconcile function pure and avoids stashing mutable state
/// in `Context`.
fn build_recorder(ctx: &Context, stack: &PlatformStack) -> Recorder {
    let reporter = Reporter {
        controller: EVENT_REPORTER_CONTROLLER.into(),
        instance: std::env::var("POD_NAME").ok(),
    };
    let reference = stack.object_ref(&());
    Recorder::new(ctx.client.clone(), reporter, reference)
}

/// Static ObjectReference for the parent platform Application.
/// Used as the `secondary` (Kubernetes `related`) field on
/// events so operators can correlate `kubectl describe
/// application platform -n argocd` ↔ `kubectl describe
/// platformstack default -n apprafter-system`.
fn parent_object_reference() -> ObjectReference {
    ObjectReference {
        api_version: Some("argoproj.io/v1alpha1".into()),
        kind: Some("Application".into()),
        name: Some(PARENT_APPLICATION_NAME.into()),
        namespace: Some(PARENT_APPLICATION_NAMESPACE.into()),
        ..ObjectReference::default()
    }
}

/// Backoff when the parent Application is mid-sync; the loop
/// re-evaluates after this delay rather than cancelling the
/// in-flight sync.
const IN_FLIGHT_REQUEUE: Duration = Duration::from_secs(30);

/// Default cadence when `spec.source.checkInterval` parsing fails.
const DEFAULT_REQUEUE: Duration = Duration::from_secs(3600);

/// Floor for how often PlatformController actually queries the
/// OCI registry for channel-latest. Walk-found bug v0.1.118 →
/// v0.1.119: every reconcile was unconditionally calling
/// `latest_in_channel` and stamping `status.lastUpstreamCheck =
/// Utc::now()`. The status write bumped the resource version,
/// the watcher fired a fresh event, the next reconcile bumped
/// the version again — controller burned hundreds of reconciles
/// per second in a tight loop.
///
/// 60s is generous enough to absorb watch-event bursts (Argo CD
/// reconcile patches on parent App, our own SSA patches when
/// values genuinely change, user kubectl-edits) without making
/// the cadence feel sluggish. The chart's webhook minimum for
/// `checkInterval` is 1h; this is only the throttle for
/// "the user hasn't asked for a poll yet but a watch event
/// woke us up".
const MIN_OCI_POLL_INTERVAL_SECS: i64 = 60;

#[derive(Debug, Error)]
pub enum Error {
    #[error("kube-rs error: {0}")]
    Kube(#[from] kube::Error),
    #[error("oci poll error: {0}")]
    Oci(#[from] crate::oci::OciError),
    #[error("compatibility fetch error: {0}")]
    Compatibility(#[from] crate::compatibility::CompatError),
    #[error("serde_json error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("unparseable check interval {0:?}")]
    CheckInterval(String),
}

struct Context {
    client: Client,
    #[allow(dead_code)]
    metrics: Arc<Metrics>,
    app_api_resource: ApiResource,
}

pub async fn run(client: Client, metrics: Arc<Metrics>) -> Result<(), Error> {
    let stacks: Api<PlatformStack> = Api::namespaced(client.clone(), SINGLETON_NAMESPACE);
    let app_api_resource = ApiResource::from_gvk(&GroupVersionKind {
        group: "argoproj.io".into(),
        version: "v1alpha1".into(),
        kind: "Application".into(),
    });
    // Dynamic Api for the parent platform Application — used both
    // for the read in reconcile() and for the watch mapping that
    // bridges Application change events to PlatformStack
    // reconciles (so a foreign kubectl-patch on the parent App's
    // spec.source triggers immediate revert instead of waiting
    // for the next checkInterval).
    let apps: Api<DynamicObject> = Api::namespaced_with(
        client.clone(),
        PARENT_APPLICATION_NAMESPACE,
        &app_api_resource,
    );
    let ctx = Arc::new(Context {
        client,
        metrics,
        app_api_resource,
    });

    info!(
        parent_app = format!("{PARENT_APPLICATION_NAMESPACE}/{PARENT_APPLICATION_NAME}").as_str(),
        "PlatformController Controller::run() entering watch loop"
    );
    Controller::new(stacks, watcher::Config::default())
        .watches_with(
            apps,
            ctx.app_api_resource.clone(),
            watcher::Config::default(),
            |app: DynamicObject| {
                // Bridge: any change to the parent platform App
                // triggers a reconcile of the singleton
                // PlatformStack. Reconcile filters non-singleton
                // names internally.
                if app.metadata.namespace.as_deref() == Some(PARENT_APPLICATION_NAMESPACE)
                    && app.metadata.name.as_deref() == Some(PARENT_APPLICATION_NAME)
                {
                    Some(
                        ObjectRef::<PlatformStack>::new(SINGLETON_NAME).within(SINGLETON_NAMESPACE),
                    )
                } else {
                    None
                }
            },
        )
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, action)) => {
                    info!(
                        object = %obj,
                        ?action,
                        "PlatformController reconcile completed"
                    );
                }
                Err(e) => {
                    warn!(error = %e, "PlatformController reconcile error");
                }
            }
        })
        .await;
    Ok(())
}

async fn reconcile(stack: Arc<PlatformStack>, ctx: Arc<Context>) -> Result<Action, Error> {
    // Filter to the singleton coordinates per webhook contract.
    if stack.name_any() != SINGLETON_NAME {
        warn!(name = %stack.name_any(), "ignoring non-singleton PlatformStack");
        return Ok(Action::await_change());
    }
    info!(
        name = %stack.name_any(),
        generation = stack.metadata.generation.unwrap_or(0),
        "PlatformController reconcile fired"
    );

    let spec = &stack.spec;
    let prior_conds = stack
        .status
        .as_ref()
        .and_then(|s| s.conditions.clone())
        .unwrap_or_default();

    // 1. Channel-latest from upstream, throttled to MIN_OCI_POLL_INTERVAL_SECS.
    //
    // The query feeds two consumers:
    //   (a) `status.availableVersion` (regardless of pin);
    //   (b) the `UpgradeAvailable` semver comparison.
    //
    // Walk-found bug v0.1.116 → v0.1.117 fixed (a) — old code
    // used values_differ instead of semver. Walk-found bug
    // v0.1.118 → v0.1.119 fixes the cadence: an unconditional
    // OCI poll + `lastUpstreamCheck = Utc::now()` on every
    // reconcile bumped the resource version, fired a watch
    // event, and looped the controller hundreds of times per
    // second. Throttle to 60s (MIN_OCI_POLL_INTERVAL_SECS);
    // intermediate reconciles re-use the cached
    // `status.availableVersion` and skip writing
    // lastUpstreamCheck.
    let channel = Channel::parse(&spec.channel).unwrap_or(Channel::Stable);
    let now = Utc::now();
    let prior_last_check = stack
        .status
        .as_ref()
        .and_then(|s| s.last_upstream_check.as_deref())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc));
    let prior_available = stack
        .status
        .as_ref()
        .and_then(|s| s.available_version.clone());
    let should_poll_oci = match (prior_last_check, &prior_available) {
        (Some(t), Some(_)) => (now - t).num_seconds() >= MIN_OCI_POLL_INTERVAL_SECS,
        _ => true,
    };
    // Resolve the channel-latest + pull compatibility.yaml in a
    // single throttled OCI poll cycle. The compat doc is needed
    // twice:
    //
    //   1. channel-latest resolution: the latest non-yanked,
    //      channel-matching version (Track B.1.74a — yanking
    //      support; ADR 0041 — read it from the channel tag).
    //   2. The `YankedVersion` condition below — look up the
    //      eventually-deployed target's yank status by version
    //      key in the same doc.
    //
    // ADR 0041 FAST PATH: the publish workflow moves a moving
    // `<repo>:<channel>` tag onto the channel-latest after each
    // release, and the chart's `compatibility.yaml` is cumulative
    // (lists every published version + yank status). So ONE
    // `fetch_compatibility_doc(&upstream, channel.as_tag())` pull
    // resolves the channel-latest — `latest_non_yanked_in_compat`
    // reads the answer straight out of that doc, no tag listing,
    // no pagination.
    //
    // FALLBACK: a pre-contract chart (or a channel never
    // published) has no `:<channel>` tag, so the manifest pull
    // 404s — surfaced as the typed `CompatError::ManifestNotFound`
    // (classified off the OCI error *code*, not message text). In
    // that case — and when the channel doc parses but declares no
    // usable version — we fall back to the prior paginated path:
    // `tags_in_channel` → top tag → `resolve_non_yanked_latest`.
    // Any OTHER fetch error (network, auth, parse, a blob 404)
    // PROPAGATES rather than silently masking a real failure
    // behind the listing.
    //
    // The fetch also returns the chart's own version (the
    // `org.opencontainers.image.version` annotation); it caps the
    // fast-path resolution so a compat-doc key ABOVE the published
    // channel-latest chart (a not-yet-published / phantom entry)
    // can never be reported as `availableVersion`.
    //
    // Bounded by the 60s throttle (`MIN_OCI_POLL_INTERVAL_SECS`).
    //
    // DETECTION vs ENFORCEMENT. Resolving the channel-latest is
    // DETECTION (drives `availableVersion` + `UpgradeAvailable`).
    // It is ORTHOGONAL to ENFORCEMENT (deploying the policy
    // target). A resolver failure must NOT abort the reconcile:
    // doing so wedged the v0.2.12 operator — its OCI poll crashed
    // on a `tags: null` quirk every cycle, BEFORE the pin was ever
    // applied, so a pinned stack never converged and (because the
    // crashing reconcile never wrote status) every condition froze
    // at its last-good value. Instead we DEGRADE: a pinned stack
    // still enforces its pin, an unpinned stack keeps its
    // last-known target, and `UpstreamReachable=False` makes the
    // degraded detection visible. Only an unpinned stack that has
    // NEVER resolved a version has no target at all — that alone
    // propagates.
    let mut upstream_poll_error: Option<String> = None;
    let (channel_latest_str, did_poll_oci, compat_doc) = if should_poll_oci {
        match resolve_channel_latest(&spec.source.upstream, channel, &spec.channel).await {
            Ok(resolved) => resolved,
            Err(e) => {
                let msg = e.to_string();
                warn!(
                    error = %msg,
                    pinned = spec.pin.is_some(),
                    "channel-latest resolution failed; degrading (pin / last-known target preserved)"
                );
                upstream_poll_error = Some(msg);
                match degrade_on_resolution_failure(spec.pin.is_some(), prior_available.as_deref())
                {
                    Some(tuple) => tuple,
                    // Unpinned AND never resolved a version — no
                    // target to deploy. Genuinely fatal; propagate.
                    None => return Err(e),
                }
            }
        }
    } else {
        // SAFETY: when `should_poll_oci` is false we've already
        // confirmed `prior_available` is Some(_) above.
        (prior_available.clone().unwrap(), false, None)
    };

    // 2. Policy target — what PlatformController wants the
    //    parent's `spec.source.targetRevision` to be. Pin wins
    //    over channel-latest.
    let policy_target = match &spec.pin {
        Some(p) => p.clone(),
        None => channel_latest_str.clone(),
    };

    // 3. Read parent Application state via dynamic API.
    let apps: Api<DynamicObject> = Api::namespaced_with(
        ctx.client.clone(),
        PARENT_APPLICATION_NAMESPACE,
        &ctx.app_api_resource,
    );
    let parent = apps.get(PARENT_APPLICATION_NAME).await?;
    let parent_json = serde_json::to_value(&parent)?;
    let current_target = parent_json
        .pointer("/spec/source/targetRevision")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let in_flight = is_in_flight(&parent_json);

    // 4. Desired SSA payload.
    let desired = build_desired(spec, &policy_target);

    let target_changed = current_target != desired.target_revision;
    let values_changed = values_differ(&parent_json, &desired.helm_values);

    // 5. Pre-build a status skeleton; conditions filled in below.
    // Only stamp lastUpstreamCheck / availableVersion when we
    // actually polled OCI this cycle — preserving prior values
    // otherwise so byte-equal status diffs don't bump the
    // resource version and trigger another watch event.
    let mut new_status: PlatformStackStatus = stack.status.clone().unwrap_or_default();
    if did_poll_oci {
        new_status.last_upstream_check = Some(now.to_rfc3339());
        new_status.available_version = Some(channel_latest_str.clone());
    }
    // Surface the upstream-poll outcome BEFORE the in-flight
    // early-return below, so an in-flight cycle that also degraded
    // doesn't persist a stale `UpstreamReachable=True`. At this
    // point only the resolution-stage error is known — which is
    // exactly right for the in-flight path (the transition-
    // classification fetch is post-in-flight). The normal path
    // re-runs this after classification (which may add its own
    // error) just before the final write.
    set_upstream_reachable(
        &mut new_status,
        should_poll_oci,
        upstream_poll_error.as_deref(),
        &prior_conds,
    );

    // 6. In-flight gating. We do NOT fight an in-progress sync —
    //    Argo CD's app-controller is mid-apply; another patch
    //    from us would race with that.
    if (target_changed || values_changed) && in_flight {
        info!(
            "parent Application in flight; requeuing reconcile in {:?}",
            IN_FLIGHT_REQUEUE
        );
        new_status.target_version = Some(current_target.clone());
        new_status.current_version = Some(current_target.clone());
        // In-flight branch: we DID NOT append history this cycle,
        // so omit `versionHistory` from the SSA patch to preserve
        // server-side state.
        write_status_if_changed(&stack, &ctx, new_status, false).await?;
        return Ok(Action::requeue(IN_FLIGHT_REQUEUE));
    }

    // 7. Decide what target_revision to put into the SSA patch.
    //
    //    `helm.valuesObject` is ALWAYS owned by PlatformController
    //    once it touches the resource — values are runtime config,
    //    not a version bump, and the pin/autoUpgrade policy does
    //    not gate them. `targetRevision` IS gated by policy.
    //
    //    If policy forbids the target change, the SSA patch still
    //    includes `targetRevision = current_target` so
    //    PlatformController takes ownership of the field without
    //    actually changing it. This lets `detect_outside_writer`
    //    catch any subsequent foreign write reliably.
    let pin_set = spec.pin.is_some();
    let allow_target_bump = pin_set || spec.auto_upgrade;

    let mut migration_pending: Option<MigrationPendingState> = None;
    let target_for_patch = if target_changed && allow_target_bump {
        // Track B.1.78: gate destructive transitions behind a
        // MigrationPlan. Deterministic plan name per
        // `(from, to)` pair makes the controller's plan-
        // create call idempotent (repeated reconciles on the
        // same transition return the existing plan, not a
        // duplicate). Per spec.md §3.11, any non-`safe`
        // classification triggers a plan.
        let plan_name = synthesize_platform_plan_name(&current_target, &desired.target_revision);
        let plan_api: Api<MigrationPlan> =
            Api::namespaced(ctx.client.clone(), MIGRATION_PLAN_NAMESPACE);
        let existing_plan = match plan_api.get(&plan_name).await {
            Ok(p) => Some(p),
            Err(kube::Error::Api(api_err)) if api_err.code == 404 => None,
            Err(e) => return Err(Error::from(e)),
        };

        if let Some(plan) = existing_plan {
            let phase = plan
                .status
                .as_ref()
                .and_then(|s| s.phase.as_deref())
                .unwrap_or("pending-approval");
            if phase == "completed" {
                // Approved + executed — proceed with bump.
                info!(
                    plan = %plan_name,
                    "platform MigrationPlan completed — proceeding with bump"
                );
                desired.target_revision.clone()
            } else {
                // Pending / approved / executing / failed /
                // rejected — block bump. Rejected blocks too:
                // the operator explicitly declined, and
                // `PlatformMigrationStrategy.reject` (B.1.76)
                // reverted `spec.pin`; subsequent reconciles
                // that find this rejected plan continue to
                // skip the same transition. Operator clears
                // it by deleting the plan or pinning to a
                // different target.
                info!(
                    plan = %plan_name,
                    %phase,
                    "platform MigrationPlan blocks bump"
                );
                migration_pending = Some(MigrationPendingState {
                    classification: plan_classification(&plan)
                        .unwrap_or_else(|| "unknown".to_string()),
                    plan_name: Some(plan_name.clone()),
                });
                current_target.clone()
            }
        } else {
            // No existing plan — classify the full transition
            // path and either create a plan (destructive) or
            // bump (safe). Walk-fix #8 post-B.1.78: use
            // `fetch_path_max_change_class` instead of single-
            // target `fetch_change_class` so jumps that span
            // intermediate destructive versions (e.g. 0.1.A →
            // 0.1.C where 0.1.B was breaking) don't silently
            // bypass the gate via classification of the C
            // record alone.
            // SECOND degrade point: classifying the transition pulls
            // the compatibility doc from the SAME upstream. If that
            // is unreachable we must NOT abort (aborting crash-loops
            // and freezes status, the very wedge we are fixing) and
            // must NOT bump blind (a destructive change without a
            // MigrationPlan gate). Fail CLOSED: hold at current_target
            // this cycle (the reconcile continues normally — writes
            // status with UpstreamReachable=False, then requeues) and
            // the pin applies once the upstream recovers and the
            // transition can be classified.
            match fetch_path_max_change_class(
                &spec.source.upstream,
                &current_target,
                &desired.target_revision,
            )
            .await
            {
                Err(e) => {
                    warn!(
                        error = %e,
                        from = %current_target,
                        to = %desired.target_revision,
                        "cannot classify transition (upstream unreachable); holding current target"
                    );
                    upstream_poll_error.get_or_insert(e.to_string());
                    current_target.clone()
                }
                Ok(class)
                    if matches!(
                        class,
                        ChangeClass::Breaking
                            | ChangeClass::DataMigration
                            | ChangeClass::RequiresRestart
                    ) =>
                {
                    info!(
                        plan = %plan_name,
                        classification = ?class,
                        from = %current_target,
                        to = %desired.target_revision,
                        "creating platform MigrationPlan for destructive transition"
                    );
                    // ADR 0048: GET the chart-emitted anchor
                    // ConfigMap so the new plan can carry a
                    // same-namespace ownerRef to it (Argo CD then
                    // surfaces the plan on the platform-stack
                    // tree). 404/None-tolerant by design: an
                    // older chart without the anchor yields an
                    // un-owned (off-tree, still CLI-approvable)
                    // plan rather than blocking the create.
                    let anchor_api: Api<ConfigMap> =
                        Api::namespaced(ctx.client.clone(), MIGRATION_PLAN_NAMESPACE);
                    let anchor_uid = anchor_api
                        .get_opt(PLATFORM_MIGRATION_ANCHOR)
                        .await?
                        .and_then(|c| c.metadata.uid);
                    if anchor_uid.is_none() {
                        warn!(
                            anchor = PLATFORM_MIGRATION_ANCHOR,
                            namespace = MIGRATION_PLAN_NAMESPACE,
                            "anchor ConfigMap absent — creating MigrationPlan un-owned (off the Argo tree)"
                        );
                    } else {
                        debug!(
                            anchor = PLATFORM_MIGRATION_ANCHOR,
                            "anchoring MigrationPlan to ConfigMap ownerRef"
                        );
                    }
                    create_platform_migration_plan(
                        &plan_api,
                        &plan_name,
                        &current_target,
                        &desired.target_revision,
                        class,
                        spec.pin.as_deref(),
                        anchor_uid.as_deref(),
                    )
                    .await?;
                    migration_pending = Some(MigrationPendingState {
                        classification: change_class_to_string(class).to_string(),
                        plan_name: Some(plan_name.clone()),
                    });
                    current_target.clone()
                }
                // Safe transition — bump to the desired target.
                Ok(_) => desired.target_revision.clone(),
            }
        }
    } else {
        // Either no change needed OR policy forbids bump
        // (pin unset + autoUpgrade=false). Keep current; the
        // UpgradeAvailable condition will reflect whether a
        // newer version exists upstream.
        current_target.clone()
    };

    // 8. SSA patch parent App. Always run when ANY field changes
    //    (values OR target). On steady state (no diff) we skip the
    //    patch to avoid unnecessary churn through Argo CD.
    let patch_payload = DesiredSource {
        target_revision: target_for_patch.clone(),
        helm_values: desired.helm_values.clone(),
    };

    // ADR 0048: stamp the pending-upgrade surface onto the root
    // Application ONLY while a destructive transition is held
    // behind a not-yet-completed MigrationPlan. `migration_pending`
    // is `Some` exactly in the two gating arms of step 7 (an
    // existing non-`completed` plan, or a freshly-created
    // destructive plan) and stays `None` when the plan reaches
    // `completed` (operator approved + executed) or no destructive
    // transition exists — so the annotations (and the optional
    // approval banner they feed) clear automatically on approval.
    // `from`/`to` are the held current target and the gated
    // desired target — the same pair `synthesize_platform_plan_name`
    // hashed into `plan_name`, keeping the surface self-consistent.
    let pending_upgrade = migration_pending.as_ref().map(|m| PendingUpgrade {
        from: current_target.clone(),
        to: desired.target_revision.clone(),
        class: m.classification.clone(),
        plan: m.plan_name.clone().unwrap_or_else(|| {
            synthesize_platform_plan_name(&current_target, &desired.target_revision)
        }),
    });

    // Detect foreign writer BEFORE patching — so we know whether
    // we need force=true on the SSA patch. Walk-found bug
    // v0.1.117 → v0.1.118: the old order (patch without force,
    // then detect-and-revert) deadlocked when the loader's
    // `kubectl-client-side-apply` already owned
    // `f:spec.f:source.f:targetRevision`. The non-force patch
    // 409'd and reconcile errored before reaching the revert
    // path.
    //
    // Now PlatformController IS the single writer for
    // `spec.source.{targetRevision, helm.valuesObject}` — every
    // SSA patch uses force=true. Foreign-writer detection only
    // surfaces the audit condition; the patch itself is
    // unconditional and always wins.
    let foreign_writer = detect_outside_writer(&parent_json);
    let patched_this_cycle =
        target_changed || values_changed || !platform_controller_owns_source(&parent_json);
    if patched_this_cycle || foreign_writer.is_some() {
        if let Some(foreign) = &foreign_writer {
            warn!(manager = %foreign, "foreign field manager on parent spec.source; force-reverting");
            // Walk-fix #6 v0.1.119 → v0.1.120: emit a
            // Kubernetes Event so the foreign-write +
            // revert pair leaves a durable audit trace
            // visible via `kubectl describe platformstack
            // default` (and `kubectl get events`). Without
            // this, a transient revert vanishes from the
            // `UnauthorizedSourceModification` condition
            // within one reconcile cycle and operators
            // staring at `kubectl get platformstack` see
            // only the post-recovery `False/Clean` state —
            // no record that a foreign write happened.
            //
            // Best-effort: failures to publish the event
            // are logged but don't fail the reconcile. The
            // force-revert SSA patch (below) is the actual
            // load-bearing action.
            let recorder = build_recorder(&ctx, &stack);
            let ev = KubeEvent {
                type_: EventType::Warning,
                reason: "ForeignFieldManager".into(),
                note: Some(format!(
                    "reverted external write to spec.source on parent Application \
                     {PARENT_APPLICATION_NAMESPACE}/{PARENT_APPLICATION_NAME} by field manager \
                     {foreign:?}; PlatformController force-reapplied desired state \
                     (target={target_for_patch})"
                )),
                action: "ForceRevert".into(),
                secondary: Some(parent_object_reference()),
            };
            if let Err(e) = recorder.publish(ev).await {
                warn!(error = %e, "failed to publish ForeignFieldManager event (continuing)");
            }
        }
        patch_application(&apps, &patch_payload, &pending_upgrade).await?;
        if foreign_writer.is_some() {
            // Companion Normal event so the audit trail
            // records the recovery action, not just the
            // violation.
            let recorder = build_recorder(&ctx, &stack);
            let ev = KubeEvent {
                type_: EventType::Normal,
                reason: "SourceReverted".into(),
                note: Some(format!(
                    "parent Application spec.source restored to PlatformController \
                     desired state (target={target_for_patch})"
                )),
                action: "Reconciled".into(),
                secondary: Some(parent_object_reference()),
            };
            if let Err(e) = recorder.publish(ev).await {
                warn!(error = %e, "failed to publish SourceReverted event (continuing)");
            }
        }
    }

    // 10. Conditions. `Synced` reflects whether PlatformController
    //     achieved its desired state on the parent.
    //     `UpgradeAvailable` is the semver comparison of
    //     channel-latest against the deployed target —
    //     independent of values diffs and policy gates.
    let upgrade_available = semver_gt(&channel_latest_str, &target_for_patch);
    let cond_upgrade = if upgrade_available {
        let (reason, message) = match &migration_pending {
            Some(state) if state.plan_name.is_some() => {
                let plan = state.plan_name.as_deref().unwrap_or("");
                (
                    "BlockedByMigrationPlan",
                    format!(
                        "channel {ch} latest is {channel_latest_str}; deployed target is \
                         {target}; blocked by MigrationPlan \
                         {MIGRATION_PLAN_NAMESPACE}/{plan} awaiting approval",
                        ch = spec.channel,
                        target = target_for_patch,
                    ),
                )
            }
            _ => (
                "ManualApprovalRequired",
                format!(
                    "channel {ch} latest is {channel_latest_str}; deployed target is {target}; \
                     set spec.autoUpgrade=true or spec.pin to advance",
                    ch = spec.channel,
                    target = target_for_patch,
                ),
            ),
        };
        condition(
            COND_UPGRADE_AVAILABLE,
            "True",
            reason,
            &message,
            &prior_conds,
        )
    } else {
        condition(
            COND_UPGRADE_AVAILABLE,
            "False",
            "UpToDate",
            &format!(
                "deployed target {target} is the latest in channel {ch}",
                ch = spec.channel,
                target = target_for_patch
            ),
            &prior_conds,
        )
    };
    upsert_condition(&mut new_status, cond_upgrade);

    // UpstreamReachable — re-evaluated here so the NORMAL (non-in-
    // flight) write captures a transition-classification failure
    // (`upstream_poll_error` may have been set after the early call
    // above, which runs before the in-flight gate). Same truthful
    // semantics: False on any upstream error this cycle, True on a
    // successful poll, prior carried forward on a throttled-no-error
    // cycle. A failed poll never advances `lastUpstreamCheck`, so a
    // throttled cycle always follows a SUCCESS — True is correct.
    set_upstream_reachable(
        &mut new_status,
        should_poll_oci,
        upstream_poll_error.as_deref(),
        &prior_conds,
    );

    let cond_migration = match &migration_pending {
        Some(state) => {
            let plan_part = match &state.plan_name {
                Some(name) => {
                    format!(" — see MigrationPlan {MIGRATION_PLAN_NAMESPACE}/{name}")
                }
                None => String::new(),
            };
            condition(
                COND_MIGRATION_PENDING,
                "True",
                &state.classification,
                &format!(
                    "change from {current_target} → {desired_target} classified as {cls}; \
                     manual approval required{plan_part}",
                    desired_target = desired.target_revision,
                    cls = state.classification,
                ),
                &prior_conds,
            )
        }
        None => condition(
            COND_MIGRATION_PENDING,
            "False",
            "Clean",
            "no destructive diff pending",
            &prior_conds,
        ),
    };
    upsert_condition(&mut new_status, cond_migration);

    let cond_synced = if patched_this_cycle {
        condition(
            COND_SYNCED,
            "True",
            "Patched",
            &format!(
                "PlatformController patched parent Application (target={target_for_patch}); \
                 values_changed={values_changed}, target_changed={target_changed}"
            ),
            &prior_conds,
        )
    } else {
        condition(
            COND_SYNCED,
            "True",
            "Reconciled",
            "parent Application matches PlatformStack desired state",
            &prior_conds,
        )
    };
    upsert_condition(&mut new_status, cond_synced);

    let cond_unauthorized = match &foreign_writer {
        Some(foreign) => condition(
            COND_UNAUTHORIZED_SOURCE_MODIFICATION,
            "True",
            "ForeignFieldManager",
            &format!(
                "detected external write to spec.source by field manager {foreign:?}; \
                 PlatformController force-reverted"
            ),
            &prior_conds,
        ),
        None => condition(
            COND_UNAUTHORIZED_SOURCE_MODIFICATION,
            "False",
            "Clean",
            "no foreign writer detected on spec.source",
            &prior_conds,
        ),
    };
    upsert_condition(&mut new_status, cond_unauthorized);

    // `Ready` mirrors parent's aggregate health — True iff
    // Argo CD reports the parent platform Application Healthy
    // (which in turn requires all child Applications +
    // their workloads at their chart-defined health threshold).
    // Walk-fix B.1.74. Sourcing from parent status saves us
    // walking each child individually; Argo CD's app-controller
    // does the aggregation work already.
    let parent_health = parent_json
        .pointer("/status/health/status")
        .and_then(Value::as_str)
        .unwrap_or("");
    let cond_ready = if parent_health == "Healthy" {
        condition(
            COND_READY,
            "True",
            "Healthy",
            "parent platform Application reports Healthy",
            &prior_conds,
        )
    } else {
        condition(
            COND_READY,
            "False",
            "ParentNotHealthy",
            &format!(
                "parent platform Application health is {h:?} (target {target}); \
                 platform reconciling or degraded",
                h = parent_health,
                target = target_for_patch
            ),
            &prior_conds,
        )
    };
    upsert_condition(&mut new_status, cond_ready);

    // `YankedVersion` condition (B.1.74a). True iff the
    // currently-deployed target is annotated `yanked: true` in
    // the compatibility metadata; informational only (does NOT
    // flip `Ready=False`, does NOT force an upgrade — yanked is
    // chart-author hint, not a policy override).
    //
    // We only re-evaluate when we pulled the compat doc this
    // cycle. On throttled (no-poll) reconciles the prior
    // condition value carries forward via `new_status =
    // stack.status.clone()`.
    if let Some(doc) = &compat_doc {
        let yanked_entry = doc.get(&target_for_patch).filter(|r| r.yanked);
        let cond_yanked = match yanked_entry {
            Some(rec) => condition(
                COND_YANKED_VERSION,
                "True",
                "Yanked",
                &format!(
                    "currentVersion {target_for_patch} is yanked: {reason}",
                    reason = rec
                        .yanked_reason
                        .as_deref()
                        .unwrap_or("(no reason supplied — see compatibility.yaml)")
                ),
                &prior_conds,
            ),
            None => condition(
                COND_YANKED_VERSION,
                "False",
                "NotYanked",
                "currentVersion is not marked yanked in compatibility metadata",
                &prior_conds,
            ),
        };
        upsert_condition(&mut new_status, cond_yanked);
    }

    // versionHistory ring buffer (B.1.74). Only record on a
    // SUCCESSFUL bump of `targetRevision` — values-only patches
    // and no-op reconciles don't constitute a version
    // transition. `target_changed` captures pre-patch state
    // (current_target != desired) and the patch must have
    // actually included a new target (so we exclude the policy-
    // refused / MigrationPending branches where target_for_patch
    // == current_target).
    let appended_history = target_changed && target_for_patch != current_target;
    // Walk-fix #4 post-B.1.77: explicit logging of the bump
    // decision + history snapshot so future walks diagnose
    // missing versionHistory entries without source-level
    // tracing rebuilds. Each value here governs whether the
    // SSA patch carries the `versionHistory` field claim:
    //
    //   * `appended_history=true` ⇒ build_status_patch sends
    //     the new vector and `platform-controller` claims
    //     ownership of `f:status.f:versionHistory`.
    //   * `appended_history=false` ⇒ field stripped from the
    //     patch body; server preserves the existing value.
    //
    // If the field never appears in `managedFields[*].fieldsV1
    // .f:status` after a series of bumps, this log line is
    // the first place to look.
    info!(
        target_changed,
        appended_history,
        target_for_patch = %target_for_patch,
        current_target = %current_target,
        prior_history_len = stack
            .status
            .as_ref()
            .and_then(|s| s.version_history.as_ref())
            .map_or(0, |v| v.len()),
        "PlatformController bump decision"
    );
    if appended_history {
        append_version_history(
            &mut new_status,
            PlatformStackVersionHistoryEntry {
                version: target_for_patch.clone(),
                applied_at: now.to_rfc3339(),
                outcome: "succeeded".into(),
            },
        );
    }

    new_status.current_version = Some(target_for_patch.clone());
    new_status.target_version = Some(target_for_patch);
    info!(
        include_version_history = appended_history,
        new_history_len = new_status.version_history.as_ref().map_or(0, |v| v.len()),
        "PlatformController writing status"
    );
    write_status_if_changed(&stack, &ctx, new_status, appended_history).await?;
    Ok(Action::requeue(parse_check_interval(
        &spec.source.check_interval,
    )))
}

/// Strict semver comparison: returns true iff `a > b`. Falls back
/// to `false` when either side is unparseable — fail-safe (better
/// to not fire `UpgradeAvailable` than to flap on garbage).
fn semver_gt(a: &str, b: &str) -> bool {
    match (Version::parse(a), Version::parse(b)) {
        (Ok(av), Ok(bv)) => av > bv,
        _ => false,
    }
}

/// Walk `candidates_desc` (newest first) and return the version
/// string of the first entry whose compatibility record is NOT
/// `yanked: true`. Candidates missing from the doc are treated
/// as not yanked (older versions outside the doc's history
/// window — surfaced to fresh clusters without warning so we
/// don't break installations of long-lived earlier versions).
///
/// All candidates yanked is a chart-author error condition;
/// for safety we return the top tag anyway so the cluster
/// stays on a defined version. The yanked status separately
/// surfaces via the `YankedVersion` condition.
///
/// Track B.1.74a.
fn resolve_non_yanked_latest(candidates_desc: &[Version], doc: &CompatibilityDoc) -> String {
    for v in candidates_desc {
        let key = v.to_string();
        let yanked = doc.get(&key).is_some_and(|r| r.yanked);
        if !yanked {
            return key;
        }
    }
    candidates_desc[0].to_string()
}

/// ADR 0041 FAST PATH resolver. The compat doc fetched from the
/// moving `<repo>:<channel>` tag is cumulative: it lists every
/// published version with its yank status. Enumerate those
/// versions, keep the ones that PARSE as semver, MATCH the
/// channel (`channel_matches`), and are NOT `yanked: true`
/// (read exactly as `resolve_non_yanked_latest` does:
/// `record.yanked`), and return the semver-max.
///
/// `cap` (the chart's own `org.opencontainers.image.version`,
/// when known) bounds the result: any key STRICTLY GREATER than
/// the channel-latest chart's own version is a phantom entry — a
/// compat record for a version whose chart hasn't been published
/// — and is dropped, so `availableVersion` can never point at a
/// tag Argo CD cannot pull. `None` cap = no bound (older charts
/// without the annotation; documented degrade to prior
/// behaviour).
///
/// Returns `None` when the doc declares no usable version for
/// the channel (every candidate yanked, channel-filtered out,
/// above the cap, or the doc empty / all keys unparseable). The
/// reconcile loop treats `None` as a signal to fall back to the
/// paginated tag listing.
fn latest_non_yanked_in_compat(
    doc: &CompatibilityDoc,
    channel: Channel,
    cap: Option<&Version>,
) -> Option<Version> {
    doc.iter()
        .filter(|(_, record)| !record.yanked)
        .filter_map(|(key, _)| Version::parse(key).ok())
        .filter(|v| channel_matches(v, channel))
        .filter(|v| cap.is_none_or(|c| v <= c))
        .max()
}

/// Decide how to DEGRADE when channel-latest resolution fails,
/// keeping DETECTION failures from blocking ENFORCEMENT. Returns
/// the `(channel_latest, did_poll_oci=false, compat_doc=None)`
/// tuple to use, or `None` when there is genuinely no target to
/// deploy (unpinned AND no prior-resolved version) — the caller
/// propagates the error in that case.
///
/// - pinned: always `Some` — the pin is the target; `available`
///   degrades to the best-known prior (empty string if none).
/// - unpinned with a prior: `Some(prior)` — keep the last-known
///   target rather than regress a healthy deploy on a blip.
/// - unpinned with no prior: `None` — nothing to deploy.
fn degrade_on_resolution_failure(
    pinned: bool,
    prior_available: Option<&str>,
) -> Option<(String, bool, Option<CompatibilityDoc>)> {
    match (pinned, prior_available) {
        (true, prior) => Some((prior.unwrap_or_default().to_string(), false, None)),
        (false, Some(prior)) => Some((prior.to_string(), false, None)),
        (false, None) => None,
    }
}

/// Upsert the `UpstreamReachable` condition reflecting THIS
/// cycle's upstream outcome. Set only when we have a definitive
/// signal — we attempted a poll (`should_poll`) or a later
/// upstream fetch failed (`poll_error` is `Some`); otherwise a
/// throttled-no-error cycle leaves the prior value in place
/// (already cloned into `new_status`). Factored out so BOTH the
/// in-flight early-return path AND the normal status write surface
/// the same truthful signal — without it, an in-flight cycle that
/// also degraded would persist a stale `UpstreamReachable=True`.
fn set_upstream_reachable(
    new_status: &mut PlatformStackStatus,
    should_poll: bool,
    poll_error: Option<&str>,
    prior: &[PlatformStackCondition],
) {
    if !(should_poll || poll_error.is_some()) {
        return;
    }
    let c = match poll_error {
        Some(err) => condition(
            COND_UPSTREAM_REACHABLE,
            "False",
            "PollFailed",
            &format!(
                "could not resolve channel-latest from upstream ({err}); availableVersion is \
                 stale — pinned / last-known target still enforced"
            ),
            prior,
        ),
        None => condition(
            COND_UPSTREAM_REACHABLE,
            "True",
            "Reachable",
            "channel-latest resolved from the OCI upstream",
            prior,
        ),
    };
    upsert_condition(new_status, c);
}

/// Resolve the channel-latest: ADR 0041 fast path (the moving
/// `:<channel>` compat doc) → paginated tag-listing fallback.
/// Returns the `(channel_latest, did_poll_oci=true, compat_doc)`
/// tuple the reconcile body consumes, or an `Err` when BOTH the
/// fast path and the fallback fail. Pulled out of the reconcile so
/// the caller can DEGRADE on `Err` (enforce the pin / keep the
/// last-known target) instead of aborting — see the "DETECTION vs
/// ENFORCEMENT" note at the call site. `channel_label` is the raw
/// `spec.channel` string, for logging only.
async fn resolve_channel_latest(
    upstream: &str,
    channel: Channel,
    channel_label: &str,
) -> Result<(String, bool, Option<CompatibilityDoc>), Error> {
    let channel_tag = channel.as_tag();
    match fetch_compatibility_doc_with_self_version(upstream, channel_tag).await {
        Ok((doc, self_version)) => {
            match latest_non_yanked_in_compat(&doc, channel, self_version.as_ref()) {
                Some(v) => {
                    info!(
                        channel = %channel_label,
                        channel_tag,
                        resolved = %v,
                        "resolved channel-latest from :<channel> compat doc (ADR 0041 fast path)"
                    );
                    Ok((v.to_string(), true, Some(doc)))
                }
                None => {
                    // The channel doc exists but yields no usable
                    // version (all yanked / channel-filtered out /
                    // empty / above the self-version cap). Fall back
                    // to the listing.
                    warn!(
                        channel = %channel_label,
                        channel_tag,
                        "channel-tag compat doc declared no usable version; \
                         falling back to tag listing"
                    );
                    resolve_via_tag_listing(upstream, channel).await
                }
            }
        }
        Err(CompatError::ManifestNotFound(reference)) => {
            // Pre-contract chart: no `:<channel>` tag (404 /
            // MANIFEST_UNKNOWN). Fall back to the paginated tag
            // listing.
            info!(
                channel = %channel_label,
                channel_tag,
                reference,
                "no :<channel> tag (pre-contract chart); falling back to tag listing"
            );
            resolve_via_tag_listing(upstream, channel).await
        }
        // Network / auth / parse / missing-file / blob 404 — a real
        // failure. Surface it to the caller, which decides whether
        // to degrade (pinned / last-known) or propagate.
        Err(e) => Err(Error::from(e)),
    }
}

/// ADR 0041 FALLBACK path: resolve the channel-latest via the
/// paginated OCI tag listing (the pre-ADR-0041 mechanism, kept
/// solely as the backstop). Lists every channel-matching tag
/// (`tags_in_channel`, newest-first), pulls the compat doc from
/// the top tag, and walks the candidates skipping yanked entries
/// (`resolve_non_yanked_latest`). Returns the same
/// `(channel_latest_str, did_poll_oci=true, compat_doc)` tuple
/// the fast path produces so the reconcile body is agnostic to
/// which path resolved.
async fn resolve_via_tag_listing(
    upstream: &str,
    channel: Channel,
) -> Result<(String, bool, Option<CompatibilityDoc>), Error> {
    let candidates = tags_in_channel(upstream, channel).await?;
    let top_tag = candidates[0].to_string();
    let doc = match fetch_compatibility_doc(upstream, &top_tag).await {
        Ok(d) => Some(d),
        Err(e) => {
            warn!(
                error = %e,
                "failed to pull compatibility doc from top tag {top_tag}; \
                 yank filter inactive this cycle"
            );
            None
        }
    };
    let resolved = match &doc {
        Some(d) => resolve_non_yanked_latest(&candidates, d),
        None => top_tag,
    };
    Ok((resolved, true, doc))
}

/// Has PlatformController already taken SSA ownership of any
/// `spec.source` field? Used to decide whether to send a no-op
/// SSA patch on the first reconcile (so future foreign writes
/// get caught by `detect_outside_writer`). Without this the
/// initial reconcile would skip the patch entirely whenever
/// `target_changed==false && values_changed==false` and never
/// register the field manager.
fn platform_controller_owns_source(parent: &Value) -> bool {
    let Some(entries) = parent
        .pointer("/metadata/managedFields")
        .and_then(Value::as_array)
    else {
        return false;
    };
    entries.iter().any(|e| {
        e.get("manager").and_then(Value::as_str) == Some(FIELD_MANAGER)
            && e.get("fieldsV1")
                .and_then(|v| v.get("f:spec"))
                .and_then(|s| s.get("f:source"))
                .is_some()
    })
}

fn is_in_flight(parent: &Value) -> bool {
    let sync = parent
        .pointer("/status/sync/status")
        .and_then(Value::as_str)
        .unwrap_or("");
    let phase = parent
        .pointer("/status/operationState/phase")
        .and_then(Value::as_str)
        .unwrap_or("");
    sync == "OutOfSync" || phase == "Running"
}

fn values_differ(parent: &Value, desired: &Value) -> bool {
    let current = parent
        .pointer("/spec/source/helm/valuesObject")
        .cloned()
        .unwrap_or(Value::Null);
    &current != desired
}

/// Field managers whose write to parent App `spec.source` is
/// considered legitimate and does NOT trip the
/// `UnauthorizedSourceModification` condition.
///
/// * `platform-controller` — this controller. Obviously OK.
/// * `argocd-application-controller` — Argo CD writes status,
///   never spec.source, but its entry shows up in `managedFields`
///   so we whitelist defensively.
/// * `apprafter-cli` — the loader's initial SSA apply of the
///   root Application during bootstrap (walk-fix v0.1.117 →
///   v0.1.118 switched the loader from client-side to SSA with
///   this field manager). PlatformController takes ownership on
///   first reconcile via force=true patch; whitelisting prevents
///   the bootstrap state from looping
///   `UnauthorizedSourceModification=True`.
const WHITELISTED_FIELD_MANAGERS: &[&str] = &[
    FIELD_MANAGER,
    "argocd-application-controller",
    "apprafter-cli",
];

fn detect_outside_writer(parent: &Value) -> Option<String> {
    let entries = parent
        .pointer("/metadata/managedFields")
        .and_then(Value::as_array)?;
    for entry in entries {
        let manager = entry.get("manager").and_then(Value::as_str)?;
        if WHITELISTED_FIELD_MANAGERS.contains(&manager) {
            continue;
        }
        let fields = entry
            .get("fieldsV1")
            .and_then(|v| v.get("f:spec"))
            .and_then(|s| s.get("f:source"));
        if fields.is_some_and(|s| s.get("f:targetRevision").is_some() || s.get("f:helm").is_some())
        {
            return Some(manager.to_string());
        }
    }
    None
}

async fn patch_application(
    apps: &Api<DynamicObject>,
    desired: &DesiredSource,
    pending: &Option<PendingUpgrade>,
) -> Result<(), Error> {
    // PlatformController is the single writer for parent
    // Application's `spec.source.{targetRevision, helm.valuesObject}`
    // per the B.1.73 design. Always SSA with `force=true` —
    // negotiating ownership against `kubectl-patch`,
    // `kubectl-edit`, or the loader's `apprafter-cli` would
    // produce unrecoverable 409 deadlocks when reconciles fire
    // before the foreign writer has been displaced. Foreign
    // writes get surfaced via the `UnauthorizedSourceModification`
    // condition; the patch itself is unconditional.
    info!(
        target = %desired.target_revision,
        "SSA-patching parent platform Application (force=true)"
    );
    let payload = build_application_patch(desired, pending);
    let params = PatchParams::apply(FIELD_MANAGER).force();
    apps.patch(PARENT_APPLICATION_NAME, &params, &Patch::Apply(&payload))
        .await?;
    Ok(())
}

fn build_application_patch(desired: &DesiredSource, pending: &Option<PendingUpgrade>) -> Value {
    // apiVersion + kind + metadata.name are REQUIRED in every SSA
    // patch body — the apiserver uses them to resolve the target
    // resource's schema. Same TypeMeta contract that
    // `build_status_patch` enforces for PlatformStack writes.
    //
    // `metadata.annotations` carries the ADR-0048 pending-upgrade
    // surface ONLY while a destructive transition is gated behind
    // a `pending-approval` MigrationPlan. On every other path
    // (`pending == None`) the field is omitted entirely:
    // `platform-controller` owns these keys, so an annotation-free
    // body makes SSA prune any set stamped on a prior gated cycle
    // — the approval banner clears as soon as the gate releases.
    let mut metadata = json!({ "name": PARENT_APPLICATION_NAME });
    if let Some(up) = pending {
        metadata["annotations"] = json!({
            "apprafter.io/upgrade-pending": "true",
            "apprafter.io/upgrade-from": up.from,
            "apprafter.io/upgrade-to": up.to,
            "apprafter.io/upgrade-class": up.class,
            "apprafter.io/upgrade-plan": up.plan,
        });
    }
    json!({
        "apiVersion": "argoproj.io/v1alpha1",
        "kind": "Application",
        "metadata": metadata,
        "spec": {
            "source": {
                "targetRevision": desired.target_revision,
                "helm": { "valuesObject": desired.helm_values },
            }
        }
    })
}

async fn write_status(
    stack: &PlatformStack,
    ctx: &Context,
    mut new_status: PlatformStackStatus,
    _include_version_history: bool,
) -> Result<(), Error> {
    let api: Api<PlatformStack> = Api::namespaced(ctx.client.clone(), SINGLETON_NAMESPACE);
    let name = stack.name_any();

    // Walk-fix #5 post-B.1.77: read server state + merge
    // `versionHistory` BEFORE SSA-apply. Replaces the
    // walk-fix #7 "omit field to preserve value" pattern,
    // which was incorrect for SSA Apply semantics — when a
    // field manager re-applies without a previously-owned
    // field, ownership is RELINQUISHED, and if no other
    // manager owns it, **apiserver removes the field**
    // (Kubernetes SSA spec). The omit pattern thus deleted
    // entries one reconcile after they landed.
    //
    // The race walk-fix #7 was originally guarding against:
    // a follow-up reconcile fires from our own status write,
    // reads the watcher cache (lagged → no entry visible),
    // writes the stale vector back. Solution: fetch the
    // authoritative server-side `versionHistory` per write
    // and merge with whatever local entries the current
    // reconcile produced. The cache's value is irrelevant —
    // we always reflect server truth.
    //
    // Cost: one extra `Api::get_status` round-trip per
    // write. `write_status_if_changed` shortcuts no-op
    // writes, so steady-state reconciles (no diff) don't
    // pay the extra GET.
    let server_state = api.get_status(&name).await?;
    let server_history = server_state
        .status
        .as_ref()
        .and_then(|s| s.version_history.clone())
        .unwrap_or_default();
    let our_history = new_status.version_history.clone().unwrap_or_default();
    new_status.version_history = Some(crate::status::merge_version_history(
        server_history,
        our_history,
    ));

    // SSA REQUIRES apiVersion + kind + metadata.name in the
    // patch body — the apiserver uses them to look up the
    // resource's OpenAPI schema before merging. Walk-found
    // bug v0.1.115 → v0.1.116: a `{"status": {...}}` patch
    // alone hits the apiserver with `invalid object type: /,
    // Kind=` (empty GroupVersion, empty Kind).
    //
    // Always include `versionHistory` in the SSA body
    // (`include_version_history=true`) — see comment above
    // for the SSA ownership-release rationale. The
    // `_include_version_history` parameter is retained as a
    // no-op for binary compatibility with existing call
    // sites; future cleanup removes it.
    let patch = build_status_patch(&name, &new_status, true);

    // `force=true` for the same reason MigrationController's
    // status write uses it (walk-found bug v0.1.126 →
    // v0.1.127 on the migration side): a manual `kubectl
    // patch --subresource=status` registers a foreign field
    // manager as the owner of the status field we just
    // overwrote, and the next reconcile SSA-applies the
    // controller's desired status under
    // `platform-controller` — without `.force()` that 409s
    // with a managedFields conflict and freezes the loop.
    api.patch_status(
        &name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&patch),
    )
    .await?;
    Ok(())
}

/// Skip the SSA status patch when the computed status is
/// byte-equal to what's already on the resource. Walk-found
/// bug v0.1.118 → v0.1.119: every reconcile bumped
/// `lastUpstreamCheck` and other timestamps unconditionally,
/// which made every SSA patch a real change, which fired a
/// watch event, which kicked off another reconcile. Result:
/// hundreds of reconciles per second in a tight loop.
///
/// The skip predicate combines with the OCI-poll throttle
/// (`MIN_OCI_POLL_INTERVAL_SECS`): on intermediate reconciles
/// the throttle preserves prior `lastUpstreamCheck` +
/// `availableVersion`, and `condition()` preserves
/// `lastTransitionTime` when status is unchanged. So a
/// no-op reconcile produces a byte-equal `new_status` and
/// this function short-circuits, breaking the loop.
///
/// `include_version_history` — when false, the SSA patch body
/// OMITS the `versionHistory` field entirely so server-side
/// state on that field is preserved. Walk-fix #7 v0.1.121 →
/// v0.1.122: previously the reconcile loop always serialized
/// `version_history` from `new_status` (which started as a
/// clone of the stale-cache `stack.status`), so a
/// race-fired second reconcile could overwrite a freshly-
/// appended entry from the first reconcile with an older
/// cached list. Including the field only when this cycle
/// actually appended makes the write idempotent w.r.t. that
/// race.
async fn write_status_if_changed(
    stack: &PlatformStack,
    ctx: &Context,
    new_status: PlatformStackStatus,
    include_version_history: bool,
) -> Result<(), Error> {
    let prior = stack.status.clone().unwrap_or_default();
    if prior == new_status {
        return Ok(());
    }
    write_status(stack, ctx, new_status, include_version_history).await
}

fn build_status_patch(
    name: &str,
    new_status: &PlatformStackStatus,
    include_version_history: bool,
) -> Value {
    // Serialize the status as JSON Value, then conditionally
    // strip `versionHistory` so server-side state on that
    // append-only field is preserved across racy reconciles.
    // Walk-fix #7 v0.1.121 → v0.1.122. See
    // `write_status_if_changed` docstring for the rationale.
    let mut status_value = serde_json::to_value(new_status)
        .expect("PlatformStackStatus is always serializable to JSON");
    if !include_version_history {
        if let Value::Object(map) = &mut status_value {
            map.remove("versionHistory");
        }
    }
    json!({
        "apiVersion": "apprafter.io/v1alpha1",
        "kind": "PlatformStack",
        "metadata": { "name": name },
        "status": status_value,
    })
}

fn parse_check_interval(s: &str) -> Duration {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return DEFAULT_REQUEUE;
    }
    let (digits, unit) = bytes.split_at(bytes.len() - 1);
    let Ok(num_str) = std::str::from_utf8(digits) else {
        return DEFAULT_REQUEUE;
    };
    let Ok(value) = num_str.parse::<u64>() else {
        return DEFAULT_REQUEUE;
    };
    let secs = match unit[0] {
        b's' => value,
        b'm' => value * 60,
        b'h' => value * 3600,
        _ => return DEFAULT_REQUEUE,
    };
    Duration::from_secs(secs)
}

fn error_policy(_: Arc<PlatformStack>, err: &Error, _: Arc<Context>) -> Action {
    warn!(error = %err, "PlatformController reconcile failed");
    Action::requeue(Duration::from_secs(60))
}

/// In-memory marker the reconcile body sets when a destructive
/// transition is gated by a `MigrationPlan`. Drives:
///
///   * `MigrationPending` condition (reason + message).
///   * `UpgradeAvailable` condition's plan-aware
///     `BlockedByMigrationPlan` reason (vs the generic
///     `ManualApprovalRequired` for pin-unset + autoUpgrade-
///     false flows).
///
/// Walk-fix B.1.78. Prior shape was `Option<ChangeClass>` —
/// sufficient when controller never created or read plans;
/// fails to surface the plan name in condition messages so
/// `kubectl describe platformstack default` shows nothing the
/// operator can navigate from.
#[derive(Debug, Clone)]
struct MigrationPendingState {
    classification: String,
    plan_name: Option<String>,
}

/// ADR 0048 — the upgrade metadata stamped onto the root Argo
/// `Application` (`argocd/platform`) as machine-readable
/// `apprafter.io/upgrade-*` annotations while a destructive
/// transition is gated behind a `pending-approval`
/// `MigrationPlan`. `platform-controller` is the sole owner of
/// these keys, so emitting the patch body WITHOUT them (the
/// `&None` arm of `build_application_patch`) makes SSA prune any
/// previously-stamped set — the optional approval banner clears
/// the moment the gate releases (operator approves the plan, or
/// the transition turns out non-destructive).
#[derive(Debug, Clone)]
struct PendingUpgrade {
    from: String,
    to: String,
    class: String,
    plan: String,
}

/// Build a deterministic `MigrationPlan` CR name from a
/// `(from_version, to_version)` pair. DNS-1123 names disallow
/// dots — replace them with dashes. Repeated reconciles of
/// the same transition arrive at the same name, making
/// `Api::create` idempotent (404 → create; otherwise reuse).
fn synthesize_platform_plan_name(from_version: &str, to_version: &str) -> String {
    format!(
        "platform-{}-to-{}",
        from_version.replace('.', "-"),
        to_version.replace('.', "-")
    )
}

/// Convert the internal `ChangeClass` enum to the string token
/// the MigrationPlan's `spec.risks.classification` schema
/// accepts. Mirrors the `compatibility.cue` change-class
/// vocabulary.
fn change_class_to_string(class: ChangeClass) -> &'static str {
    match class {
        ChangeClass::Safe => "safe",
        ChangeClass::RequiresRestart => "requires-restart",
        ChangeClass::DataMigration => "data-migration",
        ChangeClass::Breaking => "breaking",
    }
}

/// Extract the classification field from an existing
/// `MigrationPlan`. Returns `None` when the field is absent
/// (e.g. the plan was created manually without
/// `spec.risks.classification`). Used to surface the existing
/// plan's classification on subsequent reconciles so that the
/// operator's `kubectl describe` output stays consistent
/// across reconciles even when the controller doesn't
/// re-classify.
fn plan_classification(plan: &MigrationPlan) -> Option<String> {
    plan.spec.risks.as_ref().map(|r| r.classification.clone())
}

/// SSA-create a `MigrationPlan` CR for a platform-scope
/// destructive transition.
///
/// Caller guarantees the plan does NOT already exist (404
/// path in the reconcile body); concurrent creates from
/// other operators are protected by name uniqueness — the
/// apiserver returns 409 Conflict that propagates up to
/// reconcile's error_policy, which requeues 60s. The next
/// reconcile sees the existing plan via the GET path and
/// blocks the bump.
async fn create_platform_migration_plan(
    api: &Api<MigrationPlan>,
    plan_name: &str,
    from_version: &str,
    to_version: &str,
    class: ChangeClass,
    current_pin: Option<&str>,
    anchor_uid: Option<&str>,
) -> Result<(), Error> {
    let plan = build_platform_migration_plan_cr(
        plan_name,
        from_version,
        to_version,
        class,
        current_pin,
        anchor_uid,
    );
    api.create(&PostParams::default(), &plan).await?;
    Ok(())
}

/// Pure builder for a platform-scope `MigrationPlan` CR.
/// Pulled out so unit tests can pin the resulting shape
/// without a kube client.
fn build_platform_migration_plan_cr(
    plan_name: &str,
    from_version: &str,
    to_version: &str,
    class: ChangeClass,
    current_pin: Option<&str>,
    anchor_uid: Option<&str>,
) -> MigrationPlan {
    let classification = change_class_to_string(class).to_string();
    // `previousSpecSnapshot.pin` — verbatim copy of the
    // pre-transition `PlatformStack.spec.pin`. `null` (JSON)
    // represents "no pin was set" — channel-following mode.
    // `PlatformMigrationStrategy.reject` reads this back on
    // rejection and SSA-patches `spec.pin` to it (B.1.76).
    let snapshot_pin = match current_pin {
        Some(v) => Value::String(v.to_string()),
        None => Value::Null,
    };
    let spec = MigrationPlanSpec {
        scope: MigrationPlanScope {
            type_: "platform".into(),
            application: None,
            platform: Some(MigrationPlatformScope {
                // 1.78 simplification: list the entire stack
                // as the affected component. Future work can
                // narrow by diff'ing component-level chart
                // values between the two chart versions.
                components: vec!["platform-stack".into()],
            }),
        },
        trigger: MigrationTrigger {
            type_: "platform-classification".into(),
            field: "spec.pin".into(),
            from: Some(Value::String(from_version.to_string())),
            to: Some(Value::String(to_version.to_string())),
        },
        risks: Some(MigrationRisks {
            classification,
            estimated_downtime: None,
            data_volume: None,
            reversible: None,
            requires_full_backup: None,
        }),
        plan: None,
        approvers: None,
        previous_spec_snapshot: Some(serde_json::json!({ "pin": snapshot_pin })),
    };
    let mut mp = MigrationPlan::new(plan_name, spec);
    mp.metadata.namespace = Some(MIGRATION_PLAN_NAMESPACE.to_string());
    // ADR 0048: anchor the plan to the chart-emitted ConfigMap in
    // the SAME namespace so Argo CD's ownerRef walk surfaces it on
    // the platform-stack root Application's tree. `controller` and
    // `block_owner_deletion` are both false — this is a
    // tree-membership anchor, not a lifecycle owner, and we never
    // want it to interfere with GC of either object.
    if let Some(uid) = anchor_uid {
        mp.metadata.owner_references = Some(vec![OwnerReference {
            api_version: "v1".into(),
            kind: "ConfigMap".into(),
            name: PLATFORM_MIGRATION_ANCHOR.into(),
            uid: uid.into(),
            controller: Some(false),
            block_owner_deletion: Some(false),
        }]);
    }
    mp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_check_interval_with_h_m_s_units() {
        assert_eq!(parse_check_interval("6h"), Duration::from_secs(6 * 3600));
        assert_eq!(parse_check_interval("30m"), Duration::from_secs(30 * 60));
        assert_eq!(parse_check_interval("3600s"), Duration::from_secs(3600));
    }

    #[test]
    fn parses_check_interval_defaults_on_garbage() {
        assert_eq!(parse_check_interval(""), DEFAULT_REQUEUE);
        assert_eq!(parse_check_interval("abc"), DEFAULT_REQUEUE);
        assert_eq!(parse_check_interval("10x"), DEFAULT_REQUEUE);
    }

    #[test]
    fn is_in_flight_detects_progressing_phase() {
        let parent = json!({
            "status": {
                "sync": { "status": "Synced" },
                "operationState": { "phase": "Running" }
            }
        });
        assert!(is_in_flight(&parent));
    }

    #[test]
    fn is_in_flight_detects_outofsync() {
        let parent = json!({
            "status": { "sync": { "status": "OutOfSync" } }
        });
        assert!(is_in_flight(&parent));
    }

    #[test]
    fn is_in_flight_false_for_synced_succeeded() {
        let parent = json!({
            "status": {
                "sync": { "status": "Synced" },
                "operationState": { "phase": "Succeeded" }
            }
        });
        assert!(!is_in_flight(&parent));
    }

    #[test]
    fn values_differ_returns_false_when_equal() {
        let parent = json!({
            "spec": { "source": { "helm": { "valuesObject": {"tier": 1} } } }
        });
        let desired = json!({"tier": 1});
        assert!(!values_differ(&parent, &desired));
    }

    #[test]
    fn values_differ_returns_true_when_changed() {
        let parent = json!({
            "spec": { "source": { "helm": { "valuesObject": {"tier": 1} } } }
        });
        let desired = json!({"tier": 2});
        assert!(values_differ(&parent, &desired));
    }

    #[test]
    fn detect_outside_writer_skips_known_managers() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "platform-controller",
                        "fieldsV1": {"f:spec": {"f:source": {"f:targetRevision": {}}}}
                    },
                    {
                        "manager": "argocd-application-controller",
                        "fieldsV1": {"f:status": {}}
                    }
                ]
            }
        });
        assert!(detect_outside_writer(&parent).is_none());
    }

    #[test]
    fn detect_outside_writer_flags_unknown_manager_on_source() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "kubectl-client-side-apply",
                        "fieldsV1": {"f:spec": {"f:source": {"f:targetRevision": {}}}}
                    }
                ]
            }
        });
        assert_eq!(
            detect_outside_writer(&parent),
            Some("kubectl-client-side-apply".to_string())
        );
    }

    #[test]
    fn detect_outside_writer_flags_unknown_manager_on_helm_values() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "helm",
                        "fieldsV1": {"f:spec": {"f:source": {"f:helm": {}}}}
                    }
                ]
            }
        });
        assert_eq!(detect_outside_writer(&parent), Some("helm".to_string()));
    }

    #[test]
    fn detect_outside_writer_ignores_unknown_manager_on_unrelated_fields() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "kubectl-edit",
                        "fieldsV1": {"f:metadata": {"f:annotations": {}}}
                    }
                ]
            }
        });
        assert!(detect_outside_writer(&parent).is_none());
    }

    #[test]
    fn build_status_patch_includes_apiversion_kind_and_name() {
        // Regression guard for walk-fix v0.1.115 → v0.1.116. SSA
        // requires the patch body to carry apiVersion + kind +
        // metadata.name; a bare `{"status": {...}}` body fails
        // with `invalid object type: /, Kind=` from the apiserver
        // (empty GroupVersion, empty Kind) and loops every
        // reconcile retry on the same error.
        let status = PlatformStackStatus {
            current_version: Some("0.1.17".into()),
            ..Default::default()
        };
        let patch = build_status_patch("default", &status, true);
        let map = patch.as_object().expect("patch is JSON object");
        assert_eq!(
            map.get("apiVersion").and_then(Value::as_str),
            Some("apprafter.io/v1alpha1")
        );
        assert_eq!(
            map.get("kind").and_then(Value::as_str),
            Some("PlatformStack")
        );
        assert_eq!(
            patch.pointer("/metadata/name").and_then(Value::as_str),
            Some("default")
        );
        assert_eq!(
            patch
                .pointer("/status/currentVersion")
                .and_then(Value::as_str),
            Some("0.1.17")
        );
    }

    #[test]
    fn build_status_patch_omits_version_history_when_not_appended() {
        // Regression guard for walk-fix v0.1.121 → v0.1.122.
        // Without this guard the controller's own status writes
        // can clobber `versionHistory` on the apiserver: a
        // cache-stale `PlatformStack` snapshot becomes the new
        // SSA body, dropping entries the previous reconcile had
        // already persisted. SSA preserves field values that are
        // ABSENT from the patch body, so skipping
        // `versionHistory` whenever this reconcile cycle did not
        // append is the canonical fix.
        let status = PlatformStackStatus {
            current_version: Some("0.1.20".into()),
            version_history: Some(vec![operator_core::PlatformStackVersionHistoryEntry {
                version: "0.1.19".into(),
                applied_at: "t".into(),
                outcome: "succeeded".into(),
            }]),
            ..Default::default()
        };
        let patch = build_status_patch("default", &status, false);
        // The `versionHistory` field must NOT appear in the SSA
        // patch body — the apiserver keeps the field at its
        // existing value when omitted under server-side apply.
        assert!(
            patch.pointer("/status/versionHistory").is_none(),
            "versionHistory must be absent when include_version_history=false; got {patch:#?}"
        );
        // Other status fields still flow through.
        assert_eq!(
            patch
                .pointer("/status/currentVersion")
                .and_then(Value::as_str),
            Some("0.1.20")
        );
    }

    #[test]
    fn build_status_patch_includes_version_history_when_appended() {
        // Counterpart guard: when the reconcile DID append a
        // history entry this cycle, the SSA body MUST ship the
        // new vector — otherwise the append silently never
        // persists. Pairs with
        // `build_status_patch_omits_version_history_when_not_appended`.
        let status = PlatformStackStatus {
            current_version: Some("0.1.20".into()),
            version_history: Some(vec![operator_core::PlatformStackVersionHistoryEntry {
                version: "0.1.20".into(),
                applied_at: "2026-05-22T12:00:00+00:00".into(),
                outcome: "succeeded".into(),
            }]),
            ..Default::default()
        };
        let patch = build_status_patch("default", &status, true);
        let history = patch
            .pointer("/status/versionHistory")
            .and_then(Value::as_array)
            .expect("versionHistory present when include_version_history=true");
        assert_eq!(history.len(), 1);
        assert_eq!(
            history[0].get("version").and_then(Value::as_str),
            Some("0.1.20")
        );
    }

    #[test]
    fn semver_gt_compares_strictly_greater() {
        assert!(semver_gt("0.1.19", "0.1.18"));
        assert!(semver_gt("0.2.0", "0.1.99"));
        assert!(semver_gt("1.0.0", "0.99.99"));
    }

    #[test]
    fn semver_gt_returns_false_for_equal() {
        // Critical regression guard: v0.1.116 wrongly fired
        // UpgradeAvailable=True for equal versions (because the
        // old logic used values_differ instead of semver
        // comparison).
        assert!(!semver_gt("0.1.18", "0.1.18"));
    }

    #[test]
    fn semver_gt_returns_false_for_lesser() {
        assert!(!semver_gt("0.1.17", "0.1.18"));
        assert!(!semver_gt("0.1.0", "1.0.0"));
    }

    #[test]
    fn semver_gt_handles_prereleases() {
        // 0.2.0-rc.1 < 0.2.0 per semver precedence.
        assert!(semver_gt("0.2.0", "0.2.0-rc.1"));
        assert!(!semver_gt("0.2.0-rc.1", "0.2.0"));
    }

    #[test]
    fn semver_gt_returns_false_on_unparseable_input() {
        // Fail-safe — bogus version strings must NOT trigger
        // UpgradeAvailable=True. Prefer quiet "no upgrade" to a
        // flapping condition on garbage input.
        assert!(!semver_gt("not-a-version", "0.1.18"));
        assert!(!semver_gt("0.1.18", "garbage"));
        assert!(!semver_gt("", "0.1.18"));
    }

    fn rec(yanked: bool) -> crate::compatibility::VersionRecord {
        // Helper builds a VersionRecord without going through
        // serde — the `change`/`yanked_reason` fields aren't
        // load-bearing for the `resolve_non_yanked_latest`
        // tests, only `yanked`.
        serde_yaml::from_str(&format!(
            "change: safe\nyanked: {yanked}\nyankedReason: \"sample reason\"\n",
        ))
        .expect("compat record YAML")
    }

    #[test]
    fn resolve_non_yanked_latest_picks_top_when_none_yanked() {
        // Baseline: every candidate has yanked=false → return
        // the top version unchanged. Pins B.1.74a no-op behavior
        // when the chart-author hasn't yanked anything.
        let candidates: Vec<Version> = ["0.1.22", "0.1.21", "0.1.20"]
            .iter()
            .map(|s| Version::parse(s).unwrap())
            .collect();
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("0.1.22".to_string(), rec(false));
        doc.insert("0.1.21".to_string(), rec(false));
        doc.insert("0.1.20".to_string(), rec(false));
        assert_eq!(resolve_non_yanked_latest(&candidates, &doc), "0.1.22");
    }

    #[test]
    fn resolve_non_yanked_latest_skips_top_when_yanked() {
        // Core B.1.74a behavior. Top tag is yanked → walk to
        // the next candidate and return that. Fresh clusters
        // resolving channel-latest never land on a yanked
        // version (the design goal of yanking support).
        let candidates: Vec<Version> = ["0.1.22", "0.1.21", "0.1.20"]
            .iter()
            .map(|s| Version::parse(s).unwrap())
            .collect();
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("0.1.22".to_string(), rec(true));
        doc.insert("0.1.21".to_string(), rec(false));
        doc.insert("0.1.20".to_string(), rec(false));
        assert_eq!(resolve_non_yanked_latest(&candidates, &doc), "0.1.21");
    }

    #[test]
    fn resolve_non_yanked_latest_skips_consecutive_yanked() {
        // Multiple consecutive yanks at the top — keep walking
        // until the first non-yanked. Real chart-author
        // scenario: ship 0.1.22 → yank it → ship 0.1.23 → yank
        // it too → fresh clusters fall back to 0.1.21.
        let candidates: Vec<Version> = ["0.1.23", "0.1.22", "0.1.21"]
            .iter()
            .map(|s| Version::parse(s).unwrap())
            .collect();
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("0.1.23".to_string(), rec(true));
        doc.insert("0.1.22".to_string(), rec(true));
        doc.insert("0.1.21".to_string(), rec(false));
        assert_eq!(resolve_non_yanked_latest(&candidates, &doc), "0.1.21");
    }

    #[test]
    fn resolve_non_yanked_latest_treats_missing_entry_as_not_yanked() {
        // Older versions outside the doc's history window are
        // resolvable. The yank marker is purely an opt-in
        // signal; absence == ok. Prevents accidentally blocking
        // resolution on a published version that pre-dates the
        // chart-author's compatibility.cue starting point.
        let candidates: Vec<Version> = ["0.1.22", "0.1.5"]
            .iter()
            .map(|s| Version::parse(s).unwrap())
            .collect();
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("0.1.22".to_string(), rec(true));
        // 0.1.5 is absent from the doc → counts as not yanked.
        assert_eq!(resolve_non_yanked_latest(&candidates, &doc), "0.1.5");
    }

    #[test]
    fn resolve_non_yanked_latest_falls_back_to_top_when_all_yanked() {
        // Pathological case: chart-author yanks everything in
        // the doc. Returning top keeps the cluster on a defined
        // version; the YankedVersion condition will surface the
        // problem separately.
        let candidates: Vec<Version> = ["0.1.22", "0.1.21"]
            .iter()
            .map(|s| Version::parse(s).unwrap())
            .collect();
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("0.1.22".to_string(), rec(true));
        doc.insert("0.1.21".to_string(), rec(true));
        assert_eq!(resolve_non_yanked_latest(&candidates, &doc), "0.1.22");
    }

    fn compat_record(yanked: bool) -> crate::compatibility::VersionRecord {
        // Build a VersionRecord via serde so the `yanked` field is
        // exercised through the same path the real doc uses.
        serde_yaml::from_str(&format!(
            "change: safe\nyanked: {yanked}\nyankedReason: \"sample reason\"\n",
        ))
        .expect("compat record YAML")
    }

    #[test]
    fn latest_non_yanked_in_compat_picks_semver_max_per_channel() {
        // ADR 0041 fast path: a cumulative compat doc with stable
        // + rc versions. The stable channel must return the
        // semver-max STABLE (no pre-release), ignoring the rc and
        // the lower stable. The beta channel must include the rc
        // and return it when it semver-outranks the stable.
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("0.1.20".to_string(), compat_record(false));
        doc.insert("0.1.21".to_string(), compat_record(false));
        doc.insert("0.1.22-rc.1".to_string(), compat_record(false));
        assert_eq!(
            latest_non_yanked_in_compat(&doc, Channel::Stable, None),
            Some(Version::parse("0.1.21").unwrap())
        );
        // Beta accepts the rc, which is the semver-max overall.
        assert_eq!(
            latest_non_yanked_in_compat(&doc, Channel::Beta, None),
            Some(Version::parse("0.1.22-rc.1").unwrap())
        );
    }

    #[test]
    fn latest_non_yanked_in_compat_skips_yanked_top_version() {
        // One stable yanked → return the latest NON-yanked per
        // channel (the yank-walk, read exactly as
        // `resolve_non_yanked_latest` reads `record.yanked`).
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("0.1.20".to_string(), compat_record(false));
        doc.insert("0.1.21".to_string(), compat_record(false));
        doc.insert("0.1.22".to_string(), compat_record(true)); // yanked top
        assert_eq!(
            latest_non_yanked_in_compat(&doc, Channel::Stable, None),
            Some(Version::parse("0.1.21").unwrap())
        );
    }

    #[test]
    fn latest_non_yanked_in_compat_returns_none_when_all_yanked() {
        // All channel-matching versions yanked → None, which the
        // reconcile loop treats as a fall-back-to-listing signal.
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("0.1.21".to_string(), compat_record(true));
        doc.insert("0.1.22".to_string(), compat_record(true));
        assert_eq!(
            latest_non_yanked_in_compat(&doc, Channel::Stable, None),
            None
        );
    }

    #[test]
    fn latest_non_yanked_in_compat_returns_none_when_channel_rejects_all() {
        // Stable channel + only rc entries → no match → None.
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("0.1.22-rc.1".to_string(), compat_record(false));
        doc.insert("0.1.22-rc.2".to_string(), compat_record(false));
        assert_eq!(
            latest_non_yanked_in_compat(&doc, Channel::Stable, None),
            None
        );
        // ...but Edge accepts the rc and returns the max.
        assert_eq!(
            latest_non_yanked_in_compat(&doc, Channel::Edge, None),
            Some(Version::parse("0.1.22-rc.2").unwrap())
        );
    }

    #[test]
    fn latest_non_yanked_in_compat_skips_unparseable_version_keys() {
        // Garbage keys in the doc are ignored; valid entries still
        // resolve.
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("not-a-version".to_string(), compat_record(false));
        doc.insert("0.1.21".to_string(), compat_record(false));
        assert_eq!(
            latest_non_yanked_in_compat(&doc, Channel::Stable, None),
            Some(Version::parse("0.1.21").unwrap())
        );
    }

    #[test]
    fn latest_non_yanked_in_compat_caps_at_self_version() {
        // Finding #1: the cumulative compat doc declares a version
        // (0.2.13) ABOVE the channel-latest chart that carries it
        // (self-version 0.2.12) — e.g. an author prepped the next
        // release's record early. Without the cap the fast path
        // would report 0.2.13 as `availableVersion` and Argo CD
        // would fail to pull a non-existent chart. The cap drops
        // the phantom and resolves to the real published latest.
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("0.2.11".to_string(), compat_record(false));
        doc.insert("0.2.12".to_string(), compat_record(false));
        doc.insert("0.2.13".to_string(), compat_record(false)); // phantom: no chart yet
        let cap = Version::parse("0.2.12").unwrap();
        assert_eq!(
            latest_non_yanked_in_compat(&doc, Channel::Stable, Some(&cap)),
            Some(Version::parse("0.2.12").unwrap())
        );
        // Without the cap (older chart, no annotation) the doc is
        // trusted as-is — the documented degrade to prior
        // behaviour.
        assert_eq!(
            latest_non_yanked_in_compat(&doc, Channel::Stable, None),
            Some(Version::parse("0.2.13").unwrap())
        );
    }

    #[test]
    fn degrade_pinned_enforces_pin_even_with_prior_available() {
        // The deadlock fix: a poll failure must NOT block a pinned
        // stack. degrade returns Some (reconcile proceeds to apply
        // the pin); `available` degrades to the best-known prior.
        let (available, did_poll, doc) =
            degrade_on_resolution_failure(true, Some("0.2.2")).expect("pinned always degrades");
        assert_eq!(available, "0.2.2");
        assert!(!did_poll); // no successful poll this cycle
        assert!(doc.is_none());
    }

    #[test]
    fn degrade_pinned_with_no_prior_available_still_enforces() {
        // Fresh pinned install whose very first poll fails: still
        // Some (the pin gets enforced); `available` is empty
        // (truly unknown) rather than wedging.
        let (available, _, _) =
            degrade_on_resolution_failure(true, None).expect("pinned degrades even with no prior");
        assert_eq!(available, "");
    }

    #[test]
    fn degrade_unpinned_keeps_last_known_target() {
        // Unpinned but previously healthy: keep the last-known
        // target — don't regress a working deploy on a transient
        // resolver blip.
        let (available, _, _) = degrade_on_resolution_failure(false, Some("0.2.11"))
            .expect("unpinned-with-prior degrades to last-known");
        assert_eq!(available, "0.2.11");
    }

    fn upstream_cond(status: &PlatformStackStatus) -> Option<&PlatformStackCondition> {
        status
            .conditions
            .as_ref()
            .and_then(|cs| cs.iter().find(|c| c.type_ == COND_UPSTREAM_REACHABLE))
    }

    #[test]
    fn set_upstream_reachable_marks_false_on_poll_error() {
        let mut s = PlatformStackStatus::default();
        set_upstream_reachable(&mut s, true, Some("registry IO: invalid type: null"), &[]);
        let c = upstream_cond(&s).expect("condition written");
        assert_eq!(c.status, "False");
        assert_eq!(c.reason.as_deref(), Some("PollFailed"));
        assert!(c.message.as_deref().unwrap().contains("invalid type: null"));
    }

    #[test]
    fn set_upstream_reachable_marks_true_on_clean_poll() {
        let mut s = PlatformStackStatus::default();
        set_upstream_reachable(&mut s, true, None, &[]);
        let c = upstream_cond(&s).expect("condition written");
        assert_eq!(c.status, "True");
        assert_eq!(c.reason.as_deref(), Some("Reachable"));
    }

    #[test]
    fn set_upstream_reachable_is_noop_on_throttled_clean_cycle() {
        // No poll attempted (throttled) and no error — leave the
        // prior value untouched (carry forward), don't assert True.
        let mut s = PlatformStackStatus::default();
        set_upstream_reachable(&mut s, false, None, &[]);
        assert!(
            upstream_cond(&s).is_none(),
            "throttled clean cycle must not write the condition"
        );
    }

    #[test]
    fn set_upstream_reachable_marks_false_even_when_throttled_if_error() {
        // The transition-classification failure path: should_poll is
        // false but an upstream error was recorded — must surface.
        let mut s = PlatformStackStatus::default();
        set_upstream_reachable(&mut s, false, Some("connection refused"), &[]);
        let c = upstream_cond(&s).expect("condition written on error even when throttled");
        assert_eq!(c.status, "False");
    }

    #[test]
    fn degrade_unpinned_with_no_prior_propagates() {
        // Unpinned AND never resolved: no target to deploy. None
        // signals the caller to propagate the error (genuinely
        // fatal — there is nothing to enforce).
        assert!(degrade_on_resolution_failure(false, None).is_none());
    }

    #[test]
    fn latest_non_yanked_in_compat_cap_keeps_equal_self_version() {
        // The normal case: the doc's max key EQUALS the chart's
        // own version (the channel-latest's own record). The cap
        // is inclusive (`<=`), so the channel-latest resolves
        // unchanged.
        let mut doc = std::collections::BTreeMap::new();
        doc.insert("0.2.11".to_string(), compat_record(false));
        doc.insert("0.2.12".to_string(), compat_record(false));
        let cap = Version::parse("0.2.12").unwrap();
        assert_eq!(
            latest_non_yanked_in_compat(&doc, Channel::Stable, Some(&cap)),
            Some(Version::parse("0.2.12").unwrap())
        );
    }

    #[test]
    fn platform_controller_owns_source_finds_own_manager() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "platform-controller",
                        "fieldsV1": {"f:spec": {"f:source": {"f:targetRevision": {}}}}
                    }
                ]
            }
        });
        assert!(platform_controller_owns_source(&parent));
    }

    #[test]
    fn platform_controller_owns_source_false_when_only_argocd_present() {
        let parent = json!({
            "metadata": {
                "managedFields": [
                    {
                        "manager": "argocd-application-controller",
                        "fieldsV1": {"f:status": {}}
                    },
                    {
                        "manager": "kubectl-client-side-apply",
                        "fieldsV1": {"f:spec": {"f:source": {"f:targetRevision": {}}}}
                    }
                ]
            }
        });
        assert!(!platform_controller_owns_source(&parent));
    }

    #[test]
    fn platform_controller_owns_source_false_when_metadata_missing() {
        assert!(!platform_controller_owns_source(&json!({})));
    }

    #[test]
    fn parent_object_reference_points_at_argocd_application() {
        // Walk-fix #6 v0.1.119 → v0.1.120: events publish the
        // parent Application as `secondary` so operators can
        // correlate `kubectl describe platformstack default`
        // ↔ `kubectl describe application platform -n argocd`.
        // Pin the shape — accidentally pointing at the wrong
        // kind would break the audit trail.
        let r = parent_object_reference();
        assert_eq!(r.api_version.as_deref(), Some("argoproj.io/v1alpha1"));
        assert_eq!(r.kind.as_deref(), Some("Application"));
        assert_eq!(r.name.as_deref(), Some("platform"));
        assert_eq!(r.namespace.as_deref(), Some("argocd"));
    }

    #[test]
    fn status_equality_treats_identical_payloads_as_noop() {
        // Regression guard for walk-fix v0.1.118 → v0.1.119:
        // `write_status_if_changed` must short-circuit when the
        // computed status matches what's stored. Without this,
        // every reconcile's SSA patch fires a watch event,
        // kicking off another reconcile, looping the controller
        // at hundreds of cycles per second.
        let a = PlatformStackStatus {
            current_version: Some("0.1.20".into()),
            target_version: Some("0.1.20".into()),
            available_version: Some("0.1.20".into()),
            last_upstream_check: Some("2026-05-22T00:54:45+00:00".into()),
            ..PlatformStackStatus::default()
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn status_equality_distinguishes_timestamp_changes() {
        // The flip side of the no-op skip: if lastUpstreamCheck
        // genuinely advances (because we polled OCI), the
        // statuses must compare not-equal so the SSA patch
        // actually fires.
        let mut a = PlatformStackStatus {
            current_version: Some("0.1.20".into()),
            last_upstream_check: Some("2026-05-22T00:54:45+00:00".into()),
            ..PlatformStackStatus::default()
        };
        let mut b = a.clone();
        b.last_upstream_check = Some("2026-05-22T00:55:45+00:00".into());
        assert_ne!(a, b);
        // And same idea for availableVersion bumps.
        a.available_version = Some("0.1.20".into());
        b.available_version = Some("0.1.21".into());
        b.last_upstream_check = a.last_upstream_check.clone();
        assert_ne!(a, b);
    }

    #[test]
    fn build_application_patch_includes_apiversion_kind_name_and_source() {
        // SSA TypeMeta contract for the parent Application
        // patch — same shape requirement as status patch. Carried
        // forward from the v0.1.114 closure (TypeMeta was correct
        // here; this test pins the contract so future refactors
        // can't silently strip it the way write_status did).
        let desired = DesiredSource {
            target_revision: "0.1.17".into(),
            helm_values: json!({"tier": 1}),
        };
        let patch = build_application_patch(&desired, &None);
        assert_eq!(
            patch.get("apiVersion").and_then(Value::as_str),
            Some("argoproj.io/v1alpha1")
        );
        assert_eq!(
            patch.get("kind").and_then(Value::as_str),
            Some("Application")
        );
        assert_eq!(
            patch.pointer("/metadata/name").and_then(Value::as_str),
            Some(PARENT_APPLICATION_NAME)
        );
        assert_eq!(
            patch
                .pointer("/spec/source/targetRevision")
                .and_then(Value::as_str),
            Some("0.1.17")
        );
        assert_eq!(
            patch
                .pointer("/spec/source/helm/valuesObject/tier")
                .and_then(Value::as_i64),
            Some(1)
        );
    }

    #[test]
    fn application_patch_stamps_upgrade_annotations_when_pending() {
        // ADR 0048: while a destructive upgrade is gated behind a
        // pending-approval MigrationPlan, the root Application
        // carries machine-readable `apprafter.io/upgrade-*`
        // annotations (input for the optional approval-banner UI).
        let desired = DesiredSource {
            target_revision: "0.2.24".into(),
            helm_values: json!({"tier": 1}),
        };
        let pending = Some(PendingUpgrade {
            from: "0.2.24".into(),
            to: "0.2.25".into(),
            class: "requires-restart".into(),
            plan: "platform-0-2-24-to-0-2-25".into(),
        });
        let patch = build_application_patch(&desired, &pending);
        assert_eq!(
            patch
                .pointer("/metadata/annotations/apprafter.io~1upgrade-pending")
                .and_then(Value::as_str),
            Some("true")
        );
        assert_eq!(
            patch
                .pointer("/metadata/annotations/apprafter.io~1upgrade-from")
                .and_then(Value::as_str),
            Some("0.2.24")
        );
        assert_eq!(
            patch
                .pointer("/metadata/annotations/apprafter.io~1upgrade-to")
                .and_then(Value::as_str),
            Some("0.2.25")
        );
        assert_eq!(
            patch
                .pointer("/metadata/annotations/apprafter.io~1upgrade-class")
                .and_then(Value::as_str),
            Some("requires-restart")
        );
        assert_eq!(
            patch
                .pointer("/metadata/annotations/apprafter.io~1upgrade-plan")
                .and_then(Value::as_str),
            Some("platform-0-2-24-to-0-2-25")
        );
        // spec.source must remain unchanged by the annotation block.
        assert_eq!(
            patch
                .pointer("/spec/source/targetRevision")
                .and_then(Value::as_str),
            Some("0.2.24")
        );
    }

    #[test]
    fn application_patch_omits_upgrade_annotations_when_not_pending() {
        // No pending upgrade ⇒ no `apprafter.io/upgrade-*`
        // annotation keys. `platform-controller` owns them, so
        // their absence from the SSA body prunes any stamped on a
        // prior (pending) cycle — the banner clears on approval.
        let desired = DesiredSource {
            target_revision: "0.2.25".into(),
            helm_values: json!({"tier": 1}),
        };
        let patch = build_application_patch(&desired, &None);
        // No `annotations` object at all on this path.
        assert!(
            patch.pointer("/metadata/annotations").is_none(),
            "metadata.annotations must be absent so SSA prunes upgrade-* keys"
        );
        // metadata.name is still present.
        assert_eq!(
            patch.pointer("/metadata/name").and_then(Value::as_str),
            Some(PARENT_APPLICATION_NAME)
        );
    }

    #[test]
    fn synthesize_platform_plan_name_replaces_dots_with_dashes() {
        // DNS-1123 names disallow dots — replace with dashes so
        // the synthesized plan name is a valid Kubernetes
        // resource name.
        assert_eq!(
            synthesize_platform_plan_name("0.1.32", "0.1.33"),
            "platform-0-1-32-to-0-1-33"
        );
        // Pre-release suffixes use dashes too — round trip.
        assert_eq!(
            synthesize_platform_plan_name("0.2.0-rc.1", "0.2.0"),
            "platform-0-2-0-rc-1-to-0-2-0"
        );
    }

    #[test]
    fn synthesize_platform_plan_name_is_deterministic() {
        // Repeat calls with same args return identical name.
        // Idempotency of plan creation depends on this.
        let a = synthesize_platform_plan_name("0.1.32", "0.2.0");
        let b = synthesize_platform_plan_name("0.1.32", "0.2.0");
        assert_eq!(a, b);
    }

    #[test]
    fn change_class_to_string_round_trips_known_classes() {
        // CRD's `risks.classification` enum:
        // [safe, requires-restart, data-migration, breaking].
        // Producers (this helper) and consumers (apiserver)
        // must agree on the spelling.
        assert_eq!(change_class_to_string(ChangeClass::Safe), "safe");
        assert_eq!(
            change_class_to_string(ChangeClass::RequiresRestart),
            "requires-restart"
        );
        assert_eq!(
            change_class_to_string(ChangeClass::DataMigration),
            "data-migration"
        );
        assert_eq!(change_class_to_string(ChangeClass::Breaking), "breaking");
    }

    #[test]
    fn build_platform_migration_plan_cr_shape_matches_crd_schema() {
        // Pin every required field of MigrationPlanSpec against
        // CRD validation rules. CRD requires scope.type,
        // scope.platform.components non-empty (for platform
        // type), trigger.type + field, risks.classification.
        let mp = build_platform_migration_plan_cr(
            "platform-0-1-32-to-0-1-33",
            "0.1.32",
            "0.1.33",
            ChangeClass::Breaking,
            Some("0.1.32"),
            None,
        );

        assert_eq!(
            mp.metadata.name.as_deref(),
            Some("platform-0-1-32-to-0-1-33")
        );
        assert_eq!(
            mp.metadata.namespace.as_deref(),
            Some(MIGRATION_PLAN_NAMESPACE)
        );

        // Scope.
        assert_eq!(mp.spec.scope.type_, "platform");
        assert!(mp.spec.scope.application.is_none());
        let platform = mp.spec.scope.platform.as_ref().expect("platform scope");
        assert_eq!(platform.components, vec!["platform-stack".to_string()]);

        // Trigger.
        assert_eq!(mp.spec.trigger.type_, "platform-classification");
        assert_eq!(mp.spec.trigger.field, "spec.pin");
        assert_eq!(
            mp.spec.trigger.from.as_ref().and_then(Value::as_str),
            Some("0.1.32")
        );
        assert_eq!(
            mp.spec.trigger.to.as_ref().and_then(Value::as_str),
            Some("0.1.33")
        );

        // Risks.
        let risks = mp.spec.risks.as_ref().expect("risks set");
        assert_eq!(risks.classification, "breaking");

        // Previous spec snapshot — pin verbatim for reject flow.
        let snapshot = mp
            .spec
            .previous_spec_snapshot
            .as_ref()
            .expect("snapshot set");
        assert_eq!(
            snapshot.pointer("/pin").and_then(Value::as_str),
            Some("0.1.32")
        );
    }

    #[test]
    fn build_platform_migration_plan_cr_snapshot_pin_is_null_when_unpinned() {
        // No pin → snapshot.pin = JSON null. `PlatformMigrationStrategy.reject`
        // reads `null` and clears `PlatformStack.spec.pin`,
        // restoring channel-following mode.
        let mp = build_platform_migration_plan_cr(
            "platform-0-1-32-to-0-2-0",
            "0.1.32",
            "0.2.0",
            ChangeClass::Breaking,
            None,
            None,
        );
        let snapshot = mp.spec.previous_spec_snapshot.as_ref().unwrap();
        assert_eq!(snapshot.pointer("/pin"), Some(&Value::Null));
    }

    #[test]
    fn migration_plan_owner_ref_points_at_anchor() {
        // ADR 0048: when the chart-emitted anchor ConfigMap is
        // present, the plan carries a same-namespace ownerRef to
        // it so Argo CD's ownerRef walk pulls the plan into the
        // platform-stack root Application's resource tree.
        let mp = build_platform_migration_plan_cr(
            "platform-0-1-32-to-0-1-33",
            "0.1.32",
            "0.1.33",
            ChangeClass::Breaking,
            Some("0.1.32"),
            Some("anchor-uid-123"),
        );
        let refs = mp
            .metadata
            .owner_references
            .as_ref()
            .expect("ownerReferences set");
        assert_eq!(refs.len(), 1);
        let owner = &refs[0];
        assert_eq!(owner.api_version, "v1");
        assert_eq!(owner.kind, "ConfigMap");
        assert_eq!(owner.name, "platform-migration-anchor");
        assert_eq!(owner.uid, "anchor-uid-123");
        assert_eq!(owner.controller, Some(false));
        assert_eq!(owner.block_owner_deletion, Some(false));
    }

    #[test]
    fn migration_plan_unowned_when_no_anchor() {
        // No anchor (e.g. older chart) → plan is created
        // un-owned. Still CLI-approvable, just off the Argo tree.
        let mp = build_platform_migration_plan_cr(
            "platform-0-1-32-to-0-1-33",
            "0.1.32",
            "0.1.33",
            ChangeClass::Breaking,
            Some("0.1.32"),
            None,
        );
        assert!(mp.metadata.owner_references.is_none());
    }

    #[test]
    fn plan_classification_returns_string_when_risks_set() {
        use operator_core::{
            MigrationPlanScope, MigrationPlanSpec, MigrationPlatformScope, MigrationRisks,
            MigrationTrigger,
        };
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "platform".into(),
                application: None,
                platform: Some(MigrationPlatformScope {
                    components: vec!["x".into()],
                }),
            },
            trigger: MigrationTrigger {
                type_: "t".into(),
                field: "f".into(),
                from: None,
                to: None,
            },
            risks: Some(MigrationRisks {
                classification: "breaking".into(),
                estimated_downtime: None,
                data_volume: None,
                reversible: None,
                requires_full_backup: None,
            }),
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        let plan = MigrationPlan::new("p", spec);
        assert_eq!(plan_classification(&plan), Some("breaking".to_string()));
    }

    #[test]
    fn plan_classification_returns_none_when_risks_absent() {
        use operator_core::{
            MigrationPlanScope, MigrationPlanSpec, MigrationPlatformScope, MigrationTrigger,
        };
        let spec = MigrationPlanSpec {
            scope: MigrationPlanScope {
                type_: "platform".into(),
                application: None,
                platform: Some(MigrationPlatformScope {
                    components: vec!["x".into()],
                }),
            },
            trigger: MigrationTrigger {
                type_: "t".into(),
                field: "f".into(),
                from: None,
                to: None,
            },
            risks: None,
            plan: None,
            approvers: None,
            previous_spec_snapshot: None,
        };
        let plan = MigrationPlan::new("p", spec);
        assert_eq!(plan_classification(&plan), None);
    }
}
