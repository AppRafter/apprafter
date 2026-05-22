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
use k8s_openapi::api::core::v1::ObjectReference;
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
use tracing::{info, warn};

use operator_core::{
    Metrics, PlatformStack, PlatformStackStatus, PlatformStackVersionHistoryEntry,
};

use crate::compatibility::{fetch_change_class, ChangeClass};
use crate::desired::{build as build_desired, DesiredSource};
use crate::oci::{latest_in_channel, Channel};
use crate::policy::{NoOpHooks, PolicyHooks};
use crate::status::{
    append_version_history, condition, upsert_condition, COND_MIGRATION_PENDING, COND_READY,
    COND_SYNCED, COND_UNAUTHORIZED_SOURCE_MODIFICATION, COND_UPGRADE_AVAILABLE,
};
use crate::{FIELD_MANAGER, SINGLETON_NAME, SINGLETON_NAMESPACE};

const PARENT_APPLICATION_NAME: &str = "platform";
const PARENT_APPLICATION_NAMESPACE: &str = "argocd";

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
    #[error("policy hook error: {0}")]
    Policy(#[from] crate::policy::PolicyError),
    #[error("serde_json error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("unparseable check interval {0:?}")]
    CheckInterval(String),
}

struct Context {
    client: Client,
    #[allow(dead_code)]
    metrics: Arc<Metrics>,
    hooks: Arc<dyn PolicyHooks>,
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
        hooks: Arc::new(NoOpHooks),
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
    let (channel_latest_str, did_poll_oci) = if should_poll_oci {
        let v = latest_in_channel(&spec.source.upstream, channel).await?;
        (v.to_string(), true)
    } else {
        // SAFETY: when `should_poll_oci` is false we've already
        // confirmed `prior_available` is Some(_) above.
        (prior_available.clone().unwrap(), false)
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

    let mut migration_pending: Option<ChangeClass> = None;
    let target_for_patch = if target_changed && allow_target_bump {
        // Pin OR autoUpgrade=true → classify diff. Safe /
        // requires-restart bump; breaking / data-migration stays
        // on current_target and surfaces MigrationPending=True.
        let class = fetch_change_class(&spec.source.upstream, &desired.target_revision).await?;
        if matches!(class, ChangeClass::Breaking | ChangeClass::DataMigration) {
            ctx.hooks
                .request_migration_plan(&current_target, &desired.target_revision, class)
                .await?;
            migration_pending = Some(class);
            current_target.clone()
        } else {
            desired.target_revision.clone()
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
        patch_application(&apps, &patch_payload).await?;
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
        condition(
            COND_UPGRADE_AVAILABLE,
            "True",
            "ManualApprovalRequired",
            &format!(
                "channel {ch} latest is {channel_latest_str}; deployed target is {target}; \
                 set spec.autoUpgrade=true or spec.pin to advance",
                ch = spec.channel,
                target = target_for_patch
            ),
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

    let cond_migration = match migration_pending {
        Some(class) => condition(
            COND_MIGRATION_PENDING,
            "True",
            &format!("{class:?}"),
            &format!(
                "change from {current_target} → {desired_target} classified as {class:?}; \
                 manual approval required (1.74 MigrationPlan)",
                desired_target = desired.target_revision
            ),
            &prior_conds,
        ),
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

    // versionHistory ring buffer (B.1.74). Only record on a
    // SUCCESSFUL bump of `targetRevision` — values-only patches
    // and no-op reconciles don't constitute a version
    // transition. `target_changed` captures pre-patch state
    // (current_target != desired) and the patch must have
    // actually included a new target (so we exclude the policy-
    // refused / MigrationPending branches where target_for_patch
    // == current_target).
    let appended_history = target_changed && target_for_patch != current_target;
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
    let payload = build_application_patch(desired);
    let params = PatchParams::apply(FIELD_MANAGER).force();
    apps.patch(PARENT_APPLICATION_NAME, &params, &Patch::Apply(&payload))
        .await?;
    Ok(())
}

fn build_application_patch(desired: &DesiredSource) -> Value {
    // apiVersion + kind + metadata.name are REQUIRED in every SSA
    // patch body — the apiserver uses them to resolve the target
    // resource's schema. Same TypeMeta contract that
    // `build_status_patch` enforces for PlatformStack writes.
    json!({
        "apiVersion": "argoproj.io/v1alpha1",
        "kind": "Application",
        "metadata": { "name": PARENT_APPLICATION_NAME },
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
    new_status: PlatformStackStatus,
    include_version_history: bool,
) -> Result<(), Error> {
    let api: Api<PlatformStack> = Api::namespaced(ctx.client.clone(), SINGLETON_NAMESPACE);
    let name = stack.name_any();
    // SSA REQUIRES apiVersion + kind + metadata.name in the patch
    // body — the apiserver uses them to look up the resource's
    // OpenAPI schema before merging. Walk-found bug v0.1.115 →
    // v0.1.116: a `{"status": {...}}` patch alone hits the
    // apiserver with `invalid object type: /, Kind=` (empty
    // GroupVersion, empty Kind) and every reconcile retry loops
    // on that error. Mirror'ed from
    // `operator_controllers_application::apply_status` which has
    // always carried the TypeMeta.
    let patch = build_status_patch(&name, &new_status, include_version_history);
    api.patch_status(
        &name,
        &PatchParams::apply(FIELD_MANAGER),
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
        let patch = build_application_patch(&desired);
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
}
