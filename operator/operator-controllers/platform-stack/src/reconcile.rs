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
use k8s_openapi::api::batch::v1::{CronJob, Job};
use k8s_openapi::api::core::v1::{ConfigMap, ObjectReference, Pod};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{Api, ApiResource, DeleteParams, DynamicObject, ListParams, Patch, PatchParams};
use kube::core::GroupVersionKind;
use kube::runtime::controller::{Action, Controller};
use kube::runtime::events::{Event as KubeEvent, EventType, Reporter};
use kube::runtime::reflector::ObjectRef;
use kube::runtime::watcher;
use kube::{Client, Resource, ResourceExt};
use operator_core::events::ObjectRecorder;
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
    append_version_history, condition, platform_controller_view, upsert_condition,
    without_reconcile_stalled, COND_MIGRATION_PENDING, COND_NODE_DISK_PRESSURE, COND_READY,
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
fn build_recorder(ctx: &Context, stack: &PlatformStack) -> ObjectRecorder {
    let reporter = Reporter {
        controller: EVENT_REPORTER_CONTROLLER.into(),
        instance: std::env::var("POD_NAME").ok(),
    };
    let reference = stack.object_ref(&());
    ObjectRecorder::new(ctx.client.clone(), reporter, reference)
}

/// Publish one audit Event, best-effort and bounded by
/// `DECORATIVE_CALL_BUDGET`: an Event that fails or never answers is logged
/// and skipped, never a reason to hold the reconcile.
async fn publish_bounded(recorder: &ObjectRecorder, ev: KubeEvent) {
    let reason = ev.reason.clone();
    match tokio::time::timeout(DECORATIVE_CALL_BUDGET, recorder.publish(ev)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(reason = %reason, error = %e, "failed to publish event (continuing)"),
        Err(_) => warn!(
            reason = %reason,
            budget_secs = DECORATIVE_CALL_BUDGET.as_secs(),
            "event publish did not answer in time (continuing)"
        ),
    }
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

/// Annotation the CLI stamps on the PlatformStack CR to request an
/// immediate upstream re-poll, bypassing the 60s
/// `MIN_OCI_POLL_INTERVAL_SECS` throttle. Holds an RFC3339
/// timestamp; the operator force-polls when it is NEWER than
/// `status.lastUpstreamCheck` (i.e. a recheck was requested since
/// the last successful poll). Self-clearing: a successful poll
/// advances `lastUpstreamCheck` past the request, so the next
/// reconcile won't re-poll for the same request — no annotation
/// removal needed. Backs `apprafter platform status`/`update` so
/// `availableVersion` is never shown stale for up to
/// `spec.source.checkInterval` (default 6h).
const RECHECK_REQUESTED_ANNOTATION: &str = "apprafter.io/recheck-requested";

/// How long one question to the registry may take as a whole (WI-400):
/// resolving the channel-latest — the `:<channel>` pull, or the paginated
/// listing plus a pull when it falls back — or classifying a transition (one
/// pull). Healthy ghcr answers either in 0.5–3 s.
///
/// The registry client's own bounds (`oci::REGISTRY_CONNECT_TIMEOUT`,
/// `oci::REGISTRY_READ_TIMEOUT`) stop a peer that goes SILENT. This stops one
/// that keeps answering too slowly ever to finish: up to `MAX_PAGES` listing
/// requests, or a blob dribbled out a few bytes per read. On expiry the
/// question fails like any registry error, so the reconcile takes its
/// existing degrade paths — `UpstreamReachable=False`, the pin still
/// enforced, a transition held rather than bumped blind — and still writes
/// its status.
const OCI_OPERATION_BUDGET: Duration = Duration::from_secs(20);

/// How long the backup objects' reads may take together (WI-400): three
/// namespaced LISTs and one GET, milliseconds each on a healthy apiserver. On
/// expiry the reads have FAILED, and that is the verdict the condition
/// already gives a failed read: `BackupHealthy=Unknown` (`StateUnreadable`).
const BACKUP_READ_BUDGET: Duration = Duration::from_secs(10);

/// How long the `NodeDiskPressure` sample may take (WI-400): a Node LIST and
/// the kubelet Summary through the apiserver's node proxy. The proxy is a
/// long-running request that the apiserver's own 60s limit does not cover,
/// and the kubelet answers slowest exactly when its node is short of disk. On
/// expiry there is no sample and the condition is left as it was — the
/// existing answer to "we could not look".
const NODE_SAMPLE_BUDGET: Duration = Duration::from_secs(10);

/// How long each decorative call may take (WI-400): the ADR 0048 anchor
/// lookup, the anchor's annotation patch, and each audit Event. The anchor-403
/// fix made these tolerate an ERROR; a call that never answers froze the
/// reconcile before its status write all the same. On expiry each is skipped
/// exactly as when it fails.
const DECORATIVE_CALL_BUDGET: Duration = Duration::from_secs(5);

/// The registry did not finish one question within `OCI_OPERATION_BUDGET`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the OCI upstream did not finish {operation} within {}s", .after.as_secs())]
pub struct UpstreamTimedOut {
    pub operation: &'static str,
    pub after: Duration,
}

/// Ask the registry one question under `OCI_OPERATION_BUDGET`.
async fn within_oci_budget<T>(
    operation: &'static str,
    question: impl std::future::Future<Output = T>,
) -> Result<T, UpstreamTimedOut> {
    tokio::time::timeout(OCI_OPERATION_BUDGET, question)
        .await
        .map_err(|_| UpstreamTimedOut {
            operation,
            after: OCI_OPERATION_BUDGET,
        })
}

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
    #[error(transparent)]
    UpstreamTimedOut(#[from] UpstreamTimedOut),
    #[error(transparent)]
    TimedOut(#[from] operator_core::deadline::ReconcileTimedOut),
}

struct Context {
    client: Client,
    metrics: Arc<Metrics>,
    app_api_resource: ApiResource,
    /// TTL cache for the kubelet Summary sample behind `NodeDiskPressure`
    /// (2.22d / D8). Shared with the provisioner's own use of the same type,
    /// so a node's kubelet is hit at most once per TTL per controller rather
    /// than once per reconcile.
    capacity: operator_core::capacity::CapacityCache,
    /// Where published versions are read from: [`OciRegistry`] in
    /// production. See [`Upstream`].
    upstream: Arc<dyn Upstream>,
    /// Bumps that landed on the parent and are not yet in a status write
    /// (WI-400). See [`Context::note_bump`].
    unrecorded_bumps: std::sync::Mutex<Vec<PlatformStackVersionHistoryEntry>>,
}

impl Context {
    /// Remember a bump the moment the parent patch carrying it has landed,
    /// until a status write records it in `versionHistory`.
    ///
    /// WI-400: the history entry is decided from the LIVE parent — a pass
    /// appends it only when it is the one that moves `targetRevision`. A pass
    /// cut (or failed) between the bump and its status write lost the entry
    /// for good, because the next pass already finds the parent on the new
    /// version. Held here, it rides the next status write instead. In memory
    /// only: an operator restart in that window still loses it, as a crash
    /// there always has.
    fn note_bump(&self, entry: PlatformStackVersionHistoryEntry) {
        let mut bumps = self
            .unrecorded_bumps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bumps.push(entry);
        if bumps.len() > crate::status::VERSION_HISTORY_CAP {
            let drop = bumps.len() - crate::status::VERSION_HISTORY_CAP;
            bumps.drain(0..drop);
        }
    }

    /// The bumps not yet recorded, oldest first.
    fn unrecorded_bumps(&self) -> Vec<PlatformStackVersionHistoryEntry> {
        self.unrecorded_bumps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Drop `recorded` once a status write carrying them has succeeded.
    fn forget_bumps(&self, recorded: &[PlatformStackVersionHistoryEntry]) {
        self.unrecorded_bumps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|e| !recorded.contains(e));
    }
}

/// The two questions the reconcile asks the published chart repository.
///
/// A SEAM, and only one: production has exactly one implementation,
/// [`OciRegistry`]. It exists so a whole reconcile can be driven against an
/// upstream that never answers (WI-400). A real socket cannot stand in for
/// that once the registry client has its own connect and read bounds — they
/// fire first, so a test on a silent socket would pass with the reconcile's
/// whole-operation budget removed.
#[async_trait::async_trait]
trait Upstream: Send + Sync {
    /// The channel-latest; see [`resolve_channel_latest`].
    async fn channel_latest(
        &self,
        upstream: &str,
        channel: Channel,
        channel_label: &str,
    ) -> Result<(String, bool, Option<CompatibilityDoc>), Error>;

    /// The most destructive change class between two versions; see
    /// [`fetch_path_max_change_class`].
    async fn path_max_change_class(
        &self,
        upstream: &str,
        from_version: &str,
        to_version: &str,
    ) -> Result<ChangeClass, CompatError>;
}

/// The OCI registry named by `spec.source.upstream`.
struct OciRegistry;

#[async_trait::async_trait]
impl Upstream for OciRegistry {
    async fn channel_latest(
        &self,
        upstream: &str,
        channel: Channel,
        channel_label: &str,
    ) -> Result<(String, bool, Option<CompatibilityDoc>), Error> {
        resolve_channel_latest(upstream, channel, channel_label).await
    }

    async fn path_max_change_class(
        &self,
        upstream: &str,
        from_version: &str,
        to_version: &str,
    ) -> Result<ChangeClass, CompatError> {
        fetch_path_max_change_class(upstream, from_version, to_version).await
    }
}

/// The `kind` label this controller's metrics carry.
const KIND: &str = "PlatformStack";

/// How long one PlatformStack reconcile may run before it is abandoned
/// (WI-400). The controller watches a singleton, so a reconcile that never
/// returns stalls EVERY trigger it has: the stack, the parent Application and
/// the backup CronJobs, Jobs and runner pods all map to `PlatformStack/default`,
/// and kube-runtime holds them behind the running one (GOTCHA-51).
///
/// The legitimate worst case is bounded from the inside: the two registry
/// questions (`OCI_OPERATION_BUDGET`, 20s each), the backup reads
/// (`BACKUP_READ_BUDGET`, 10s), the node sample (`NODE_SAMPLE_BUDGET`, 10s),
/// four decorative calls (`DECORATIVE_CALL_BUDGET`, 5s each) and a
/// MigrationPlan create behind its admission webhook (10s) come to 90s, beside
/// about a dozen apiserver calls that take milliseconds. 120s leaves room and
/// stays under the client's 295s socket read timeout, so this — not a socket —
/// is the real bound.
///
/// What is left for it to cut is an apiserver call that does not answer, and
/// against such an apiserver no status write could land either. Every hang the
/// status CAN outlive — the registry, the backup reads, the node proxy, the
/// anchor and the Events — has its own bound and becomes the condition that
/// says so (`UpstreamReachable=False`, `BackupHealthy=Unknown`), so the
/// deadline never re-creates the v0.2.12 wedge of frozen conditions with a
/// stale `UpstreamReachable=True`.
///
/// A cut is reported on the stack itself: [`reconcile_with_deadline`] sets
/// `ReconcileStalled=True` under a field manager of its own (`crate::stall`),
/// which `apprafter platform status` shows, and the first reconcile that
/// finishes removes it. Every other condition keeps the value of the last
/// reconcile that finished. `error_policy` adds a Warning Event on the stack,
/// a WARN and `apprafter_reconcile_timeouts_total{kind="PlatformStack"}`, and
/// requeues the stack in 60s.
pub const RECONCILE_DEADLINE: Duration = Duration::from_secs(120);

/// One reconcile under [`RECONCILE_DEADLINE`], with the outcome kept in the
/// stack's `ReconcileStalled` condition (WI-400, `crate::stall`): set when
/// the pass is cut, removed after a pass that finishes. A pass that fails for
/// any other reason changes neither: it neither stalled nor proved the stall
/// over.
///
/// Both writes come after the pass, under their own budget
/// (`stall::STALL_WRITE_BUDGET`), so a pass holds its slot for at most
/// `RECONCILE_DEADLINE + STALL_WRITE_BUDGET`. The clear acts on the stack
/// this pass read: a `ReconcileStalled` that lands after the read is removed
/// by the next pass, which that write itself starts through the stack's
/// watch.
async fn reconcile_with_deadline(
    stack: Arc<PlatformStack>,
    ctx: Arc<Context>,
) -> Result<Action, Error> {
    let outcome =
        operator_core::deadline::within(RECONCILE_DEADLINE, reconcile(stack.clone(), ctx.clone()))
            .await;
    match &outcome {
        Err(Error::TimedOut(timed_out)) => {
            crate::stall::mark(&ctx.client, &stack, timed_out.after).await
        }
        Ok(_) => crate::stall::clear(&ctx.client, &stack).await,
        Err(_) => {}
    }
    outcome
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
        capacity: operator_core::capacity::CapacityCache::new(),
        upstream: Arc::new(OciRegistry),
        unrecorded_bumps: std::sync::Mutex::new(Vec::new()),
    });

    info!(
        parent_app = format!("{PARENT_APPLICATION_NAMESPACE}/{PARENT_APPLICATION_NAME}").as_str(),
        "PlatformController Controller::run() entering watch loop"
    );
    // `BackupHealthy` (WI-386) reads the backup CronJobs, their Jobs and the
    // runner pods. Every change to one of them reconciles the singleton, so
    // a pod the scheduler cannot place, a runner killed at its limit or a
    // Job that fails shows on the stack within seconds, not at the next
    // `checkInterval` (six hours by default). A pod still inside the grace,
    // or a Job still waiting for its first pod, has no event left to wake
    // the controller; the reconcile requeues for the end of the grace
    // instead (`backup_health::Assessment::recheck_after`).
    // RBAC: the operator chart's `-backup-health` Role, list + watch in
    // `apprafter-system` only.
    //
    // These watches share the controller's ONE trigger stream, and
    // kube-runtime 4.2 wraps that whole stream in a single `StreamBackoff`
    // (`Controller::run`): an error from any watch — a 403 while the Role is
    // not yet applied, or on a fork that trimmed it — backs off the
    // PlatformStack and Application triggers too, by up to about 30 s each
    // time. Reconciles still run on their requeues, and the reads in
    // `backup_health::observe` fail on their own, so the condition says
    // `StateUnreadable`. Isolating them would take a separate trigger stream
    // with its own backoff, which kube-runtime offers only behind its
    // `unstable-runtime-stream-control` feature; not taken for this.
    let backup_ns = crate::backup_health::BACKUP_NAMESPACE;
    let to_singleton =
        || Some(ObjectRef::<PlatformStack>::new(SINGLETON_NAME).within(SINGLETON_NAMESPACE));
    Controller::new(stacks, watcher::Config::default())
        .watches(
            Api::<CronJob>::namespaced(ctx.client.clone(), backup_ns),
            watcher::Config::default(),
            move |_: CronJob| to_singleton(),
        )
        .watches(
            Api::<Job>::namespaced(ctx.client.clone(), backup_ns),
            watcher::Config::default(),
            move |_: Job| to_singleton(),
        )
        .watches(
            Api::<Pod>::namespaced(ctx.client.clone(), backup_ns),
            watcher::Config::default().labels(crate::backup_health::RUNNER_POD_SELECTOR),
            move |_: Pod| to_singleton(),
        )
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
        .run(reconcile_with_deadline, error_policy, ctx)
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

/// Best-effort node-free fraction for the cluster's first node (2.22d / D8).
///
/// Single-node Tier 1 is the case this serves; on a larger cluster the first
/// node is a sample rather than a survey, which is honest for a warning and
/// would be wrong for anything that acted on it. `None` on any failure — the
/// caller leaves the condition untouched rather than asserting a false
/// negative, because "we could not look" and "there is space" are different
/// answers and only one of them is safe to print.
async fn sample_node_free_fraction(
    client: &Client,
    cache: &operator_core::capacity::CapacityCache,
) -> Option<f64> {
    let nodes = Api::<k8s_openapi::api::core::v1::Node>::all(client.clone())
        .list(&Default::default())
        .await
        .ok()?;
    let node = nodes.items.first()?.name_any();
    let summary = cache.summary_for_node(client, &node).await?;
    operator_core::capacity::node_free_fraction(&summary)
}

/// The `NodeDiskPressure` verdict for a node-free fraction: `(status, reason,
/// message)`.
///
/// PURE, AND EXTRACTED FOR THAT REASON. This is the entire user-facing surface
/// of D8 — the sentence an operator reads, and the only warning that a node is
/// about to stop accepting writes — and it was assembled inline inside a
/// thousand-line async reconcile where no test could reach it. The only thing
/// exercising the wording was `e2e/node-disk-pressure-hetzner.sh`, which costs
/// a provisioned Hetzner node and thirteen minutes, so a regression in the one
/// sentence users actually read was thirteen minutes and real money away from
/// being noticed.
///
/// The threshold itself lives in `operator_core::capacity` and is asserted
/// there; what this adds is that the right SIDE of it produces the right
/// condition, reason and message.
fn node_disk_pressure_verdict(fraction: f64) -> (&'static str, &'static str, String) {
    let pct_free = (fraction * 100.0).round() as i64;
    if operator_core::capacity::is_capacity_warning(
        fraction,
        operator_core::capacity::DEFAULT_NODE_FREE_THRESHOLD,
    ) {
        (
            "True",
            "NodeFilesystemNearlyFull",
            format!(
                "the node's filesystem is {}% full ({pct_free}% free). Every workload on \
                 this node shares it — local-path volumes, database storage, snapshots, \
                 container images and logs.",
                100 - pct_free
            ),
        )
    } else {
        (
            "False",
            "SufficientSpace",
            format!("the node's filesystem has {pct_free}% free"),
        )
    }
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
    // CLI-triggered force-recheck: the `apprafter.io/recheck-requested`
    // annotation carries an RFC3339 timestamp. When it parses and is
    // newer than `lastUpstreamCheck`, `should_poll_oci` bypasses the
    // 60s throttle so `apprafter platform status`/`update` never show
    // a stale `availableVersion`. Self-clearing via the
    // `lastUpstreamCheck` advance below — no annotation removal.
    let recheck_requested = stack
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(RECHECK_REQUESTED_ANNOTATION))
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc));
    let should_poll_oci = should_poll_oci(
        prior_last_check,
        prior_available.as_deref(),
        recheck_requested,
        now,
    );
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
        let resolved = match within_oci_budget(
            "resolving the channel-latest",
            ctx.upstream
                .channel_latest(&spec.source.upstream, channel, &spec.channel),
        )
        .await
        {
            Ok(answer) => answer,
            Err(timed_out) => Err(Error::from(timed_out)),
        };
        match resolved {
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

    // `BackupHealthy` (WI-386), BEFORE the in-flight early return: a
    // platform upgrade in progress is exactly when a node is short of room,
    // and the condition must not freeze for its duration. Every write below
    // sends the WHOLE status — this condition included — under the one field
    // manager, so it is never pruned by a write that forgot it.
    let last_prune = stack
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(LAST_PRUNE_ANNOTATION))
        .map(String::as_str);
    let backup_recheck =
        assess_backup_health(&ctx, spec, last_prune, &prior_conds, now, &mut new_status).await;

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
        return Ok(Action::requeue(sooner(IN_FLIGHT_REQUEUE, backup_recheck)));
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
    // The `completed` plan whose approval this pass's bump rides on. The GC
    // below must keep it until a pass sees the parent on the new version:
    // deleted before the bump lands, a cut or failed pass loses the approval
    // for good (the next pass finds no plan and gates the transition anew).
    let mut authorising_plan: Option<String> = None;
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
                authorising_plan = Some(plan_name.clone());
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
            let classified = match within_oci_budget(
                "classifying the transition",
                ctx.upstream.path_max_change_class(
                    &spec.source.upstream,
                    &current_target,
                    &desired.target_revision,
                ),
            )
            .await
            {
                Ok(answer) => answer.map_err(|e| e.to_string()),
                Err(timed_out) => Err(timed_out.to_string()),
            };
            match classified {
                Err(e) => {
                    warn!(
                        error = %e,
                        from = %current_target,
                        to = %desired.target_revision,
                        "cannot classify transition (upstream unreachable); holding current target"
                    );
                    upstream_poll_error.get_or_insert(e);
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
                    // tree). BEST-EFFORT by design: the anchor is an
                    // OPTIONAL Argo-tree nicety, so its absence
                    // (`Ok(None)` — older chart) OR an error reading
                    // it (`Err` — e.g. a `configmaps` RBAC gap) must
                    // yield an un-owned (off-tree, still
                    // CLI-approvable) plan and NEVER abort the
                    // reconcile. Walk-found v0.2.26 freeze: a 403 on
                    // this GET used to propagate via `?` and kill the
                    // reconcile BEFORE the status write, so
                    // `availableVersion` froze and the cluster stopped
                    // detecting upgrades — it could not even create
                    // its own gate (GitOps deadlock). `anchor_uid_from_get`
                    // swallows both None and Err to None.
                    let anchor_api: Api<ConfigMap> =
                        Api::namespaced(ctx.client.clone(), MIGRATION_PLAN_NAMESPACE);
                    let anchor_uid = match tokio::time::timeout(
                        DECORATIVE_CALL_BUDGET,
                        anchor_api.get_opt(PLATFORM_MIGRATION_ANCHOR),
                    )
                    .await
                    {
                        Ok(anchor_get) => {
                            match &anchor_get {
                                Ok(Some(_)) => debug!(
                                    anchor = PLATFORM_MIGRATION_ANCHOR,
                                    "anchoring MigrationPlan to ConfigMap ownerRef"
                                ),
                                Ok(None) => warn!(
                                    anchor = PLATFORM_MIGRATION_ANCHOR,
                                    namespace = MIGRATION_PLAN_NAMESPACE,
                                    "anchor ConfigMap absent — creating MigrationPlan un-owned (off the Argo tree)"
                                ),
                                Err(e) => warn!(
                                    anchor = PLATFORM_MIGRATION_ANCHOR,
                                    namespace = MIGRATION_PLAN_NAMESPACE,
                                    error = %e,
                                    "anchor ConfigMap lookup failed (e.g. configmaps RBAC) — creating MigrationPlan un-owned; detection/status NOT blocked"
                                ),
                            }
                            anchor_uid_from_get(anchor_get)
                        }
                        Err(_) => {
                            warn!(
                                anchor = PLATFORM_MIGRATION_ANCHOR,
                                namespace = MIGRATION_PLAN_NAMESPACE,
                                budget_secs = DECORATIVE_CALL_BUDGET.as_secs(),
                                "anchor ConfigMap lookup did not answer in time — creating MigrationPlan un-owned; detection/status NOT blocked"
                            );
                            None
                        }
                    };
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

    // GC platform MigrationPlans EVERY reconcile so the Argo tree shows
    // at most ONE active gate. Keep only the current gate
    // (`migration_pending`) + any mid-rollout plan (approved/executing);
    // delete everything else — stale pending plans from a target that
    // advanced (walk-found: a yank moved 0.2.27→0.2.28, orphaning 26-27
    // beside 26-28) AND terminal completed/rejected/failed records that
    // otherwise pile up one-per-upgrade (walk-found: 24-25 + 25-26 +
    // 26-28 all lingered). Runs unconditionally — not only inside the
    // gating branch — so the last completed plan is dropped once the
    // upgrade settles. Best-effort: a failure is logged, never fatal.
    //
    // The two names it keeps never coexist: `migration_pending` is the gate
    // of a held transition, `authorising_plan` the completed plan of the one
    // being bumped this pass (WI-400). The latter is collected by the first
    // pass that finds the parent already on the new version.
    {
        let plan_api: Api<MigrationPlan> =
            Api::namespaced(ctx.client.clone(), MIGRATION_PLAN_NAMESPACE);
        let keep = migration_pending
            .as_ref()
            .and_then(|m| m.plan_name.as_deref())
            .or(authorising_plan.as_deref())
            .unwrap_or("");
        match plan_api.list(&ListParams::default()).await {
            Ok(list) => {
                for stale in superseded_platform_plan_names(&list.items, keep) {
                    info!(plan = %stale, keep = %keep, "GC platform MigrationPlan");
                    if let Err(e) = plan_api.delete(&stale, &DeleteParams::default()).await {
                        warn!(plan = %stale, error = %e, "failed to GC platform MigrationPlan (continuing)");
                    }
                }
            }
            Err(e) => warn!(error = %e, "failed to list MigrationPlans for GC (continuing)"),
        }
    }

    // ADR 0048 (revised — kind+Argo-validated): mirror the pending
    // surface onto the chart-managed `platform-migration-anchor`
    // ConfigMap. The root App's OWN tile health is the worst-of
    // aggregate of its managed `.status.resources`, and the anchor IS
    // one of them — so a `ConfigMap` health customization that returns
    // Suspended for the stamped anchor rolls the root App tile to
    // Suspended (the argoproj.io_Application customization on the root
    // App, by contrast, never affects the top-level app's own tile).
    // SSA with our own field manager survives Argo syncs + causes no
    // OutOfSync. Best-effort: the tile signal is a nicety, never fatal.
    match tokio::time::timeout(
        DECORATIVE_CALL_BUDGET,
        reconcile_anchor_health(&ctx, &pending_upgrade),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!(
            error = %e,
            "failed to reconcile anchor ConfigMap pending-upgrade annotation (continuing)"
        ),
        Err(_) => warn!(
            budget_secs = DECORATIVE_CALL_BUDGET.as_secs(),
            "anchor ConfigMap pending-upgrade annotation did not answer in time (continuing)"
        ),
    }

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
    // ADR 0048 (walk-found 2026-06-12): the pending-upgrade
    // annotation surface must reconcile INDEPENDENTLY of the source
    // patch. A gated upgrade HOLDS `spec.source.targetRevision`
    // unchanged, so without this term `patched_this_cycle` is false
    // in exactly the state the banner is meant for — the root-App
    // `apprafter.io/upgrade-*` annotations never get stamped and the
    // root approval banner never appears (the MigrationPlan node
    // still works; the root App just stays green). Driving the patch
    // off the annotation diff too fixes the SET path (gated) and
    // hardens the CLEAR path (post-approval) — the source re-apply it
    // rides is idempotent (held target) so it adds no real churn.
    let annotations_changed = pending_upgrade_annotations_differ(&parent_json, &pending_upgrade);
    let patched_this_cycle = target_changed
        || values_changed
        || !platform_controller_owns_source(&parent_json)
        || annotations_changed;
    if patched_this_cycle || foreign_writer.is_some() {
        if let Some(foreign) = &foreign_writer {
            warn!(manager = %foreign, "foreign field manager on parent spec.source; force-reverting");
            // Walk-fix #6 v0.1.119 → v0.1.120: emit a
            // Kubernetes Event pair so the foreign-write +
            // revert leaves a durable audit trace visible via
            // `kubectl describe platformstack default` (and
            // `kubectl get events`). Without it, a transient
            // revert vanishes from the
            // `UnauthorizedSourceModification` condition within
            // one reconcile cycle and operators staring at
            // `kubectl get platformstack` see only the
            // post-recovery `False/Clean` state — no record
            // that a foreign write happened.
            //
            // This Warning is the DETECTION record, and it goes
            // out BEFORE the revert on purpose (WI-400). If the
            // revert lands but its answer is lost — the pass
            // cut at `RECONCILE_DEADLINE`, or the operator
            // restarted — the force apply has already taken
            // the fields from the foreign manager: the next
            // pass sees no foreign writer, publishes nothing,
            // and the condition never goes True. Published
            // after the patch, such a write would leave no
            // trace at all. So the note says only what is true
            // NOW — the revert is in progress — and the
            // completion is `SourceReverted` below, published
            // once the patch has landed. A revert whose answer
            // is lost therefore shows the detection without the
            // completion: it under-claims, never over-claims.
            //
            // Best-effort and bounded: a failed or unanswered
            // publish is logged, never fatal. The force-revert
            // SSA patch is the load-bearing action.
            let recorder = build_recorder(&ctx, &stack);
            let ev = KubeEvent {
                type_: EventType::Warning,
                reason: "ForeignFieldManager".into(),
                note: Some(format!(
                    "detected external write to spec.source on parent Application \
                     {PARENT_APPLICATION_NAMESPACE}/{PARENT_APPLICATION_NAME} by field manager \
                     {foreign:?}; PlatformController is force-reapplying desired state \
                     (target={target_for_patch})"
                )),
                action: "ForceRevert".into(),
                secondary: Some(parent_object_reference()),
            };
            publish_bounded(&recorder, ev).await;
        }
        patch_application(&apps, &patch_payload, &pending_upgrade).await?;
        if target_for_patch != current_target {
            ctx.note_bump(PlatformStackVersionHistoryEntry {
                version: target_for_patch.clone(),
                applied_at: now.to_rfc3339(),
                outcome: "succeeded".into(),
            });
        }
        if foreign_writer.is_some() {
            // The COMPLETION record: reached only once the
            // revert has landed (`?` above returns on a failed
            // patch, and a cut pass never gets here).
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
            publish_bounded(&recorder, ev).await;
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

    // `NodeDiskPressure` (2.22d / D8). The node's root filesystem carries
    // every local-path PVC, the CNPG data directory, Dragonfly's snapshots,
    // the image store and the logs, so it filling up stops far more than
    // volumes. The signal itself is not new — it was computed inside the
    // SharedVolume reconcile and stamped there, so a cluster with no
    // SharedVolume was never warned about its own disk.
    //
    // BEST-EFFORT, like the sampler it calls: an unreachable kubelet, a
    // missing node or a parse failure leaves the condition untouched rather
    // than flipping it to a false negative. A decorative read must never
    // fail a reconcile — the ADR 0048 anchor-403 lesson.
    let fraction = match tokio::time::timeout(
        NODE_SAMPLE_BUDGET,
        sample_node_free_fraction(&ctx.client, &ctx.capacity),
    )
    .await
    {
        Ok(fraction) => fraction,
        Err(_) => {
            warn!(
                budget_secs = NODE_SAMPLE_BUDGET.as_secs(),
                "node disk sample did not answer in time; NodeDiskPressure left as it was"
            );
            None
        }
    };
    if let Some(fraction) = fraction {
        let (status, reason, message) = node_disk_pressure_verdict(fraction);
        let cond = condition(
            COND_NODE_DISK_PRESSURE,
            status,
            reason,
            &message,
            &prior_conds,
        );
        upsert_condition(&mut new_status, cond);
    }

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
    // This pass's bump (noted when its patch landed) and any an earlier pass
    // landed but was cut before writing (WI-400), oldest first. An entry
    // already in the status is not appended twice.
    let unrecorded = ctx.unrecorded_bumps();
    for entry in &unrecorded {
        let recorded = new_status
            .version_history
            .as_ref()
            .is_some_and(|h| h.contains(entry));
        if !recorded {
            append_version_history(&mut new_status, entry.clone());
        }
    }

    new_status.current_version = Some(target_for_patch.clone());
    new_status.target_version = Some(target_for_patch);
    info!(
        include_version_history = appended_history,
        unrecorded_bumps = unrecorded.len(),
        new_history_len = new_status.version_history.as_ref().map_or(0, |v| v.len()),
        "PlatformController writing status"
    );
    write_status_if_changed(&stack, &ctx, new_status, appended_history).await?;
    ctx.forget_bumps(&unrecorded);
    Ok(Action::requeue(sooner(
        parse_check_interval(&spec.source.check_interval),
        backup_recheck,
    )))
}

/// The annotation `apprafter backup prune` stamps on the stack when it has
/// pruned from outside the cluster. `BackupRetention` names it.
const LAST_PRUNE_ANNOTATION: &str = "apprafter.io/last-prune";

/// Read the backup objects and put the `BackupHealthy` and `BackupRetention`
/// verdicts on `new_status`. Returns when to look again because a runner
/// pod's grace ends (see `backup_health`).
///
/// Never fails the reconcile. A read that fails is itself the verdict
/// (`Unknown`, `StateUnreadable` / `RecordUnreadable`), rather than a stale
/// `True` left in place or a reconcile error that would freeze every other
/// condition too — the ADR 0048 anchor-403 lesson. Nothing is read while
/// backups are disabled.
async fn assess_backup_health(
    ctx: &Context,
    spec: &operator_core::PlatformStackSpec,
    last_prune: Option<&str>,
    prior_conds: &[PlatformStackCondition],
    now: DateTime<Utc>,
    new_status: &mut PlatformStackStatus,
) -> Option<Duration> {
    let enabled = spec.backup.as_ref().is_some_and(|b| b.enabled);
    let observed = if enabled {
        match tokio::time::timeout(
            BACKUP_READ_BUDGET,
            crate::backup_health::observe(&ctx.client),
        )
        .await
        {
            Ok(observed) => observed,
            Err(_) => Err(format!(
                "the apiserver did not answer within {}s",
                BACKUP_READ_BUDGET.as_secs()
            )),
        }
    } else {
        // Disabled: `assess` returns `Absent` without looking at this.
        Ok(crate::backup_health::Observed::default())
    };
    if let Err(e) = &observed {
        warn!(error = %e, "could not read the backup objects; BackupHealthy=Unknown");
    }
    let assessment = crate::backup_health::assess(
        enabled,
        observed.as_ref().map_err(String::as_str),
        prior_conds,
        now,
    );
    crate::backup_health::apply(
        new_status,
        crate::status::COND_BACKUP_HEALTHY,
        &assessment.verdict,
        prior_conds,
    );
    // Retention, from the same reads, in the same write.
    let retention = crate::backup_retention::assess(
        spec.backup.as_ref(),
        last_prune,
        observed.as_ref().map_err(String::as_str),
    );
    crate::backup_health::apply(
        new_status,
        crate::status::COND_BACKUP_RETENTION,
        &retention,
        prior_conds,
    );
    assessment.recheck_after
}

/// The earlier of a reconcile's own requeue and a backup recheck.
fn sooner(requeue: Duration, recheck: Option<Duration>) -> Duration {
    recheck.map_or(requeue, |r| requeue.min(r))
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

/// Best-effort extraction of the anchor ConfigMap's UID for the
/// ADR 0048 MigrationPlan ownerRef. The anchor is an OPTIONAL
/// Argo-tree nicety, so BOTH its absence (`Ok(None)` — older chart
/// without the anchor) AND an error reading it (`Err` — e.g. the
/// `configmaps` RBAC gap that caused the walk-found v0.2.26 reconcile
/// freeze) collapse to `None`: the plan is created un-owned (off the
/// Argo tree, still CLI-approvable) rather than propagating and
/// aborting the reconcile before the `status.availableVersion` write.
fn anchor_uid_from_get(get: Result<Option<ConfigMap>, kube::Error>) -> Option<String> {
    get.ok().flatten().and_then(|cm| cm.metadata.uid)
}

/// Names of platform MigrationPlans to garbage-collect, keeping at
/// most ONE active gate. Collect every PLATFORM-scope plan that is
/// NOT the current gate's `keep` name AND is NOT mid-rollout
/// (`approved`/`executing`). That deletes both (a) stale
/// `pending-approval` plans left when the channel-latest target
/// advances — the name is keyed on `(from → to)`, so an advance mints
/// a new name and orphans the old (walk-found: 26-27 lingered beside
/// 26-28) — and (b) terminal `completed`/`rejected`/`failed` records
/// that otherwise pile up one per upgrade (walk-found: 24-25, 25-26,
/// 26-28 all lingered). A mid-rollout plan (approved → executing) is
/// preserved so GC never interrupts an in-flight upgrade; app-scope
/// plans are never collected here. With `keep == ""` (no pending gate
/// — settled) every non-mid-rollout platform plan is collected, so the
/// last completed plan is dropped once the upgrade settles.
fn superseded_platform_plan_names(plans: &[MigrationPlan], keep: &str) -> Vec<String> {
    plans
        .iter()
        .filter(|p| p.spec.scope.type_ == "platform")
        .filter_map(|p| {
            let name = p.metadata.name.as_deref()?;
            if name == keep {
                return None;
            }
            let phase = p
                .status
                .as_ref()
                .and_then(|s| s.phase.as_deref())
                .unwrap_or("pending-approval");
            (!matches!(phase, "approved" | "executing")).then(|| name.to_string())
        })
        .collect()
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

/// Decide whether to query the OCI upstream this cycle.
///
/// Steady state: a poll is throttled to once per
/// `MIN_OCI_POLL_INTERVAL_SECS` (60s) — a watch-event burst must
/// not loop the controller re-polling + re-stamping
/// `lastUpstreamCheck`. The throttle applies only once we have a
/// prior poll AND a cached `availableVersion`; the first reconcile
/// (or any state with no cached version) always polls.
///
/// Override: a CLI-triggered force-recheck bypasses the throttle.
/// The CLI stamps `apprafter.io/recheck-requested` with an RFC3339
/// timestamp; when that is NEWER than the last poll
/// (`recheck_requested > prior_last_check`, or `prior_last_check`
/// is None) the request is unserviced, so we poll NOW regardless
/// of the 60s window. This keeps `apprafter platform status`/
/// `update` from showing a stale `availableVersion` for up to
/// `spec.source.checkInterval` (default 6h). Self-clearing: the
/// poll advances `lastUpstreamCheck` past the request timestamp,
/// so the next reconcile sees the request as already serviced and
/// re-throttles — no annotation removal required.
fn should_poll_oci(
    prior_last_check: Option<DateTime<Utc>>,
    prior_available: Option<&str>,
    recheck_requested: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    // Force-recheck: a recheck requested after (or with no) prior
    // poll bypasses the throttle entirely.
    let force_recheck = match recheck_requested {
        Some(req) => prior_last_check.is_none_or(|last| req > last),
        None => false,
    };
    if force_recheck {
        return true;
    }
    // Steady-state throttle.
    match (prior_last_check, prior_available) {
        (Some(t), Some(_)) => (now - t).num_seconds() >= MIN_OCI_POLL_INTERVAL_SECS,
        _ => true,
    }
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

/// True when the root Application's current `apprafter.io/upgrade-*`
/// annotation surface (ADR 0048) does not already match the desired
/// pending-upgrade state — so the SSA patch must run to converge it.
///
/// Walk-found 2026-06-12: the annotation patch used to ride ONLY the
/// `spec.source` patch (`patched_this_cycle`), but a gated upgrade
/// holds the source unchanged — so in exactly the state the approval
/// banner is for, the patch never fired and the root App stayed green
/// (annotations never stamped). Pure over the parent JSON + desired
/// state so the SET (pending → stamp) and CLEAR (approved → prune)
/// transitions are both unit-tested.
fn pending_upgrade_annotations_differ(
    parent_json: &Value,
    pending: &Option<PendingUpgrade>,
) -> bool {
    let ann = parent_json
        .get("metadata")
        .and_then(|m| m.get("annotations"));
    let get = |k: &str| ann.and_then(|a| a.get(k)).and_then(Value::as_str);
    match pending {
        Some(up) => {
            get("apprafter.io/upgrade-pending") != Some("true")
                || get("apprafter.io/upgrade-from") != Some(up.from.as_str())
                || get("apprafter.io/upgrade-to") != Some(up.to.as_str())
                || get("apprafter.io/upgrade-class") != Some(up.class.as_str())
                || get("apprafter.io/upgrade-plan") != Some(up.plan.as_str())
        }
        // No gate: the banner must be absent — patch (to prune) only
        // if a stale `upgrade-pending` is still stamped.
        None => get("apprafter.io/upgrade-pending").is_some(),
    }
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

/// Build the SSA patch for the `platform-migration-anchor` ConfigMap's
/// pending-upgrade annotations (ADR 0048 revised). Mirrors
/// `build_application_patch`'s metadata stanza but on the ConfigMap:
/// the chart-managed anchor is one of the root App's managed
/// `.status.resources`, so a `ConfigMap` health customization reading
/// these annotations rolls the root App's TILE to Suspended
/// (kind+Argo-validated — the root App's OWN annotations never do, as
/// the `argoproj.io_Application` health customization never applies to
/// a top-level app's own tile). Annotation-ONLY body (no `data`) so SSA
/// owns just the annotations and never fights Argo over the ConfigMap's
/// content. `pending == None` omits annotations → SSA prunes them → the
/// tile returns to Healthy when the gate releases.
fn build_anchor_health_patch(pending: &Option<PendingUpgrade>) -> Value {
    let mut metadata = json!({ "name": PLATFORM_MIGRATION_ANCHOR });
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
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": metadata,
    })
}

/// SSA-stamp (or prune) the pending-upgrade annotations on the
/// chart-managed anchor ConfigMap so the root App's Argo TILE reflects
/// the gate (see [`build_anchor_health_patch`]). A missing anchor
/// (older chart / pre-first-sync) is a no-op — an SSA apply would
/// otherwise CREATE a bare ConfigMap the chart must own. Same field
/// manager as the root-App patch (a different object, so no
/// field-ownership overlap); validated live to survive Argo syncs
/// without OutOfSync.
async fn reconcile_anchor_health(
    ctx: &Context,
    pending: &Option<PendingUpgrade>,
) -> Result<(), Error> {
    let api: Api<ConfigMap> = Api::namespaced(ctx.client.clone(), MIGRATION_PLAN_NAMESPACE);
    if api.get_opt(PLATFORM_MIGRATION_ANCHOR).await?.is_none() {
        return Ok(());
    }
    let payload = build_anchor_health_patch(pending);
    let params = PatchParams::apply(FIELD_MANAGER).force();
    api.patch(PLATFORM_MIGRATION_ANCHOR, &params, &Patch::Apply(&payload))
        .await?;
    Ok(())
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
    // Compared as `platform-controller` applies it (WI-400): the stall
    // manager's `ReconcileStalled` alone is no reason to write; a duplicate
    // condition type read back is, once, so the write drops it; and so is an
    // ownership of `status.conditions` that only this manager's own apply
    // puts right (`stall::controller_must_reapply`).
    let prior_view = without_reconcile_stalled(&prior);
    let unchanged = prior_view == platform_controller_view(&new_status);
    if unchanged && !crate::stall::controller_must_reapply(stack) {
        return Ok(());
    }
    // A list this manager owns WHOLE (last written under the atomic CRD), or
    // one carrying a condition nobody holds that this write leaves out, is
    // first re-applied as it was read: that apply takes every condition by
    // key. Applied straight away, the new status would NOT remove a
    // condition it leaves out (a `BackupHealthy` that backups being turned
    // off retire): it would stay, owned by nobody, and nothing would ever
    // remove it (`stall::controller_must_adopt_first`).
    if crate::stall::controller_must_adopt_first(stack, &new_status) {
        write_status(stack, ctx, prior_view, false).await?;
        if unchanged {
            return Ok(());
        }
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
    //
    // Serialized through `platform_controller_view` (WI-400): never
    // `ReconcileStalled`, which has its own field manager, and never a
    // second condition of one `type`, which the list-map apply refuses.
    let mut status_value = serde_json::to_value(platform_controller_view(new_status))
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
        // `saturating_mul`, not `*`: the value is user data from
        // `spec.checkInterval`, and a large number times 3600 overflows.
        b's' => value,
        b'm' => value.saturating_mul(60),
        b'h' => value.saturating_mul(3600),
        _ => return DEFAULT_REQUEUE,
    };
    // CLAMPED, and it is a crash guard rather than a tuning knob. kube-runtime
    // schedules through a tokio `DelayQueue`, which PANICS on a deadline it
    // cannot represent (`invalid deadline; err=Invalid`) — and that panic takes
    // the whole operator process down, every controller with it. So a single
    // mistyped `checkInterval: "999999h"` would crash-loop the platform
    // controller for every cluster that synced it. The same hazard, found in
    // the RetainedClaim GC by the 2.22 battery, is fixed there in the same
    // commit. A day is far beyond any useful poll cadence.
    Duration::from_secs(secs.min(MAX_CHECK_INTERVAL_SECS))
}

/// Ceiling on a parsed `checkInterval`. See `parse_check_interval` for why
/// this exists: an unbounded, data-derived requeue is an operator-wide panic,
/// not a slow poll.
const MAX_CHECK_INTERVAL_SECS: u64 = 86_400;

fn error_policy(stack: Arc<PlatformStack>, err: &Error, ctx: Arc<Context>) -> Action {
    ctx.metrics
        .reconcile_errors
        .with_label_values(&[KIND])
        .inc();
    match err {
        Error::TimedOut(timed_out) => {
            ctx.metrics
                .reconcile_timeouts
                .with_label_values(&[KIND])
                .inc();
            warn!(
                error = %err,
                deadline_secs = timed_out.after.as_secs(),
                "PlatformController reconcile abandoned at its deadline; PlatformStack/default \
                 keeps the status of the last reconcile that finished"
            );
            publish_deadline_event(&ctx, &stack, *timed_out);
        }
        _ => warn!(error = %err, "PlatformController reconcile failed"),
    }
    Action::requeue(Duration::from_secs(60))
}

/// Report a reconcile cut at `RECONCILE_DEADLINE` as a Warning Event on
/// `PlatformStack/default` (`kubectl describe platformstack default`).
///
/// Beside `ReconcileStalled`, which [`reconcile_with_deadline`] sets on the
/// status: the condition says the stack is stalled NOW and goes when a pass
/// finishes, while the Event stays in `kubectl get events` as the record of
/// each cut. It is the only report of a cut whose condition could not be
/// written, or must not be: while a rollback serves the atomic CRD
/// (`crate::stall`). `error_policy` is synchronous, so the publish runs on a
/// task of its own, bounded like every other Event here.
fn publish_deadline_event(
    ctx: &Context,
    stack: &PlatformStack,
    timed_out: operator_core::deadline::ReconcileTimedOut,
) {
    let recorder = build_recorder(ctx, stack);
    let ev = KubeEvent {
        type_: EventType::Warning,
        reason: "ReconcileTimedOut".into(),
        note: Some(format!(
            "the reconcile did not finish within {}s and was abandoned; the status shown is \
             from the last reconcile that finished, and the next attempt starts within 60s",
            timed_out.after.as_secs()
        )),
        action: "Reconcile".into(),
        secondary: None,
    };
    tokio::spawn(async move { publish_bounded(&recorder, ev).await });
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
/// `MigrationPlan`. These annotations are the LOAD-BEARING source
/// of the root-level "platform update pending" signal: the
/// `argoproj.io_Application` health customization reads them to
/// raise the banner on the root App tile. (There is no health
/// aggregation from the anchored MigrationPlan — Argo CD computes
/// an App's health from its MANAGED resource set, not from the
/// anchored ownerRef child, so the banner is the only root-level
/// cue.) `platform-controller` is the sole owner of these keys, so
/// emitting the patch body WITHOUT them (the `&None` arm of
/// `build_application_patch`) makes SSA prune any previously-
/// stamped set — the approval banner clears the moment the gate
/// releases (operator approves the plan, or the transition turns
/// out non-destructive).
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
            sourcecredential: None,
        },
        trigger: MigrationTrigger {
            type_: "platform-classification".into(),
            field: "spec.pin".into(),
            from: Some(Value::String(from_version.to_string())),
            to: Some(Value::String(to_version.to_string())),
            // 2.16b S-4 is app-scope: an app approval must not transfer
            // across a different spec edit. Platform-scope plans revert
            // via `previousSpecSnapshot` (a separate, version-anchored
            // mechanism) and are not consumed by the Application
            // reconciler's hash gate, so no content hash is stamped here.
            approved_spec_hash: None,
        },
        risks: Some(MigrationRisks {
            classification,
            // 2.16b S1.2 is an app-scope rollup (multiple destructive ops in a
            // single Application spec edit). A platform-classification plan
            // carries exactly one classification, so the distinct-list is left
            // unset (the primary `classification` above is authoritative).
            classifications: None,
            estimated_downtime: None,
            data_volume: None,
            reversible: None,
            requires_full_backup: None,
        }),
        // 2.16b S1.2 rollup is app-scope only; platform plans have a single
        // classification with no candidate set to roll up.
        changes: None,
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
    // -----------------------------------------------------------------
    // D8 — the sentence an operator actually reads
    // -----------------------------------------------------------------

    #[test]
    fn a_nearly_full_node_warns_and_names_what_shares_the_disk() {
        // 10% free is under the 0.15 threshold — the state the hardware walk
        // produces by filling the node.
        let (status, reason, message) = super::node_disk_pressure_verdict(0.10);
        assert_eq!(status, "True");
        assert_eq!(reason, "NodeFilesystemNearlyFull");
        assert!(message.contains("90% full"), "states fullness: {message}");
        assert!(
            message.contains("10% free"),
            "states the remainder: {message}"
        );
        // The message exists to tell the reader the blast radius is not one
        // volume. Without this half it is just a number.
        assert!(
            message.contains("local-path volumes") && message.contains("database storage"),
            "names what else shares the disk: {message}"
        );
    }

    #[test]
    fn a_healthy_node_reports_false_without_the_alarming_sentence() {
        let (status, reason, message) = super::node_disk_pressure_verdict(0.84);
        assert_eq!(status, "False");
        assert_eq!(reason, "SufficientSpace");
        assert!(message.contains("84% free"), "states the figure: {message}");
        // A healthy node must not carry the warning text. The CLI prints the
        // message only when status is True, but a condition that always reads
        // alarming is how a banner becomes noise.
        assert!(
            !message.contains("Every workload"),
            "no warning prose on a healthy node: {message}"
        );
    }

    #[test]
    fn the_threshold_boundary_falls_on_the_documented_side() {
        // `is_capacity_warning` is `fraction < threshold`, so exactly at the
        // threshold is NOT a warning. Pinned here because the boundary is the
        // part a refactor gets wrong, and because 0.15 is a bare constant with
        // no type to protect it.
        assert_eq!(super::node_disk_pressure_verdict(0.15).0, "False");
        assert_eq!(super::node_disk_pressure_verdict(0.1499).0, "True");
    }

    // ---- D23: the same hazard on the platform side ----

    #[test]
    fn an_absurd_check_interval_is_clamped_not_scheduled() {
        // One mistyped `checkInterval` would otherwise crash-loop the platform
        // controller on every cluster that synced it.
        assert_eq!(
            parse_check_interval("999999h"),
            Duration::from_secs(MAX_CHECK_INTERVAL_SECS)
        );
        // And the multiplication itself must not overflow on the way there.
        assert_eq!(
            parse_check_interval("99999999999999999999h"),
            DEFAULT_REQUEUE,
            "an unparseable u64 falls back to the default"
        );
        assert_eq!(
            parse_check_interval(&format!("{}h", u64::MAX)),
            Duration::from_secs(MAX_CHECK_INTERVAL_SECS)
        );
    }

    #[test]
    fn an_ordinary_check_interval_is_untouched() {
        assert_eq!(parse_check_interval("30s"), Duration::from_secs(30));
        assert_eq!(parse_check_interval("15m"), Duration::from_secs(900));
        assert_eq!(parse_check_interval("6h"), Duration::from_secs(21_600));
    }
    use super::*;

    #[test]
    fn parses_check_interval_with_h_m_s_units() {
        assert_eq!(parse_check_interval("6h"), Duration::from_secs(6 * 3600));
        assert_eq!(parse_check_interval("30m"), Duration::from_secs(30 * 60));
        assert_eq!(parse_check_interval("3600s"), Duration::from_secs(3600));
    }

    // --- should_poll_oci decision (CLI force-recheck) ---

    #[test]
    fn should_poll_oci_throttled_within_window_no_recheck() {
        // Fresh poll 10s ago, cached version, no recheck request →
        // throttled (false).
        let now = Utc::now();
        let last = now - chrono::Duration::seconds(10);
        assert!(!should_poll_oci(Some(last), Some("0.2.20"), None, now));
    }

    #[test]
    fn should_poll_oci_force_recheck_newer_than_last_check_bypasses_throttle() {
        // Within the 60s window, but a recheck was requested AFTER
        // the last poll → poll now (true), bypassing the throttle.
        let now = Utc::now();
        let last = now - chrono::Duration::seconds(10);
        let recheck = now - chrono::Duration::seconds(5); // after `last`
        assert!(should_poll_oci(
            Some(last),
            Some("0.2.20"),
            Some(recheck),
            now
        ));
    }

    #[test]
    fn should_poll_oci_force_recheck_older_than_last_check_is_serviced() {
        // Within the 60s window and the recheck request predates the
        // last poll (already serviced — self-cleared) → throttled
        // (false).
        let now = Utc::now();
        let last = now - chrono::Duration::seconds(10);
        let recheck = now - chrono::Duration::seconds(30); // before `last`
        assert!(!should_poll_oci(
            Some(last),
            Some("0.2.20"),
            Some(recheck),
            now
        ));
    }

    #[test]
    fn should_poll_oci_no_prior_check_always_polls() {
        // First reconcile: no prior poll → always poll, regardless
        // of recheck presence.
        let now = Utc::now();
        assert!(should_poll_oci(None, None, None, now));
        // A recheck with no prior poll is also unserviced → poll.
        assert!(should_poll_oci(None, None, Some(now), now));
    }

    #[test]
    fn should_poll_oci_elapsed_beyond_window_polls() {
        // >60s since last poll → poll (existing throttle expiry),
        // no recheck needed.
        let now = Utc::now();
        let last = now - chrono::Duration::seconds(120);
        assert!(should_poll_oci(Some(last), Some("0.2.20"), None, now));
    }

    #[test]
    fn should_poll_oci_no_cached_version_polls_even_within_window() {
        // Throttle only applies once a version is cached; a prior
        // check with no cached availableVersion still polls.
        let now = Utc::now();
        let last = now - chrono::Duration::seconds(10);
        assert!(should_poll_oci(Some(last), None, None, now));
    }

    #[test]
    fn parses_check_interval_defaults_on_garbage() {
        assert_eq!(parse_check_interval(""), DEFAULT_REQUEUE);
        assert_eq!(parse_check_interval("abc"), DEFAULT_REQUEUE);
        assert_eq!(parse_check_interval("10x"), DEFAULT_REQUEUE);
    }

    // --- ADR 0048 anchor ownerRef is best-effort (walk-found v0.2.26
    // freeze: a `configmaps` RBAC 403 on the anchor GET used to abort
    // the reconcile before the status write → availableVersion froze). ---
    #[test]
    fn anchor_uid_from_get_present_yields_uid() {
        let mut cm = ConfigMap::default();
        cm.metadata.uid = Some("uid-123".to_string());
        assert_eq!(
            anchor_uid_from_get(Ok(Some(cm))),
            Some("uid-123".to_string())
        );
    }

    #[test]
    fn anchor_uid_from_get_absent_yields_none() {
        // Ok(None) — older chart without the anchor: un-owned plan.
        assert_eq!(anchor_uid_from_get(Ok(None)), None);
        // Present but no uid set is also None (defensive).
        assert_eq!(anchor_uid_from_get(Ok(Some(ConfigMap::default()))), None);
    }

    #[test]
    fn anchor_uid_from_get_forbidden_does_not_propagate() {
        // The exact failure that FROZE the reconcile pre-fix: a 403
        // Forbidden on the anchor GET must collapse to None (un-owned
        // plan), NOT propagate and abort before the status write.
        let forbidden = kube::Error::Api(
            kube::core::Status::failure(
                "configmaps \"platform-migration-anchor\" is forbidden",
                "Forbidden",
            )
            .with_code(403)
            .boxed(),
        );
        assert_eq!(anchor_uid_from_get(Err(forbidden)), None);
    }

    // --- ADR 0048 root-App banner: the annotation surface reconciles
    // independently of the source patch (walk-found 2026-06-12: a gated
    // upgrade holds the source, so the banner never appeared). ---
    #[test]
    fn pending_upgrade_annotations_differ_set_clear_steady() {
        let up = PendingUpgrade {
            from: "0.2.26".to_string(),
            to: "0.2.27".to_string(),
            class: "RequiresRestart".to_string(),
            plan: "platform-0-2-26-to-0-2-27".to_string(),
        };
        let pending = Some(up);
        let matching = json!({"metadata": {"annotations": {
            "apprafter.io/upgrade-pending": "true",
            "apprafter.io/upgrade-from": "0.2.26",
            "apprafter.io/upgrade-to": "0.2.27",
            "apprafter.io/upgrade-class": "RequiresRestart",
            "apprafter.io/upgrade-plan": "platform-0-2-26-to-0-2-27",
        }}});
        let no_ann = json!({"metadata": {"name": "platform"}});

        // SET — the gated-upgrade case the old code missed: no
        // annotations yet, a gate pending → MUST patch.
        assert!(pending_upgrade_annotations_differ(&no_ann, &pending));
        // STEADY (gated) — already matches → no churn.
        assert!(!pending_upgrade_annotations_differ(&matching, &pending));
        // STALE — a field drifted (to) → re-patch.
        let mut stale = matching.clone();
        stale["metadata"]["annotations"]["apprafter.io/upgrade-to"] = json!("0.2.28");
        assert!(pending_upgrade_annotations_differ(&stale, &pending));
        // CLEAR — gate released but a stale banner is stamped → prune.
        assert!(pending_upgrade_annotations_differ(&matching, &None));
        // STEADY (no gate) — nothing stamped → nothing to do.
        assert!(!pending_upgrade_annotations_differ(&no_ann, &None));
    }

    // --- GC: keep only the current gate + mid-rollout (approved/executing);
    // collect stale-pending AND terminal completed/rejected/failed
    // (walk-found: 24-25, 25-26, 26-28 all lingered beside the new gate). ---
    #[test]
    fn superseded_platform_plan_names_keeps_gate_and_in_flight_only() {
        let mk = |name: &str, scope: &str, phase: Option<&str>| -> MigrationPlan {
            let mut v = json!({
                "apiVersion": "apprafter.io/v1alpha1",
                "kind": "MigrationPlan",
                "metadata": {"name": name, "namespace": "apprafter-system"},
                "spec": {
                    "scope": {"type": scope},
                    "trigger": {"type": "platformUpgrade", "field": "spec.source.targetRevision"}
                }
            });
            if let Some(ph) = phase {
                v["status"] = json!({"phase": ph});
            }
            serde_json::from_value(v).unwrap()
        };
        let keep = "platform-0-2-26-to-0-2-28";
        let plans = vec![
            mk(keep, "platform", Some("pending-approval")), // current gate — keep (name==keep)
            mk(
                "platform-0-2-26-to-0-2-27",
                "platform",
                Some("pending-approval"),
            ), // stale pending → collect
            mk("platform-0-2-20-to-0-2-24", "platform", Some("completed")), // terminal → collect
            mk("platform-0-2-22-to-0-2-23", "platform", Some("rejected")), // terminal → collect
            mk("platform-0-2-24-to-0-2-25", "platform", Some("executing")), // mid-rollout → KEEP
            mk("platform-0-2-25-to-0-2-26", "platform", Some("approved")), // mid-rollout → KEEP
            mk("some-app-plan", "application", Some("pending-approval")), // app-scope → never touch
            mk("platform-no-status", "platform", None),     // no status ⇒ pending default → collect
        ];
        let mut got = superseded_platform_plan_names(&plans, keep);
        got.sort();
        assert_eq!(
            got,
            vec![
                "platform-0-2-20-to-0-2-24".to_string(),
                "platform-0-2-22-to-0-2-23".to_string(),
                "platform-0-2-26-to-0-2-27".to_string(),
                "platform-no-status".to_string(),
            ]
        );

        // SETTLED (keep == "") — no current gate → every non-mid-rollout
        // platform plan is collected, incl. the just-completed one (so the
        // last completed plan is dropped once the upgrade settles).
        let mut settled = superseded_platform_plan_names(&plans, "");
        settled.sort();
        assert_eq!(
            settled,
            vec![
                "platform-0-2-20-to-0-2-24".to_string(),
                "platform-0-2-22-to-0-2-23".to_string(),
                "platform-0-2-26-to-0-2-27".to_string(),
                "platform-0-2-26-to-0-2-28".to_string(), // the gate too, now keep==""
                "platform-no-status".to_string(),
            ]
        );
    }

    #[test]
    fn build_anchor_health_patch_sets_and_prunes() {
        // pending Some → annotation-only ConfigMap body carrying the surface.
        let up = PendingUpgrade {
            from: "0.2.28".to_string(),
            to: "0.2.29".to_string(),
            class: "RequiresRestart".to_string(),
            plan: "platform-0-2-28-to-0-2-29".to_string(),
        };
        let set = build_anchor_health_patch(&Some(up));
        assert_eq!(set["kind"], "ConfigMap");
        assert_eq!(set["metadata"]["name"], PLATFORM_MIGRATION_ANCHOR);
        assert_eq!(
            set["metadata"]["annotations"]["apprafter.io/upgrade-pending"],
            "true"
        );
        assert_eq!(
            set["metadata"]["annotations"]["apprafter.io/upgrade-to"],
            "0.2.29"
        );
        // No `data` — SSA owns only the annotations, never fights Argo.
        assert!(set.get("data").is_none());
        // pending None → annotations omitted → SSA prunes the surface.
        let clear = build_anchor_health_patch(&None);
        assert!(clear["metadata"].get("annotations").is_none());
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
        // annotations — the load-bearing input the
        // `argoproj.io_Application` health banner reads (there is no
        // health aggregation from the anchored plan).
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
                sourcecredential: None,
            },
            trigger: MigrationTrigger {
                type_: "t".into(),
                field: "f".into(),
                from: None,
                to: None,
                approved_spec_hash: None,
            },
            risks: Some(MigrationRisks {
                classification: "breaking".into(),
                classifications: None,
                estimated_downtime: None,
                data_volume: None,
                reversible: None,
                requires_full_backup: None,
            }),
            changes: None,
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
                sourcecredential: None,
            },
            trigger: MigrationTrigger {
                type_: "t".into(),
                field: "f".into(),
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
        let plan = MigrationPlan::new("p", spec);
        assert_eq!(plan_classification(&plan), None);
    }
}

/// Whole reconciles against a scripted apiserver (WI-386): the status patch a
/// reconcile actually sends, with `BackupHealthy` in it, and the conditions
/// and history it must carry forward beside it. `status.conditions` is an
/// atomic list owned by one field manager, and an SSA write that leaves a
/// field out prunes it — so a patch that got the backup verdict right and
/// dropped `Ready` would be worse than no verdict at all.
#[cfg(test)]
mod backup_reconcile_tests {
    use std::sync::{Arc, Mutex};

    use kube::client::Body;
    use operator_core::Metrics;
    use serde_json::{json, Value};

    use super::*;
    use crate::backup_health::{
        REASON_NO_RUN_YET, REASON_SUCCEEDED, REASON_UNREADABLE, REASON_UNSCHEDULABLE,
    };
    use crate::status::{COND_BACKUP_HEALTHY, COND_BACKUP_RETENTION};

    #[derive(Clone, Debug)]
    struct Call {
        method: String,
        uri: String,
        body: Value,
    }

    impl Call {
        fn path(&self) -> &str {
            self.uri.split('?').next().unwrap_or("")
        }
    }

    /// What the scripted cluster holds.
    struct Cluster {
        stack: Value,
        parent: Value,
        cronjobs: Vec<Value>,
        jobs: Vec<Value>,
        pods: Vec<Value>,
        /// Answer every Job list with 403, as a missing RBAC rule would.
        forbid_jobs: bool,
        /// `data` of the runner's status ConfigMap; `None` answers 404.
        runner_status: Option<Value>,
    }

    fn not_found() -> (u16, Value) {
        (
            404,
            json!({ "kind": "Status", "apiVersion": "v1", "status": "Failure",
                    "reason": "NotFound", "code": 404, "message": "not found" }),
        )
    }

    fn list(kind: &str, api_version: &str, items: &[Value]) -> (u16, Value) {
        (
            200,
            json!({ "apiVersion": api_version, "kind": kind,
                    "metadata": { "resourceVersion": "1" }, "items": items }),
        )
    }

    fn respond(cluster: &Cluster, call: &Call) -> (u16, Value) {
        let path = call.path();
        let ns = "/namespaces/apprafter-system";
        match (call.method.as_str(), path) {
            ("GET", "/apis/argoproj.io/v1alpha1/namespaces/argocd/applications/platform") => {
                (200, cluster.parent.clone())
            }
            ("GET", p) if p == format!("/apis/apprafter.io/v1alpha1{ns}/migrationplans") => {
                list("MigrationPlanList", "apprafter.io/v1alpha1", &[])
            }
            ("GET", "/api/v1/nodes") => list("NodeList", "v1", &[]),
            ("GET", p) if p == format!("/apis/batch/v1{ns}/cronjobs") => {
                list("CronJobList", "batch/v1", &cluster.cronjobs)
            }
            ("GET", p) if p == format!("/apis/batch/v1{ns}/jobs") => {
                if cluster.forbid_jobs {
                    (
                        403,
                        json!({ "kind": "Status", "apiVersion": "v1", "status": "Failure",
                                "reason": "Forbidden", "code": 403,
                                "message": "jobs.batch is forbidden: User \"system:serviceaccount:apprafter-system:apprafter-operator\" cannot list resource \"jobs\"" }),
                    )
                } else {
                    list("JobList", "batch/v1", &cluster.jobs)
                }
            }
            ("GET", p) if p == format!("/api/v1{ns}/pods") => {
                assert!(
                    call.uri
                        .contains("labelSelector=apprafter.io%2Fbackup-runner%3Dtrue"),
                    "the runner pods are listed by their label only: {}",
                    call.uri
                );
                list("PodList", "v1", &cluster.pods)
            }
            ("GET", p) if p == format!("/api/v1{ns}/configmaps/apprafter-backup-status") => {
                match &cluster.runner_status {
                    Some(data) => (
                        200,
                        json!({ "apiVersion": "v1", "kind": "ConfigMap",
                                "metadata": { "name": "apprafter-backup-status",
                                              "namespace": "apprafter-system" },
                                "data": data }),
                    ),
                    None => not_found(),
                }
            }
            ("GET", p) if p.starts_with(&format!("/api/v1{ns}/configmaps/")) => not_found(),
            ("GET", p)
                if p == format!(
                    "/apis/apprafter.io/v1alpha1{ns}/platformstacks/default/status"
                ) =>
            {
                (200, cluster.stack.clone())
            }
            ("PATCH", p)
                if p == format!(
                    "/apis/apprafter.io/v1alpha1{ns}/platformstacks/default/status"
                ) =>
            {
                (200, cluster.stack.clone())
            }
            _ => panic!("unscripted request: {} {}", call.method, call.uri),
        }
    }

    fn scripted(cluster: Cluster) -> (Client, Arc<Mutex<Vec<Call>>>) {
        let log = Arc::new(Mutex::new(Vec::<Call>::new()));
        let sink = log.clone();
        let cluster = Arc::new(cluster);
        let service = tower::service_fn(move |req: http::Request<Body>| {
            let sink = sink.clone();
            let cluster = cluster.clone();
            async move {
                let method = req.method().to_string();
                let uri = req.uri().to_string();
                let bytes = req.into_body().collect_bytes().await.expect("request body");
                let call = Call {
                    method,
                    uri,
                    body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                };
                let (code, payload) = respond(&cluster, &call);
                sink.lock().expect("log").push(call);
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(code)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&payload).expect("payload")))
                        .expect("response"),
                )
            }
        });
        (Client::new(service, "apprafter-system"), log)
    }

    fn context(client: Client) -> Arc<Context> {
        Arc::new(Context {
            client,
            metrics: Arc::new(Metrics::new()),
            app_api_resource: ApiResource::from_gvk(&GroupVersionKind {
                group: "argoproj.io".into(),
                version: "v1alpha1".into(),
                kind: "Application".into(),
            }),
            capacity: operator_core::capacity::CapacityCache::new(),
            upstream: Arc::new(super::test_upstreams::NoRegistry),
            unrecorded_bumps: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn ago(minutes: i64) -> String {
        (Utc::now() - chrono::Duration::minutes(minutes))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    fn prior_condition(type_: &str, status: &str, reason: &str) -> Value {
        json!({ "type": type_, "status": status, "reason": reason, "message": "m",
                "lastTransitionTime": "2026-09-01T00:00:00+00:00" })
    }

    /// `PlatformStack/default` on `0.2.80`, recently polled (so no OCI
    /// request), with every condition a settled cluster carries and one
    /// history entry — the fields the backup verdict's write must not lose.
    fn stack(backup_enabled: bool, extra_conditions: Vec<Value>) -> Value {
        let mut conditions = vec![
            prior_condition("Ready", "True", "Healthy"),
            prior_condition("Synced", "True", "Reconciled"),
            prior_condition("UpstreamReachable", "True", "Reachable"),
            prior_condition("YankedVersion", "False", "NotYanked"),
            prior_condition("MigrationPending", "False", "Clean"),
            prior_condition("UpgradeAvailable", "False", "UpToDate"),
            prior_condition("UnauthorizedSourceModification", "False", "Clean"),
        ];
        conditions.extend(extra_conditions);
        json!({
            "apiVersion": "apprafter.io/v1alpha1", "kind": "PlatformStack",
            "metadata": { "name": "default", "namespace": "apprafter-system", "generation": 3,
                          "uid": "ps-uid" },
            "spec": {
                "channel": "stable", "pin": "0.2.80",
                "source": { "upstream": "oci://ghcr.io/apprafter/platform-stack",
                            "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                            "checkInterval": "6h" },
                "values": { "tier": 1 },
                "backup": {
                    "enabled": backup_enabled, "schedule": "0 3 * * *",
                    "bucket": "s3:https://s3.example/bucket",
                    "credentialRef": { "name": "apprafter-backup-s3" },
                    "stagingMode": "monolithic", "checkSchedule": "",
                },
            },
            "status": {
                "currentVersion": "0.2.80", "targetVersion": "0.2.80",
                "availableVersion": "0.2.80", "lastUpstreamCheck": Utc::now().to_rfc3339(),
                "versionHistory": [{ "version": "0.2.80", "appliedAt": "2026-09-20T00:00:00+00:00",
                                     "outcome": "succeeded" }],
                "conditions": conditions,
            },
        })
    }

    /// The root Application, settled on what `stack` asks for, so the
    /// reconcile patches nothing but status.
    fn parent(stack: &Value, target: &str, sync: &str) -> Value {
        let spec: PlatformStack = serde_json::from_value(stack.clone()).expect("stack");
        let desired = build_desired(&spec.spec, "0.2.80");
        json!({
            "apiVersion": "argoproj.io/v1alpha1", "kind": "Application",
            "metadata": {
                "name": "platform", "namespace": "argocd",
                "managedFields": [{ "manager": "platform-controller", "operation": "Apply",
                                    "fieldsV1": { "f:spec": { "f:source": { "f:targetRevision": {} } } } }],
            },
            "spec": { "source": { "targetRevision": target,
                                  "helm": { "valuesObject": desired.helm_values } } },
            "status": { "sync": { "status": sync }, "health": { "status": "Healthy" },
                        "operationState": { "phase": "Succeeded" } },
        })
    }

    fn cronjob() -> Value {
        json!({
            "apiVersion": "batch/v1", "kind": "CronJob",
            "metadata": { "name": "apprafter-backup", "namespace": "apprafter-system" },
            "spec": { "schedule": "0 3 * * *", "jobTemplate": {} },
        })
    }

    fn job(name: &str, created: &str, conditions: Value) -> Value {
        json!({
            "apiVersion": "batch/v1", "kind": "Job",
            "metadata": {
                "name": name, "namespace": "apprafter-system", "uid": format!("{name}-uid"),
                "creationTimestamp": created,
                "ownerReferences": [{ "apiVersion": "batch/v1", "kind": "CronJob",
                                      "name": "apprafter-backup", "uid": "cj-uid", "controller": true }],
            },
            "spec": { "activeDeadlineSeconds": 21600, "backoffLimit": 6, "template": {} },
            "status": { "startTime": created, "conditions": conditions },
        })
    }

    fn pending_pod(job_name: &str, since: &str) -> Value {
        json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {
                "name": format!("{job_name}-x7k2q"), "namespace": "apprafter-system",
                "creationTimestamp": since,
                "labels": { "apprafter.io/backup-runner": "true" },
                "ownerReferences": [{ "apiVersion": "batch/v1", "kind": "Job", "name": job_name,
                                      "uid": format!("{job_name}-uid"), "controller": true }],
            },
            "spec": { "containers": [{ "name": "runner", "image": "runner",
                "resources": { "requests": { "cpu": "100m", "memory": "256Mi" } } }] },
            "status": { "phase": "Pending", "conditions": [{
                "type": "PodScheduled", "status": "False", "reason": "Unschedulable",
                "message": "0/1 nodes are available: 1 Insufficient memory.",
                "lastTransitionTime": since }] },
        })
    }

    async fn reconcile_once(cluster: Cluster) -> (Action, Vec<Call>) {
        let stack: PlatformStack = serde_json::from_value(cluster.stack.clone()).expect("stack");
        let (client, log) = scripted(cluster);
        let action = reconcile(Arc::new(stack), context(client))
            .await
            .expect("the reconcile succeeds");
        let calls = log.lock().unwrap().clone();
        (action, calls)
    }

    /// The one status patch the reconcile sent.
    fn status_patch(calls: &[Call]) -> &Call {
        let patches: Vec<&Call> = calls
            .iter()
            .filter(|c| c.method == "PATCH" && c.path().ends_with("/platformstacks/default/status"))
            .collect();
        assert_eq!(patches.len(), 1, "one status write: {calls:#?}");
        patches[0]
    }

    fn written_condition<'a>(patch: &'a Call, type_: &str) -> Option<&'a Value> {
        patch.body["status"]["conditions"]
            .as_array()
            .expect("the patch carries the conditions")
            .iter()
            .find(|c| c["type"] == type_)
    }

    fn assert_everything_else_carried(patch: &Call) {
        for t in [
            "Ready",
            "Synced",
            "UpstreamReachable",
            "YankedVersion",
            "MigrationPending",
            "UpgradeAvailable",
            "UnauthorizedSourceModification",
        ] {
            assert!(
                written_condition(patch, t).is_some(),
                "{t} must ride the same write, or SSA prunes it: {:#}",
                patch.body
            );
        }
        assert_eq!(
            patch.body["status"]["versionHistory"][0]["version"], "0.2.80",
            "the history rides the write too: {:#}",
            patch.body
        );
        assert!(
            patch.uri.contains("fieldManager=platform-controller")
                && patch.uri.contains("force=true"),
            "the controller's own SSA identity: {}",
            patch.uri
        );
    }

    #[tokio::test]
    async fn an_unschedulable_runner_reaches_the_stack_beside_every_other_condition() {
        let stack = stack(true, vec![]);
        let parent = parent(&stack, "0.2.80", "Synced");
        let name = "apprafter-backup-29312340";
        let (action, calls) = reconcile_once(Cluster {
            parent,
            stack,
            cronjobs: vec![cronjob()],
            jobs: vec![job(name, &ago(20), json!([]))],
            pods: vec![pending_pod(name, &ago(20))],
            forbid_jobs: false,
            runner_status: None,
        })
        .await;
        let patch = status_patch(&calls);
        let c = written_condition(patch, COND_BACKUP_HEALTHY).expect("BackupHealthy written");
        assert_eq!(c["status"], "False");
        assert_eq!(c["reason"], REASON_UNSCHEDULABLE);
        let message = c["message"].as_str().unwrap();
        assert!(
            message.contains(name) && message.contains("Insufficient memory"),
            "{message}"
        );
        assert_everything_else_carried(patch);
        // Nothing is waiting on a grace, so the ordinary cadence stands.
        assert_eq!(action, Action::requeue(Duration::from_secs(6 * 3600)));
    }

    #[tokio::test]
    async fn a_pod_inside_its_grace_brings_the_next_reconcile_forward() {
        let stack = stack(true, vec![]);
        let parent = parent(&stack, "0.2.80", "Synced");
        let name = "apprafter-backup-29312340";
        let (action, calls) = reconcile_once(Cluster {
            parent,
            stack,
            cronjobs: vec![cronjob()],
            jobs: vec![job(name, &ago(5), json!([]))],
            pods: vec![pending_pod(name, &ago(5))],
            forbid_jobs: false,
            runner_status: None,
        })
        .await;
        let c = written_condition(status_patch(&calls), COND_BACKUP_HEALTHY).unwrap();
        assert_eq!(c["reason"], REASON_NO_RUN_YET);
        // No watch event arrives when the grace ends, so the reconcile itself
        // must come back then: about five minutes, never the six hours.
        let wanted = |secs: u64| Action::requeue(Duration::from_secs(secs));
        assert!(
            (290..=302).any(|s| action == wanted(s)),
            "expected a requeue at the end of the grace, got {action:?}"
        );
    }

    #[tokio::test]
    async fn a_later_success_clears_the_failure_on_the_stack() {
        let failing = json!({
            "type": COND_BACKUP_HEALTHY, "status": "False", "reason": REASON_UNSCHEDULABLE,
            "message": "backup Job apprafter-backup-29310900: …",
            "lastTransitionTime": "2026-09-22T03:10:05+00:00",
        });
        let stack = stack(true, vec![failing]);
        let parent = parent(&stack, "0.2.80", "Synced");
        let done = json!([{ "type": "Complete", "status": "True",
                            "lastTransitionTime": ago(60) }]);
        let (_, calls) = reconcile_once(Cluster {
            parent,
            stack,
            cronjobs: vec![cronjob()],
            jobs: vec![job("apprafter-backup-29312340", &ago(61), done)],
            pods: vec![],
            forbid_jobs: false,
            runner_status: None,
        })
        .await;
        let patch = status_patch(&calls);
        let c = written_condition(patch, COND_BACKUP_HEALTHY).unwrap();
        assert_eq!(c["status"], "True");
        assert_eq!(c["reason"], REASON_SUCCEEDED);
        assert_ne!(
            c["lastTransitionTime"], "2026-09-22T03:10:05+00:00",
            "the flip is a transition"
        );
        assert_everything_else_carried(patch);
    }

    #[tokio::test]
    async fn disabling_backups_removes_the_condition_and_reads_nothing() {
        let old = json!({
            "type": COND_BACKUP_HEALTHY, "status": "False", "reason": REASON_UNSCHEDULABLE,
            "message": "m", "lastTransitionTime": "2026-09-22T03:10:05+00:00",
        });
        let stack = stack(false, vec![old]);
        let parent = parent(&stack, "0.2.80", "Synced");
        let (_, calls) = reconcile_once(Cluster {
            parent,
            stack,
            cronjobs: vec![],
            jobs: vec![],
            pods: vec![],
            forbid_jobs: false,
            runner_status: None,
        })
        .await;
        let patch = status_patch(&calls);
        assert!(
            written_condition(patch, COND_BACKUP_HEALTHY).is_none(),
            "{:#}",
            patch.body
        );
        assert_everything_else_carried(patch);
        assert!(
            !calls.iter().any(|c| c.path().starts_with("/apis/batch/")),
            "a cluster without backups is not read for them: {calls:#?}"
        );
    }

    #[tokio::test]
    async fn a_forbidden_read_is_unknown_and_the_reconcile_carries_on() {
        let stack = stack(true, vec![]);
        let parent = parent(&stack, "0.2.80", "Synced");
        let (_, calls) = reconcile_once(Cluster {
            parent,
            stack,
            cronjobs: vec![cronjob()],
            jobs: vec![],
            pods: vec![],
            forbid_jobs: true,
            runner_status: None,
        })
        .await;
        let patch = status_patch(&calls);
        let c = written_condition(patch, COND_BACKUP_HEALTHY).unwrap();
        assert_eq!(c["status"], "Unknown");
        assert_eq!(c["reason"], REASON_UNREADABLE);
        assert!(
            c["message"].as_str().unwrap().contains("forbidden"),
            "{c:#}"
        );
        assert_everything_else_carried(patch);
    }

    #[tokio::test]
    async fn a_platform_upgrade_in_flight_does_not_freeze_the_backup_verdict() {
        // The parent is mid-sync to a new target: the reconcile takes its
        // early in-flight return. That is when a small node is most short of
        // room, so the verdict must be written on that path too.
        let stack = stack(true, vec![]);
        let parent = parent(&stack, "0.2.79", "OutOfSync");
        let name = "apprafter-backup-29312340";
        let (action, calls) = reconcile_once(Cluster {
            parent,
            stack,
            cronjobs: vec![cronjob()],
            jobs: vec![job(name, &ago(20), json!([]))],
            pods: vec![pending_pod(name, &ago(20))],
            forbid_jobs: false,
            runner_status: None,
        })
        .await;
        let c = written_condition(status_patch(&calls), COND_BACKUP_HEALTHY).unwrap();
        assert_eq!(c["reason"], REASON_UNSCHEDULABLE);
        assert_eq!(action, Action::requeue(IN_FLIGHT_REQUEUE));
    }

    /// WI-389: the scoped key's prune, recorded by the check run, reaches
    /// the stack as its own condition in the same write — `BackupHealthy`
    /// stays about the runs, and nothing else is lost.
    #[tokio::test]
    async fn a_prune_the_key_may_not_run_reaches_the_stack_as_its_own_condition() {
        let mut stack = stack(true, vec![]);
        stack["spec"]["backup"]["checkSchedule"] = json!("0 6 * * 0");
        stack["metadata"]["annotations"] =
            json!({ "apprafter.io/last-prune": "2026-09-01T10:00:00+00:00" });
        let parent = parent(&stack, "0.2.80", "Synced");
        let done = ago(60);
        let (_, calls) = reconcile_once(Cluster {
            parent,
            stack,
            cronjobs: vec![cronjob()],
            jobs: vec![job(
                "apprafter-backup-29312340",
                &ago(70),
                json!([{ "type": "Complete", "status": "True", "lastTransitionTime": done }]),
            )],
            pods: vec![],
            forbid_jobs: false,
            runner_status: Some(json!({
                "lastSuccess": done,
                "lastCheck": "2026-09-20T06:00:30+00:00", "lastCheckResult": "passed",
                "lastCheckError": "",
                "lastPrune": "2026-09-20T06:00:41+00:00", "lastPruneResult": "not-permitted",
                "lastPruneBy": "check",
                "lastPruneDetail": "not permitted: the storage refused to delete snapshot \
                    ecd0be32 (Access Denied.), so nothing was deleted",
                "repoStatsAt": "2026-09-20T06:00:44+00:00", "repoBytes": "1288490189",
                "repoSnapshots": "42", "repoBlobs": "310512",
            })),
        })
        .await;
        let patch = status_patch(&calls);
        let health = written_condition(patch, COND_BACKUP_HEALTHY).expect("BackupHealthy");
        assert_eq!(
            health["status"], "True",
            "retention is not a failing backup"
        );
        let c = written_condition(patch, COND_BACKUP_RETENTION).expect("BackupRetention written");
        assert_eq!(c["status"], "False");
        assert_eq!(c["reason"], crate::backup_retention::REASON_NOT_PERMITTED);
        let message = c["message"].as_str().unwrap();
        assert!(message.contains("retention is not enforced"), "{message}");
        assert!(message.contains("1.2 GiB in 42 snapshot(s)"), "{message}");
        assert!(message.contains("2026-09-01T10:00:00+00:00"), "{message}");
        assert_everything_else_carried(patch);
    }

    #[tokio::test]
    async fn disabling_backups_removes_the_retention_condition_too() {
        let old = json!({
            "type": COND_BACKUP_RETENTION, "status": "False", "reason": "PruneNotPermitted",
            "message": "m", "lastTransitionTime": "2026-09-22T03:10:05+00:00",
        });
        let stack = stack(false, vec![old]);
        let parent = parent(&stack, "0.2.80", "Synced");
        let (_, calls) = reconcile_once(Cluster {
            parent,
            stack,
            cronjobs: vec![],
            jobs: vec![],
            pods: vec![],
            forbid_jobs: false,
            runner_status: None,
        })
        .await;
        let patch = status_patch(&calls);
        assert!(
            written_condition(patch, COND_BACKUP_RETENTION).is_none(),
            "{:#}",
            patch.body
        );
        assert_everything_else_carried(patch);
    }
}

/// Stand-ins for the published chart repository, for the whole-reconcile
/// tests. None of them touches the network.
#[cfg(test)]
mod test_upstreams {
    use super::*;

    /// Fails the test if the reconcile asks the registry anything. For
    /// fixtures whose `lastUpstreamCheck` is fresh: a request there would
    /// otherwise go to ghcr.io.
    pub(super) struct NoRegistry;

    #[async_trait::async_trait]
    impl Upstream for NoRegistry {
        async fn channel_latest(
            &self,
            upstream: &str,
            _: Channel,
            _: &str,
        ) -> Result<(String, bool, Option<CompatibilityDoc>), Error> {
            panic!("this test does not reach the registry, but the reconcile asked {upstream} for the channel-latest")
        }

        async fn path_max_change_class(
            &self,
            upstream: &str,
            from_version: &str,
            to_version: &str,
        ) -> Result<ChangeClass, CompatError> {
            panic!(
                "this test does not reach the registry, but the reconcile asked {upstream} \
                 to classify {from_version} -> {to_version}"
            )
        }
    }

    /// A registry that takes every request and never answers.
    pub(super) struct SilentRegistry;

    #[async_trait::async_trait]
    impl Upstream for SilentRegistry {
        async fn channel_latest(
            &self,
            _: &str,
            _: Channel,
            _: &str,
        ) -> Result<(String, bool, Option<CompatibilityDoc>), Error> {
            std::future::pending().await
        }

        async fn path_max_change_class(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<ChangeClass, CompatError> {
            std::future::pending().await
        }
    }

    /// A registry that answers at once: `latest` is the channel-latest and
    /// every transition classifies as `class`.
    pub(super) struct Published {
        pub(super) latest: &'static str,
        pub(super) class: ChangeClass,
    }

    #[async_trait::async_trait]
    impl Upstream for Published {
        async fn channel_latest(
            &self,
            _: &str,
            _: Channel,
            _: &str,
        ) -> Result<(String, bool, Option<CompatibilityDoc>), Error> {
            Ok((self.latest.to_string(), true, None))
        }

        async fn path_max_change_class(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<ChangeClass, CompatError> {
            Ok(self.class)
        }
    }
}

/// Whole reconciles against an apiserver and a registry that may never
/// answer (WI-400). The apiserver is scripted and MUTABLE between passes, so
/// a test can cut one reconcile mid-flight and then run the one that
/// follows it against what the first left behind.
#[cfg(test)]
mod bounded_reconcile_tests {
    use std::sync::{Arc, Mutex};

    use kube::client::Body;
    use operator_core::Metrics;
    use serde_json::{json, Value};

    use super::test_upstreams::{NoRegistry, Published, SilentRegistry};
    use super::*;

    const NS: &str = "/namespaces/apprafter-system";
    const PARENT: &str = "/apis/argoproj.io/v1alpha1/namespaces/argocd/applications/platform";
    const ANCHOR: &str = "/api/v1/namespaces/apprafter-system/configmaps/platform-migration-anchor";
    const STATUS: &str =
        "/apis/apprafter.io/v1alpha1/namespaces/apprafter-system/platformstacks/default/status";
    const PLANS: &str = "/apis/apprafter.io/v1alpha1/namespaces/apprafter-system/migrationplans";
    const EVENTS: &str = "/apis/events.k8s.io/v1/namespaces/apprafter-system/events";
    const CRD: &str =
        "/apis/apiextensions.k8s.io/v1/customresourcedefinitions/platformstacks.apprafter.io";

    #[derive(Clone, Debug)]
    struct Call {
        method: String,
        uri: String,
        body: Value,
    }

    impl Call {
        fn path(&self) -> &str {
            self.uri.split('?').next().unwrap_or("")
        }

        fn is(&self, method: &str, path: &str) -> bool {
            self.method == method && self.path() == path
        }
    }

    /// What the scripted apiserver holds. Writes land in it, so the next
    /// pass reads what the last one left.
    struct Cluster {
        stack: Value,
        parent: Value,
        plans: Vec<Value>,
        anchor: Option<Value>,
        /// `(method, path)` pairs the apiserver accepts and never answers.
        silent: Vec<(&'static str, String)>,
        calls: Vec<Call>,
        /// Status applies merge `status.conditions` by `type`, with the
        /// ownership kept in `metadata.managedFields`, as on a cluster whose
        /// CRD makes the list a list-map (WI-400): see
        /// [`apply_status_list_map`]. Off, a status write replaces the
        /// status whole.
        list_map: bool,
        /// The PlatformStack CRD the apiserver serves makes
        /// `status.conditions` a list-map ([`served_crd`]). Set with
        /// `list_map`; off, it is the atomic list a rollback serves.
        crd_list_map: bool,
    }

    type Shared = Arc<Mutex<Cluster>>;

    fn ok(body: Value) -> (u16, Value) {
        (200, body)
    }

    fn not_found() -> (u16, Value) {
        (
            404,
            json!({ "kind": "Status", "apiVersion": "v1", "status": "Failure",
                    "reason": "NotFound", "code": 404, "message": "not found" }),
        )
    }

    fn list(kind: &str, api_version: &str, items: &[Value]) -> (u16, Value) {
        ok(json!({ "apiVersion": api_version, "kind": kind,
                   "metadata": { "resourceVersion": "1" }, "items": items }))
    }

    fn respond(cluster: &mut Cluster, call: &Call) -> (u16, Value) {
        let path = call.path().to_string();
        let plan_name = path.strip_prefix(&format!("{PLANS}/")).map(str::to_string);
        match (call.method.as_str(), path.as_str()) {
            ("GET", PARENT) => ok(cluster.parent.clone()),
            ("PATCH", PARENT) => {
                let source = &call.body["spec"]["source"];
                cluster.parent["spec"]["source"]["targetRevision"] =
                    source["targetRevision"].clone();
                cluster.parent["spec"]["source"]["helm"] = source["helm"].clone();
                ok(cluster.parent.clone())
            }
            ("GET", PLANS) => list("MigrationPlanList", "apprafter.io/v1alpha1", &cluster.plans),
            ("POST", PLANS) => {
                cluster.plans.push(call.body.clone());
                (201, call.body.clone())
            }
            ("GET", _) if plan_name.is_some() => {
                let name = plan_name.unwrap();
                match cluster.plans.iter().find(|p| p["metadata"]["name"] == name) {
                    Some(plan) => ok(plan.clone()),
                    None => not_found(),
                }
            }
            ("DELETE", _) if plan_name.is_some() => {
                let name = plan_name.unwrap();
                cluster.plans.retain(|p| p["metadata"]["name"] != name);
                ok(
                    json!({ "kind": "Status", "apiVersion": "v1", "status": "Success",
                           "metadata": {} }),
                )
            }
            ("GET", ANCHOR) => match &cluster.anchor {
                Some(anchor) => ok(anchor.clone()),
                None => not_found(),
            },
            ("PATCH", ANCHOR) => ok(cluster
                .anchor
                .clone()
                .expect("only an existing anchor is patched")),
            ("GET", "/api/v1/nodes") => list("NodeList", "v1", &[]),
            ("GET", p) if p == format!("/apis/batch/v1{NS}/cronjobs") => {
                list("CronJobList", "batch/v1", &[])
            }
            ("GET", p) if p == format!("/apis/batch/v1{NS}/jobs") => {
                list("JobList", "batch/v1", &[])
            }
            ("GET", p) if p == format!("/api/v1{NS}/pods") => list("PodList", "v1", &[]),
            ("GET", p) if p == format!("/api/v1{NS}/configmaps/apprafter-backup-status") => {
                not_found()
            }
            ("GET", STATUS) => ok(cluster.stack.clone()),
            ("PATCH", STATUS) if cluster.list_map => apply_status_list_map(cluster, call),
            ("PATCH", STATUS) => {
                cluster.stack["status"] = call.body["status"].clone();
                ok(cluster.stack.clone())
            }
            ("POST", EVENTS) => (201, call.body.clone()),
            ("GET", CRD) => ok(served_crd(cluster.crd_list_map)),
            _ => panic!("unscripted request: {} {}", call.method, call.uri),
        }
    }

    /// The PlatformStack CRD as the apiserver serves it, cut down to what
    /// `stall` reads: `status.conditions` in `v1alpha1`, a list-map keyed by
    /// `type` or the atomic list of the CRD before WI-400.
    fn served_crd(list_map: bool) -> Value {
        let mut conditions = json!({ "type": "array",
                                     "items": { "type": "object", "required": ["type"] } });
        if list_map {
            conditions["x-kubernetes-list-type"] = json!("map");
            conditions["x-kubernetes-list-map-keys"] = json!(["type"]);
        }
        json!({
            "apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": { "name": "platformstacks.apprafter.io" },
            "spec": {
                "group": "apprafter.io", "scope": "Namespaced",
                "names": { "kind": "PlatformStack", "plural": "platformstacks" },
                "versions": [{
                    "name": "v1alpha1", "served": true, "storage": true,
                    "schema": { "openAPIV3Schema": { "type": "object", "properties": {
                        "status": { "type": "object",
                                    "properties": { "conditions": conditions } } } } },
                }],
            },
        })
    }

    /// A status apply as the apiserver merges it once `status.conditions` is
    /// a list-map keyed by `type` (WI-400), following what kind measured
    /// (`e2e/platformstack-listmap-upgrade-proof.sh`): a manager owns, by
    /// `type`, the conditions it sent; an entry it stops sending goes unless
    /// another manager owns it by key; an ownership of the whole list left
    /// from the atomic list (`f:conditions: {}`) is replaced by one by key,
    /// and an entry that apply leaves out stays, owned by nobody; a pair of
    /// one `type` already on the object becomes one entry; a list that names
    /// one `type` twice is refused. The rest of the status is
    /// `platform-controller`'s, and its apply replaces it.
    fn apply_status_list_map(cluster: &mut Cluster, call: &Call) -> (u16, Value) {
        let manager = call
            .uri
            .split(['?', '&'])
            .find_map(|p| p.strip_prefix("fieldManager="))
            .expect("an apply names its field manager")
            .to_string();
        let type_of = |c: &Value| c["type"].as_str().unwrap_or_default().to_string();
        let key = |t: &str| format!(r#"k:{{"type":"{t}"}}"#);
        let sent = call.body["status"]["conditions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut sent_types: Vec<String> = Vec::new();
        for t in sent.iter().map(type_of) {
            if sent_types.contains(&t) {
                return (
                    422,
                    json!({ "kind": "Status", "apiVersion": "v1", "status": "Failure",
                            "reason": "Invalid", "code": 422,
                            "message": format!(
                                ".status.conditions: duplicate entries for key [type=\"{t}\"]") }),
                );
            }
            sent_types.push(t);
        }
        let fields = cluster.stack["metadata"]["managedFields"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let (mine, others): (Vec<Value>, Vec<Value>) = fields
            .into_iter()
            .partition(|e| e["subresource"] == "status" && e["manager"] == manager.as_str());
        let owns = |e: &Value, t: &str| {
            e["subresource"] == "status"
                && e["fieldsV1"]["f:status"]["f:conditions"]
                    .get(key(t))
                    .is_some()
        };
        let mut conditions = cluster.stack["status"]["conditions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        conditions.retain(|c| {
            let t = type_of(c);
            let let_go = mine.iter().any(|e| owns(e, &t)) && !sent_types.contains(&t);
            !let_go || others.iter().any(|e| owns(e, &t))
        });
        for c in sent {
            match conditions
                .iter_mut()
                .find(|have| type_of(have) == type_of(&c))
            {
                Some(have) => *have = c,
                None => conditions.push(c),
            }
        }
        let mut seen: Vec<String> = Vec::new();
        conditions.retain(|c| {
            let t = type_of(c);
            let first = !seen.contains(&t);
            seen.push(t);
            first
        });
        if manager == FIELD_MANAGER {
            cluster.stack["status"] = call.body["status"].clone();
        }
        cluster.stack["status"]["conditions"] = json!(conditions);
        let mut fields = others;
        if !sent_types.is_empty() {
            let owned: serde_json::Map<String, Value> = sent_types
                .iter()
                .map(|t| (key(t), json!({ ".": {} })))
                .collect();
            fields.push(json!({
                "manager": manager, "operation": "Apply", "subresource": "status",
                "apiVersion": "apprafter.io/v1alpha1", "fieldsType": "FieldsV1",
                "fieldsV1": { "f:status": { "f:conditions": owned } },
            }));
        }
        cluster.stack["metadata"]["managedFields"] = json!(fields);
        ok(cluster.stack.clone())
    }

    fn scripted(cluster: Cluster) -> (Client, Shared) {
        let shared: Shared = Arc::new(Mutex::new(cluster));
        let state = shared.clone();
        let service = tower::service_fn(move |req: http::Request<Body>| {
            let state = state.clone();
            async move {
                let method = req.method().to_string();
                let uri = req.uri().to_string();
                let bytes = req.into_body().collect_bytes().await.expect("request body");
                let call = Call {
                    method,
                    uri,
                    body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                };
                let answer = {
                    let mut cluster = state.lock().expect("cluster");
                    cluster.calls.push(call.clone());
                    let silent = cluster
                        .silent
                        .iter()
                        .any(|(m, p)| *m == call.method && p.as_str() == call.path());
                    (!silent).then(|| respond(&mut cluster, &call))
                };
                let Some((code, payload)) = answer else {
                    std::future::pending::<()>().await;
                    unreachable!("a silent request is never answered");
                };
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(code)
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&payload).expect("payload")))
                        .expect("response"),
                )
            }
        });
        (Client::new(service, "apprafter-system"), shared)
    }

    fn context(client: Client, upstream: Arc<dyn Upstream>) -> Arc<Context> {
        Arc::new(Context {
            client,
            metrics: Arc::new(Metrics::new()),
            app_api_resource: ApiResource::from_gvk(&GroupVersionKind {
                group: "argoproj.io".into(),
                version: "v1alpha1".into(),
                kind: "Application".into(),
            }),
            capacity: operator_core::capacity::CapacityCache::new(),
            upstream,
            unrecorded_bumps: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn prior_condition(type_: &str, status: &str, reason: &str) -> Value {
        json!({ "type": type_, "status": status, "reason": reason, "message": "m",
                "lastTransitionTime": "2026-09-01T00:00:00+00:00" })
    }

    /// `PlatformStack/default` settled on `version`, pinned to `pin`,
    /// polled a moment ago (so no registry request unless a test clears
    /// `lastUpstreamCheck`), with every condition a settled cluster carries.
    fn stack_on(version: &str, pin: &str) -> Value {
        json!({
            "apiVersion": "apprafter.io/v1alpha1", "kind": "PlatformStack",
            "metadata": { "name": "default", "namespace": "apprafter-system", "generation": 3,
                          "uid": "ps-uid" },
            "spec": {
                "channel": "stable", "pin": pin,
                "source": { "upstream": "oci://ghcr.io/apprafter/platform-stack",
                            "repoURL": "oci://ghcr.io/apprafter/platform-stack",
                            "checkInterval": "6h" },
                "values": { "tier": 1 },
            },
            "status": {
                "currentVersion": version, "targetVersion": version,
                "availableVersion": pin, "lastUpstreamCheck": Utc::now().to_rfc3339(),
                "versionHistory": [{ "version": version,
                                     "appliedAt": "2026-09-20T00:00:00+00:00",
                                     "outcome": "succeeded" }],
                "conditions": [
                    prior_condition("Ready", "True", "Healthy"),
                    prior_condition("Synced", "True", "Reconciled"),
                    prior_condition("UpstreamReachable", "True", "Reachable"),
                    prior_condition("YankedVersion", "False", "NotYanked"),
                    prior_condition("MigrationPending", "False", "Clean"),
                    prior_condition("UpgradeAvailable", "False", "UpToDate"),
                    prior_condition("UnauthorizedSourceModification", "False", "Clean"),
                ],
            },
        })
    }

    /// The root Application on `target`, synced, healthy, its source owned
    /// by `platform-controller`.
    fn parent_on(stack: &Value, target: &str) -> Value {
        let spec: PlatformStack = serde_json::from_value(stack.clone()).expect("stack");
        let desired = build_desired(&spec.spec, target);
        json!({
            "apiVersion": "argoproj.io/v1alpha1", "kind": "Application",
            "metadata": {
                "name": "platform", "namespace": "argocd",
                "managedFields": [{ "manager": "platform-controller", "operation": "Apply",
                                    "fieldsV1": { "f:spec": { "f:source": { "f:targetRevision": {} } } } }],
            },
            "spec": { "source": { "targetRevision": target,
                                  "helm": { "valuesObject": desired.helm_values } } },
            "status": { "sync": { "status": "Synced" }, "health": { "status": "Healthy" },
                        "operationState": { "phase": "Succeeded" } },
        })
    }

    fn cluster(stack: Value, parent: Value) -> Cluster {
        Cluster {
            stack,
            parent,
            plans: vec![],
            anchor: None,
            silent: vec![],
            calls: vec![],
            list_map: false,
            crd_list_map: false,
        }
    }

    fn the_stack(state: &Shared) -> Arc<PlatformStack> {
        let stack = state.lock().unwrap().stack.clone();
        Arc::new(serde_json::from_value(stack).expect("stack"))
    }

    fn calls(state: &Shared) -> Vec<Call> {
        state.lock().unwrap().calls.clone()
    }

    /// The one status patch the reconcile sent.
    fn status_patch(calls: &[Call]) -> Call {
        let patches: Vec<&Call> = calls.iter().filter(|c| c.is("PATCH", STATUS)).collect();
        assert_eq!(patches.len(), 1, "one status write: {calls:#?}");
        patches[0].clone()
    }

    fn written_condition<'a>(patch: &'a Call, type_: &str) -> &'a Value {
        patch.body["status"]["conditions"]
            .as_array()
            .expect("the patch carries the conditions")
            .iter()
            .find(|c| c["type"] == type_)
            .unwrap_or_else(|| panic!("{type_} must ride the write: {:#}", patch.body))
    }

    /// Every condition rides the write, under the controller's own SSA
    /// identity: a write that left one out would prune it.
    fn assert_every_condition_carried(patch: &Call) {
        for t in [
            "Ready",
            "Synced",
            "UpstreamReachable",
            "YankedVersion",
            "MigrationPending",
            "UpgradeAvailable",
            "UnauthorizedSourceModification",
        ] {
            written_condition(patch, t);
        }
        assert!(
            patch.uri.contains("fieldManager=platform-controller")
                && patch.uri.contains("force=true"),
            "{}",
            patch.uri
        );
    }

    /// The channel-latest comes from the upstream behind `ctx.upstream`.
    #[tokio::test]
    async fn the_channel_latest_is_read_through_the_upstream() {
        let mut stack = stack_on("0.2.80", "0.2.80");
        stack["status"]["lastUpstreamCheck"] = Value::Null;
        let parent = parent_on(&stack, "0.2.80");
        let (client, state) = scripted(cluster(stack, parent));
        let upstream = Arc::new(Published {
            latest: "0.2.81",
            class: ChangeClass::Safe,
        });
        reconcile(the_stack(&state), context(client, upstream))
            .await
            .expect("the reconcile succeeds");
        let patch = status_patch(&calls(&state));
        assert_eq!(patch.body["status"]["availableVersion"], "0.2.81");
        assert_eq!(
            written_condition(&patch, "UpstreamReachable")["status"],
            "True"
        );
        assert_every_condition_carried(&patch);
    }

    /// A transition is classified through `ctx.upstream` too: a breaking one
    /// is gated behind a MigrationPlan instead of deployed.
    #[tokio::test]
    async fn a_transition_is_classified_through_the_upstream() {
        let stack = stack_on("0.2.80", "0.2.81");
        let parent = parent_on(&stack, "0.2.80");
        let (client, state) = scripted(cluster(stack, parent));
        let upstream = Arc::new(Published {
            latest: "0.2.81",
            class: ChangeClass::Breaking,
        });
        reconcile(the_stack(&state), context(client, upstream))
            .await
            .expect("the reconcile succeeds");
        let calls = calls(&state);
        assert!(
            calls.iter().any(|c| c.is("POST", PLANS)
                && c.body["metadata"]["name"] == "platform-0-2-80-to-0-2-81"),
            "{calls:#?}"
        );
        let patch = status_patch(&calls);
        assert_eq!(
            written_condition(&patch, "MigrationPending")["status"],
            "True"
        );
        assert_eq!(patch.body["status"]["currentVersion"], "0.2.80");
        assert_every_condition_carried(&patch);
    }

    /// Far past every budget inside a reconcile: a reconcile still running at
    /// this point is hung, not slow.
    const HUNG: Duration = Duration::from_secs(600);

    /// WI-400: a registry that takes the connection and never answers. The
    /// poll is the reconcile's FIRST `.await`, so without a bound nothing
    /// after it ran — no parent read, no status — and every condition froze
    /// at its last value, `UpstreamReachable=True` included: the v0.2.12
    /// wedge by a different road. Bounded, the poll fails like any registry
    /// error and the reconcile degrades: the pin is still enforced and the
    /// status says the upstream is unreachable.
    #[tokio::test(start_paused = true)]
    async fn a_registry_that_never_answers_the_poll_degrades_instead_of_freezing() {
        let mut stack = stack_on("0.2.80", "0.2.80");
        stack["status"]["lastUpstreamCheck"] = Value::Null;
        let parent = parent_on(&stack, "0.2.80");
        let (client, state) = scripted(cluster(stack, parent));
        let started = tokio::time::Instant::now();
        tokio::time::timeout(
            HUNG,
            reconcile(the_stack(&state), context(client, Arc::new(SilentRegistry))),
        )
        .await
        .expect("the reconcile finished on its own")
        .expect("a registry that never answers degrades, it does not fail the reconcile");
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(20),
            "the poll's budget"
        );
        let patch = status_patch(&calls(&state));
        let reachable = written_condition(&patch, "UpstreamReachable");
        assert_eq!(reachable["status"], "False");
        assert_eq!(reachable["reason"], "PollFailed");
        assert!(
            reachable["message"]
                .as_str()
                .unwrap()
                .contains("did not finish resolving the channel-latest within 20s"),
            "{reachable:#}"
        );
        assert_eq!(
            patch.body["status"]["availableVersion"], "0.2.80",
            "kept, not cleared"
        );
        assert!(
            patch.body["status"]["lastUpstreamCheck"].is_null(),
            "a failed poll does not count as a check: {:#}",
            patch.body
        );
        assert_every_condition_carried(&patch);
    }

    /// The same silent registry while the parent is mid-sync: the in-flight
    /// early return writes the degraded verdict too.
    #[tokio::test(start_paused = true)]
    async fn a_registry_that_never_answers_mid_upgrade_still_reaches_the_status() {
        let mut stack = stack_on("0.2.80", "0.2.81");
        stack["status"]["lastUpstreamCheck"] = Value::Null;
        let mut parent = parent_on(&stack, "0.2.80");
        parent["status"]["sync"]["status"] = json!("OutOfSync");
        let (client, state) = scripted(cluster(stack, parent));
        let action = tokio::time::timeout(
            HUNG,
            reconcile(the_stack(&state), context(client, Arc::new(SilentRegistry))),
        )
        .await
        .expect("the reconcile finished on its own")
        .expect("the reconcile succeeds");
        assert_eq!(action, Action::requeue(IN_FLIGHT_REQUEUE));
        let patch = status_patch(&calls(&state));
        assert_eq!(
            written_condition(&patch, "UpstreamReachable")["status"],
            "False"
        );
        assert_every_condition_carried(&patch);
    }

    /// A pin to a version whose transition cannot be classified because the
    /// registry never answers: the reconcile fails CLOSED — it holds the
    /// current target, creates no plan, and says why — instead of hanging
    /// before the gate.
    #[tokio::test(start_paused = true)]
    async fn a_registry_that_never_answers_the_classification_holds_the_current_target() {
        let stack = stack_on("0.2.80", "0.2.81");
        let parent = parent_on(&stack, "0.2.80");
        let (client, state) = scripted(cluster(stack, parent));
        let started = tokio::time::Instant::now();
        tokio::time::timeout(
            HUNG,
            reconcile(the_stack(&state), context(client, Arc::new(SilentRegistry))),
        )
        .await
        .expect("the reconcile finished on its own")
        .expect("the reconcile succeeds");
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(20),
            "the classification's budget"
        );
        let calls = calls(&state);
        assert!(
            !calls.iter().any(|c| c.is("POST", PLANS)),
            "no gate without a class: {calls:#?}"
        );
        let bump = calls
            .iter()
            .find(|c| c.is("PATCH", PARENT))
            .expect("the parent is patched");
        assert_eq!(
            bump.body["spec"]["source"]["targetRevision"], "0.2.80",
            "held"
        );
        let patch = status_patch(&calls);
        let reachable = written_condition(&patch, "UpstreamReachable");
        assert_eq!(reachable["status"], "False");
        assert!(
            reachable["message"]
                .as_str()
                .unwrap()
                .contains("did not finish classifying the transition within 20s"),
            "{reachable:#}"
        );
        assert_eq!(patch.body["status"]["currentVersion"], "0.2.80");
        assert_every_condition_carried(&patch);
    }

    /// `BackupHealthy` reads the backup objects (WI-386). A read the
    /// apiserver never answers is itself the verdict, `Unknown`, and it is
    /// written beside every other condition — the ADR 0048 anchor-403 lesson
    /// made these reads tolerate ERRORS; this makes them tolerate a HANG.
    #[tokio::test(start_paused = true)]
    async fn a_backup_read_that_never_answers_is_unknown_and_the_status_is_written() {
        let mut stack = stack_on("0.2.80", "0.2.80");
        stack["spec"]["backup"] = json!({
            "enabled": true, "schedule": "0 3 * * *",
            "bucket": "s3:https://s3.example/bucket",
            "credentialRef": { "name": "apprafter-backup-s3" },
            "stagingMode": "monolithic", "checkSchedule": "",
        });
        let parent = parent_on(&stack, "0.2.80");
        let mut cluster = cluster(stack, parent);
        cluster
            .silent
            .push(("GET", format!("/apis/batch/v1{NS}/cronjobs")));
        let (client, state) = scripted(cluster);
        let started = tokio::time::Instant::now();
        tokio::time::timeout(
            HUNG,
            reconcile(the_stack(&state), context(client, Arc::new(NoRegistry))),
        )
        .await
        .expect("the reconcile finished on its own")
        .expect("the reconcile succeeds");
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(10),
            "the backup reads' budget"
        );
        let patch = status_patch(&calls(&state));
        let backup = written_condition(&patch, crate::status::COND_BACKUP_HEALTHY);
        assert_eq!(backup["status"], "Unknown");
        assert_eq!(backup["reason"], crate::backup_health::REASON_UNREADABLE);
        assert!(
            backup["message"]
                .as_str()
                .unwrap()
                .contains("the apiserver did not answer within 10s"),
            "{backup:#}"
        );
        assert_every_condition_carried(&patch);
    }

    /// `NodeDiskPressure` samples the kubelet through the apiserver's node
    /// proxy — a long-running request the apiserver's own 60s limit does not
    /// cover, and slowest exactly when the node is short of disk. A sample
    /// that never answers leaves the condition as it was, and the status is
    /// still written.
    ///
    /// The stack starts WITH a `NodeDiskPressure=True` from an earlier
    /// sample: "left as it was" has to be told apart from "pruned". The
    /// status write is an SSA apply under `platform-controller`, so a write
    /// that left the condition out would remove it from the cluster —
    /// exactly when the node is short of disk.
    #[tokio::test(start_paused = true)]
    async fn a_node_sample_that_never_answers_leaves_its_condition_and_the_status_is_written() {
        let mut stack = stack_on("0.2.80", "0.2.80");
        let earlier_sample =
            prior_condition("NodeDiskPressure", "True", "NodeFilesystemNearlyFull");
        stack["status"]["conditions"]
            .as_array_mut()
            .unwrap()
            .push(earlier_sample.clone());
        let parent = parent_on(&stack, "0.2.80");
        let mut cluster = cluster(stack, parent);
        cluster.silent.push(("GET", "/api/v1/nodes".to_string()));
        let (client, state) = scripted(cluster);
        let started = tokio::time::Instant::now();
        tokio::time::timeout(
            HUNG,
            reconcile(the_stack(&state), context(client, Arc::new(NoRegistry))),
        )
        .await
        .expect("the reconcile finished on its own")
        .expect("the reconcile succeeds");
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(10),
            "the node sample's budget"
        );
        let patch = status_patch(&calls(&state));
        assert_eq!(
            written_condition(&patch, COND_NODE_DISK_PRESSURE),
            &earlier_sample,
            "no sample, so the earlier verdict is carried unchanged — status, reason, \
             message and lastTransitionTime"
        );
        assert_every_condition_carried(&patch);
    }

    /// The anchor ConfigMap is an Argo-tree nicety (ADR 0048). Its lookup
    /// and its annotation patch never answering must not stop the gate it
    /// decorates: the plan is created un-owned, exactly as when the anchor is
    /// unreadable, and the status says the upgrade is pending.
    #[tokio::test(start_paused = true)]
    async fn an_anchor_that_never_answers_neither_blocks_the_gate_nor_the_status() {
        let stack = stack_on("0.2.80", "0.2.81");
        let parent = parent_on(&stack, "0.2.80");
        let mut cluster = cluster(stack, parent);
        cluster.anchor = Some(json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": { "name": "platform-migration-anchor",
                          "namespace": "apprafter-system", "uid": "anchor-uid" },
        }));
        cluster.silent.push(("GET", ANCHOR.to_string()));
        let (client, state) = scripted(cluster);
        let upstream = Arc::new(Published {
            latest: "0.2.81",
            class: ChangeClass::Breaking,
        });
        let started = tokio::time::Instant::now();
        tokio::time::timeout(
            HUNG,
            reconcile(the_stack(&state), context(client, upstream)),
        )
        .await
        .expect("the reconcile finished on its own")
        .expect("the reconcile succeeds");
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(10),
            "two anchor calls, each on its own budget"
        );
        let calls = calls(&state);
        let plan = calls
            .iter()
            .find(|c| c.is("POST", PLANS))
            .expect("the gate is created");
        assert!(
            plan.body["metadata"]["ownerReferences"].is_null(),
            "un-owned, as when the anchor is unreadable: {:#}",
            plan.body
        );
        let patch = status_patch(&calls);
        assert_eq!(
            written_condition(&patch, "MigrationPending")["status"],
            "True"
        );
        assert_every_condition_carried(&patch);
    }

    /// The audit Events of a foreign-writer revert are best-effort. An Events
    /// API that never answers does not hold the status — which carries the
    /// same finding as `UnauthorizedSourceModification=True`.
    #[tokio::test(start_paused = true)]
    async fn an_events_api_that_never_answers_does_not_hold_the_status() {
        let stack = stack_on("0.2.80", "0.2.80");
        let mut parent = parent_on(&stack, "0.2.80");
        parent["metadata"]["managedFields"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "manager": "kubectl-edit", "operation": "Update",
                          "fieldsV1": { "f:spec": { "f:source": { "f:targetRevision": {} } } } }));
        let mut cluster = cluster(stack, parent);
        cluster.silent.push(("POST", EVENTS.to_string()));
        let (client, state) = scripted(cluster);
        let started = tokio::time::Instant::now();
        tokio::time::timeout(
            HUNG,
            reconcile(the_stack(&state), context(client, Arc::new(NoRegistry))),
        )
        .await
        .expect("the reconcile finished on its own")
        .expect("the reconcile succeeds");
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(10),
            "two audit Events, each on its own budget"
        );
        let patch = status_patch(&calls(&state));
        assert_eq!(
            written_condition(&patch, "UnauthorizedSourceModification")["status"],
            "True"
        );
        assert_every_condition_carried(&patch);
    }

    /// WI-400 / GOTCHA-51: an apiserver that takes a request and never
    /// answers held this singleton's reconcile — and every trigger queued
    /// behind it — for good. Under `RECONCILE_DEADLINE` the reconcile is
    /// abandoned at exactly the deadline and comes back as the controller's
    /// own error, which `error_policy` counts and requeues.
    #[tokio::test(start_paused = true)]
    async fn a_reconcile_the_apiserver_never_answers_is_abandoned_at_its_deadline() {
        let stack: Arc<PlatformStack> =
            Arc::new(serde_json::from_value(stack_on("0.2.80", "0.2.80")).expect("stack"));
        let ctx = context(
            operator_core::testing::stalled_client(),
            Arc::new(NoRegistry),
        );
        let started = tokio::time::Instant::now();
        let err = operator_core::deadline::within(
            RECONCILE_DEADLINE,
            reconcile(stack.clone(), ctx.clone()),
        )
        .await
        .expect_err("a reconcile that never finishes is cut");
        assert_eq!(started.elapsed(), RECONCILE_DEADLINE);
        assert!(
            matches!(
                err,
                Error::TimedOut(operator_core::deadline::ReconcileTimedOut { after })
                    if after == RECONCILE_DEADLINE
            ),
            "{err:?}"
        );
        assert_eq!(
            error_policy(stack, &err, ctx.clone()),
            Action::requeue(Duration::from_secs(60))
        );
        assert_eq!(
            ctx.metrics
                .reconcile_timeouts
                .with_label_values(&["PlatformStack"])
                .get(),
            1.0
        );
        assert_eq!(
            ctx.metrics
                .reconcile_errors
                .with_label_values(&["PlatformStack"])
                .get(),
            1.0
        );
    }

    /// Any other failure is counted as an error, not as a timeout.
    #[tokio::test]
    async fn an_ordinary_failure_is_not_counted_as_a_timeout() {
        let stack: Arc<PlatformStack> =
            Arc::new(serde_json::from_value(stack_on("0.2.80", "0.2.80")).expect("stack"));
        let (client, _) = scripted(cluster(stack_on("0.2.80", "0.2.80"), json!({})));
        let ctx = context(client, Arc::new(NoRegistry));
        let err = Error::CheckInterval("9x".into());
        assert_eq!(
            error_policy(stack, &err, ctx.clone()),
            Action::requeue(Duration::from_secs(60))
        );
        assert_eq!(
            ctx.metrics
                .reconcile_timeouts
                .with_label_values(&["PlatformStack"])
                .get(),
            0.0
        );
        assert_eq!(
            ctx.metrics
                .reconcile_errors
                .with_label_values(&["PlatformStack"])
                .get(),
            1.0
        );
    }

    /// `error_policy` reports a cut as a Warning Event on the stack, the
    /// record `kubectl describe platformstack default` keeps after the
    /// `ReconcileStalled` condition is gone, and writes no status itself:
    /// the condition is `reconcile_with_deadline`'s (see `stall_tests`).
    #[tokio::test(start_paused = true)]
    async fn an_abandoned_reconcile_is_reported_as_a_warning_event_on_the_stack() {
        let stack = stack_on("0.2.80", "0.2.80");
        let parent = parent_on(&stack, "0.2.80");
        let mut cluster = cluster(stack, parent);
        cluster.silent.push(("GET", PARENT.to_string()));
        let (client, state) = scripted(cluster);
        let ctx = context(client, Arc::new(NoRegistry));
        let err = operator_core::deadline::within(
            RECONCILE_DEADLINE,
            reconcile(the_stack(&state), ctx.clone()),
        )
        .await
        .expect_err("a reconcile that never finishes is cut");
        error_policy(the_stack(&state), &err, ctx);
        let event = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let found = calls(&state).into_iter().find(|c| c.is("POST", EVENTS));
                if let Some(event) = found {
                    return event;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the Event is published");
        assert_eq!(event.body["type"], "Warning");
        assert_eq!(event.body["reason"], "ReconcileTimedOut");
        assert_eq!(event.body["regarding"]["kind"], "PlatformStack");
        assert_eq!(event.body["regarding"]["name"], "default");
        let note = event.body["note"].as_str().unwrap();
        assert!(note.contains("did not finish within 120s"), "{note}");
        assert!(
            !calls(&state).iter().any(|c| c.is("PATCH", STATUS)),
            "error_policy writes no status"
        );
    }

    /// A platform MigrationPlan for `from -> to`, approved and executed.
    fn completed_plan(from: &str, to: &str) -> Value {
        let name = synthesize_platform_plan_name(from, to);
        let plan = build_platform_migration_plan_cr(
            &name,
            from,
            to,
            ChangeClass::Breaking,
            Some(to),
            None,
        );
        let mut plan = serde_json::to_value(plan).expect("plan");
        plan["status"] = json!({ "phase": "completed" });
        plan
    }

    /// WI-400 cancellation hazard: an approved upgrade's plan reaches
    /// `completed` and the reconcile takes the bump. The plan GC used to run
    /// with nothing to keep and DELETE that plan before the parent was
    /// patched, so a reconcile cut in between — a hang on the anchor, an
    /// Event or the patch itself, now cut at `RECONCILE_DEADLINE` — left the
    /// parent on the old version and the approval gone: the next pass found
    /// no plan, classified the transition again and gated it behind a fresh
    /// `pending-approval` plan. The operator's approval was lost, and no
    /// reconcile could bring it back.
    #[tokio::test(start_paused = true)]
    async fn a_reconcile_cut_before_the_bump_keeps_the_approval_that_authorises_it() {
        let stack = stack_on("0.2.79", "0.2.80");
        let parent = parent_on(&stack, "0.2.79");
        let mut cluster = cluster(stack, parent);
        cluster.plans.push(completed_plan("0.2.79", "0.2.80"));
        cluster.silent.push(("PATCH", PARENT.to_string()));
        let (client, state) = scripted(cluster);
        let ctx = context(client, Arc::new(NoRegistry));

        let err = operator_core::deadline::within(
            RECONCILE_DEADLINE,
            reconcile(the_stack(&state), ctx.clone()),
        )
        .await
        .expect_err("the bump never answers, so the reconcile is cut");
        assert!(matches!(err, Error::TimedOut(_)), "{err:?}");
        let first = calls(&state);
        assert!(
            first.iter().any(|c| c.is("PATCH", PARENT)),
            "the cut fell on the bump: {first:#?}"
        );
        assert!(
            !first.iter().any(|c| c.method == "DELETE"),
            "the approving plan survives the cut: {first:#?}"
        );

        // The apiserver answers again: the next pass makes the approved bump.
        state.lock().unwrap().silent.clear();
        reconcile(the_stack(&state), ctx)
            .await
            .expect("the next reconcile succeeds");
        let bump = calls(&state)
            .into_iter()
            .rev()
            .find(|c| c.is("PATCH", PARENT))
            .expect("the bump");
        assert_eq!(bump.body["spec"]["source"]["targetRevision"], "0.2.80");
        assert!(
            !calls(&state).iter().any(|c| c.is("POST", PLANS)),
            "no second gate for an approved transition"
        );
    }

    /// The approving plan is kept only while it still authorises something:
    /// once the parent is on the new version the GC collects it, as before.
    #[tokio::test]
    async fn the_approving_plan_is_collected_once_the_bump_has_landed() {
        let stack = stack_on("0.2.80", "0.2.80");
        let parent = parent_on(&stack, "0.2.80");
        let mut cluster = cluster(stack, parent);
        cluster.plans.push(completed_plan("0.2.79", "0.2.80"));
        let (client, state) = scripted(cluster);
        reconcile(the_stack(&state), context(client, Arc::new(NoRegistry)))
            .await
            .expect("the reconcile succeeds");
        let deleted = format!("{PLANS}/platform-0-2-79-to-0-2-80");
        assert!(
            calls(&state).iter().any(|c| c.is("DELETE", &deleted)),
            "{:#?}",
            calls(&state)
        );
    }

    /// The root Application with a foreign field manager on
    /// `spec.source.targetRevision` — a `kubectl edit` to revert.
    fn edited_parent(stack: &Value, target: &str) -> Value {
        let mut parent = parent_on(stack, target);
        parent["metadata"]["managedFields"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "manager": "kubectl-edit", "operation": "Update",
                          "fieldsV1": { "f:spec": { "f:source": { "f:targetRevision": {} } } } }));
        parent
    }

    /// The bodies of the Events the reconcile published, in order.
    fn audit_events(calls: &[Call]) -> Vec<Value> {
        calls
            .iter()
            .filter(|c| c.is("POST", EVENTS))
            .map(|c| c.body.clone())
            .collect()
    }

    /// WI-400 cancellation hazard: the `ForeignFieldManager` Warning goes out
    /// BEFORE the revert patch, and its note used to say the controller
    /// "force-reapplied desired state". A pass cut (or failed) at the patch
    /// left that Warning describing a revert that never happened.
    ///
    /// The Warning must still go out before the patch: it is the only record
    /// of the detection when the revert LANDS but its answer is lost. The
    /// force apply has then already taken the fields from the foreign
    /// manager, so the next pass sees no foreign writer, publishes nothing,
    /// and `UnauthorizedSourceModification` never goes True.
    #[tokio::test(start_paused = true)]
    async fn a_reconcile_cut_at_the_revert_records_the_detection_and_claims_no_revert() {
        let stack = stack_on("0.2.80", "0.2.80");
        let parent = edited_parent(&stack, "0.2.80");
        let mut cluster = cluster(stack, parent);
        cluster.silent.push(("PATCH", PARENT.to_string()));
        let (client, state) = scripted(cluster);
        let ctx = context(client, Arc::new(NoRegistry));
        let err = operator_core::deadline::within(
            RECONCILE_DEADLINE,
            reconcile(the_stack(&state), ctx.clone()),
        )
        .await
        .expect_err("the revert never answers, so the reconcile is cut");
        assert!(matches!(err, Error::TimedOut(_)), "{err:?}");
        let first = calls(&state);
        assert!(first.iter().any(|c| c.is("PATCH", PARENT)), "{first:#?}");
        let events = audit_events(&first);
        assert_eq!(
            events.len(),
            1,
            "the detection and nothing else: {events:#?}"
        );
        assert_eq!(events[0]["type"], "Warning");
        assert_eq!(events[0]["reason"], "ForeignFieldManager");
        let note = events[0]["note"].as_str().expect("note");
        assert!(note.contains("detected external write"), "{note}");
        assert!(
            !note.contains("reverted external write") && !note.contains("force-reapplied"),
            "the detection claims no revert that has not landed: {note}"
        );

        // The revert had landed after all; only its answer was lost. The
        // force apply took the fields, so the foreign manager is gone.
        {
            let mut cluster = state.lock().unwrap();
            cluster.silent.clear();
            cluster.parent["metadata"]["managedFields"]
                .as_array_mut()
                .unwrap()
                .retain(|m| m["manager"] != "kubectl-edit");
        }
        reconcile(the_stack(&state), ctx)
            .await
            .expect("the next reconcile succeeds");
        let all = calls(&state);
        assert_eq!(
            audit_events(&all).len(),
            1,
            "the next pass sees no foreign writer and audits nothing: {all:#?}"
        );
        let patch = status_patch(&all);
        assert_eq!(
            written_condition(&patch, "UnauthorizedSourceModification")["status"],
            "False",
            "the condition never records this write, so the Warning is its only trace"
        );
    }

    /// A revert that lands is audited as detection, revert, completion — in
    /// that order — and the detection says the revert is in progress.
    #[tokio::test]
    async fn a_revert_is_audited_as_detection_then_completion() {
        let stack = stack_on("0.2.80", "0.2.80");
        let parent = edited_parent(&stack, "0.2.80");
        let (client, state) = scripted(cluster(stack, parent));
        reconcile(the_stack(&state), context(client, Arc::new(NoRegistry)))
            .await
            .expect("the reconcile succeeds");
        let calls = calls(&state);
        let event_at = |reason: &str| {
            calls
                .iter()
                .position(|c| c.is("POST", EVENTS) && c.body["reason"] == reason)
                .unwrap_or_else(|| panic!("a {reason} Event: {calls:#?}"))
        };
        let detection = event_at("ForeignFieldManager");
        let completion = event_at("SourceReverted");
        let revert = calls
            .iter()
            .position(|c| c.is("PATCH", PARENT))
            .expect("the revert");
        assert!(
            detection < revert && revert < completion,
            "detection, revert, completion: {calls:#?}"
        );
        assert_eq!(audit_events(&calls).len(), 2, "{calls:#?}");
        let note = calls[detection].body["note"].as_str().expect("note");
        assert!(
            note.contains("is force-reapplying desired state (target=0.2.80)"),
            "{note}"
        );
    }

    fn history_versions(status: &Value) -> Vec<String> {
        status["versionHistory"]
            .as_array()
            .expect("versionHistory")
            .iter()
            .map(|e| e["version"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// WI-400 cancellation hazard: the `versionHistory` entry for a bump was
    /// appended only by the pass that made it, and that pass decides from the
    /// LIVE parent. A pass cut after the bump landed but before its status
    /// write lost the entry for good: the next pass finds the parent already
    /// on the new version and has nothing to append.
    #[tokio::test(start_paused = true)]
    async fn a_bump_cut_before_its_status_write_is_still_recorded_in_the_history() {
        let stack = stack_on("0.2.79", "0.2.80");
        let parent = parent_on(&stack, "0.2.79");
        let mut cluster = cluster(stack, parent);
        cluster.plans.push(completed_plan("0.2.79", "0.2.80"));
        cluster.silent.push(("GET", STATUS.to_string()));
        let (client, state) = scripted(cluster);
        let ctx = context(client, Arc::new(NoRegistry));

        let err = operator_core::deadline::within(
            RECONCILE_DEADLINE,
            reconcile(the_stack(&state), ctx.clone()),
        )
        .await
        .expect_err("the status write never answers, so the reconcile is cut");
        assert!(matches!(err, Error::TimedOut(_)), "{err:?}");
        assert_eq!(
            state.lock().unwrap().parent["spec"]["source"]["targetRevision"],
            "0.2.80",
            "the bump landed"
        );
        assert!(!calls(&state).iter().any(|c| c.is("PATCH", STATUS)));

        // The apiserver answers again: the next pass records the bump.
        state.lock().unwrap().silent.clear();
        reconcile(the_stack(&state), ctx.clone())
            .await
            .expect("the next reconcile succeeds");
        let patch = status_patch(&calls(&state));
        assert_eq!(
            history_versions(&patch.body["status"]),
            vec!["0.2.79".to_string(), "0.2.80".to_string()]
        );
        assert_eq!(patch.body["status"]["currentVersion"], "0.2.80");

        // And only once.
        reconcile(the_stack(&state), ctx)
            .await
            .expect("a settled reconcile succeeds");
        assert_eq!(
            history_versions(&state.lock().unwrap().stack["status"]),
            vec!["0.2.79".to_string(), "0.2.80".to_string()]
        );
    }

    /// `ReconcileStalled` on the stack's status, under its own field manager
    /// (WI-400): set by `reconcile_with_deadline` when a pass is cut, removed
    /// after a pass that finishes, never sent by `platform-controller`, and
    /// written only while the served CRD makes the list a list-map and the
    /// ownership on the stack is one whose transitions kind measured. Run
    /// against the scripted apiserver's list-map model
    /// ([`apply_status_list_map`]).
    mod stall_tests {
        use super::*;
        use crate::stall::{STALL_FIELD_MANAGER, STALL_WRITE_BUDGET};

        const STALL_QUERY: &str = "fieldManager=apprafter-reconcile-deadline";
        const CONTROLLER_QUERY: &str = "fieldManager=platform-controller";
        const STALLED: &str = "ReconcileStalled";

        /// Every condition `stack_on` carries.
        const SETTLED: [&str; 7] = [
            "Ready",
            "Synced",
            "UpstreamReachable",
            "YankedVersion",
            "MigrationPending",
            "UpgradeAvailable",
            "UnauthorizedSourceModification",
        ];

        fn by_key(types: &[&str]) -> Value {
            Value::Object(
                types
                    .iter()
                    .map(|t| (format!(r#"k:{{"type":"{t}"}}"#), json!({ ".": {} })))
                    .collect(),
            )
        }

        fn status_entry(manager: &str, conditions: Value) -> Value {
            json!({ "manager": manager, "operation": "Apply", "subresource": "status",
                    "apiVersion": "apprafter.io/v1alpha1", "fieldsType": "FieldsV1",
                    "fieldsV1": { "f:status": { "f:conditions": conditions } } })
        }

        /// `stack_on`, as written under the list-map CRD:
        /// `platform-controller` owns each of its conditions by key.
        fn listed_stack_on(version: &str, pin: &str) -> Value {
            let mut stack = stack_on(version, pin);
            stack["metadata"]["managedFields"] =
                json!([status_entry(FIELD_MANAGER, by_key(&SETTLED))]);
            stack
        }

        fn listed(stack: Value, parent: Value) -> Cluster {
            let mut cluster = cluster(stack, parent);
            cluster.list_map = true;
            cluster.crd_list_map = true;
            cluster
        }

        fn stalled() -> Value {
            prior_condition(STALLED, "True", "ReconcileTimedOut")
        }

        /// `ReconcileStalled` added to `stack`, owned by key by `owners`;
        /// with none, it is one nobody's apply holds any more.
        fn with_stall(mut stack: Value, owners: &[&str]) -> Value {
            stack["status"]["conditions"]
                .as_array_mut()
                .expect("conditions")
                .push(stalled());
            if !stack["metadata"]["managedFields"].is_array() {
                stack["metadata"]["managedFields"] = json!([]);
            }
            for owner in owners {
                stack["metadata"]["managedFields"]
                    .as_array_mut()
                    .expect("managedFields")
                    .push(status_entry(owner, by_key(&[STALLED])));
            }
            stack
        }

        /// What `platform-controller` owns of the conditions, replaced.
        fn controller_owns(state: &Shared, conditions: Value) {
            let mut cluster = state.lock().unwrap();
            let entry = cluster.stack["metadata"]["managedFields"]
                .as_array_mut()
                .expect("managedFields")
                .iter_mut()
                .find(|e| e["manager"] == FIELD_MANAGER && e["subresource"] == "status")
                .expect("platform-controller's status entry");
            entry["fieldsV1"]["f:status"]["f:conditions"] = conditions;
        }

        fn status_writes(state: &Shared) -> Vec<Call> {
            calls(state)
                .into_iter()
                .filter(|c| c.is("PATCH", STATUS))
                .collect()
        }

        fn condition_types(write: &Call) -> Vec<String> {
            write.body["status"]["conditions"]
                .as_array()
                .map(|cs| {
                    cs.iter()
                        .map(|c| c["type"].as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default()
        }

        fn stored_types(state: &Shared) -> Vec<String> {
            state.lock().unwrap().stack["status"]["conditions"]
                .as_array()
                .map(|cs| {
                    cs.iter()
                        .map(|c| c["type"].as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default()
        }

        /// A stack the controller has written once, under the list-map
        /// model, and the context to run more passes on it.
        async fn settled() -> (Shared, Arc<Context>) {
            let stack = listed_stack_on("0.2.80", "0.2.80");
            let parent = parent_on(&stack, "0.2.80");
            let (client, state) = scripted(listed(stack, parent));
            let ctx = context(client, Arc::new(NoRegistry));
            reconcile(the_stack(&state), ctx.clone())
                .await
                .expect("the settling pass");
            (state, ctx)
        }

        /// `ReconcileStalled` put on the stored stack, owned by `owners`.
        fn stall_into(state: &Shared, owners: &[&str]) {
            let mut cluster = state.lock().unwrap();
            let stack = cluster.stack.take();
            cluster.stack = with_stall(stack, owners);
        }

        /// A cut pass sets `ReconcileStalled=True` in one apply under its own
        /// field manager, carrying that condition and nothing else of the
        /// status, so it can neither re-assert nor prune what
        /// `platform-controller` owns.
        #[tokio::test(start_paused = true)]
        async fn a_cut_pass_sets_reconcile_stalled_under_its_own_field_manager() {
            let stack = listed_stack_on("0.2.80", "0.2.80");
            let parent = parent_on(&stack, "0.2.80");
            let mut cluster = listed(stack, parent);
            cluster.silent.push(("GET", PARENT.to_string()));
            let (client, state) = scripted(cluster);
            let ctx = context(client, Arc::new(NoRegistry));
            let started = tokio::time::Instant::now();
            let err = reconcile_with_deadline(the_stack(&state), ctx)
                .await
                .expect_err("a reconcile that never finishes is cut");
            assert!(matches!(err, Error::TimedOut(_)), "{err:?}");
            assert_eq!(
                started.elapsed(),
                RECONCILE_DEADLINE,
                "the stall write is answered at once"
            );
            let writes = status_writes(&state);
            assert_eq!(writes.len(), 1, "{writes:#?}");
            let write = &writes[0];
            assert!(
                write.uri.contains(STALL_QUERY) && write.uri.contains("force=true"),
                "{}",
                write.uri
            );
            assert_eq!(write.body["kind"], "PlatformStack");
            assert_eq!(write.body["metadata"]["name"], "default");
            assert_eq!(
                write.body["status"].as_object().map(|s| s.len()),
                Some(1),
                "the status carries the conditions alone: {:#}",
                write.body
            );
            assert_eq!(condition_types(write), vec![STALLED]);
            let written = &write.body["status"]["conditions"][0];
            assert_eq!(written["status"], "True");
            assert_eq!(written["reason"], "ReconcileTimedOut");
            let message = written["message"].as_str().expect("message");
            assert!(message.contains("did not finish within 120s"), "{message}");
            let mut kept: Vec<&str> = SETTLED.to_vec();
            kept.push(STALLED);
            assert_eq!(
                stored_types(&state),
                kept,
                "every condition stays beside it"
            );
        }

        /// An apiserver that answers nothing does not answer the stall write
        /// either. That write has its own budget, so the pass gives its slot
        /// back at `RECONCILE_DEADLINE + STALL_WRITE_BUDGET`, not never.
        #[tokio::test(start_paused = true)]
        async fn the_stall_write_has_its_own_budget() {
            let stack: Arc<PlatformStack> = Arc::new(
                serde_json::from_value(listed_stack_on("0.2.80", "0.2.80")).expect("stack"),
            );
            let ctx = context(
                operator_core::testing::stalled_client(),
                Arc::new(NoRegistry),
            );
            let started = tokio::time::Instant::now();
            let outcome = tokio::time::timeout(
                Duration::from_secs(3600),
                reconcile_with_deadline(stack, ctx),
            )
            .await
            .expect("a stall write the apiserver never answers is given up at its budget");
            assert!(matches!(outcome, Err(Error::TimedOut(_))), "{outcome:?}");
            assert_eq!(started.elapsed(), RECONCILE_DEADLINE + STALL_WRITE_BUDGET);
        }

        /// A cut on a stack whose conditions `platform-controller` still
        /// owns whole (last written while the list was atomic: the first
        /// passes after the upgrade) sets `ReconcileStalled` beside them and
        /// leaves that ownership to `platform-controller`'s next apply.
        /// Measured on kind: the apply adds the condition and keeps every
        /// other one, and the condition survives `platform-controller`'s
        /// first apply by key.
        #[tokio::test(start_paused = true)]
        async fn a_cut_on_a_list_the_controller_owns_whole_is_set_beside_it() {
            let mut stack = stack_on("0.2.80", "0.2.80");
            stack["metadata"]["managedFields"] = json!([status_entry(FIELD_MANAGER, json!({}))]);
            let parent = parent_on(&stack, "0.2.80");
            let mut cluster = listed(stack, parent);
            cluster.silent.push(("GET", PARENT.to_string()));
            let (client, state) = scripted(cluster);
            let ctx = context(client, Arc::new(NoRegistry));
            let err = reconcile_with_deadline(the_stack(&state), ctx)
                .await
                .expect_err("a reconcile that never finishes is cut");
            assert!(matches!(err, Error::TimedOut(_)), "{err:?}");
            let writes = status_writes(&state);
            assert_eq!(writes.len(), 1, "{writes:#?}");
            assert!(writes[0].uri.contains(STALL_QUERY), "{}", writes[0].uri);
            assert_eq!(condition_types(&writes[0]), vec![STALLED]);
            let mut kept: Vec<&str> = SETTLED.to_vec();
            kept.push(STALLED);
            assert_eq!(
                stored_types(&state),
                kept,
                "every condition stays beside it"
            );
            assert!(crate::stall::controller_owns_list_whole(&the_stack(&state)));
        }

        /// After a rollback while stalled and a re-upgrade, both managers own
        /// the list whole, and the `ReconcileStalled` the older operator
        /// carried is on the stack. A cut writes nothing then: the stall
        /// manager's first step there, letting go of its whole-list
        /// ownership, was not measured in that state, and the condition on
        /// the stack already says it is stalled. `platform-controller`'s
        /// next apply ends the state.
        #[tokio::test(start_paused = true)]
        async fn a_cut_writes_nothing_while_both_managers_own_a_carried_stall_whole() {
            let mut stack = with_stall(stack_on("0.2.80", "0.2.80"), &[]);
            stack["metadata"]["managedFields"] = json!([
                status_entry(FIELD_MANAGER, json!({})),
                status_entry(STALL_FIELD_MANAGER, json!({})),
            ]);
            let parent = parent_on(&stack, "0.2.80");
            let mut cluster = listed(stack, parent);
            cluster.silent.push(("GET", PARENT.to_string()));
            let (client, state) = scripted(cluster);
            let ctx = context(client, Arc::new(NoRegistry));
            let err = reconcile_with_deadline(the_stack(&state), ctx)
                .await
                .expect_err("a reconcile that never finishes is cut");
            assert!(matches!(err, Error::TimedOut(_)), "{err:?}");
            assert!(status_writes(&state).is_empty(), "{:#?}", calls(&state));
            assert!(stored_types(&state).contains(&STALLED.to_string()));
        }

        /// A rollback re-applies the atomic CRD while this operator can
        /// still hold the lease, and the CRD switch leaves every by-key
        /// `managedFields` entry in place. On the atomic list an apply of
        /// `[ReconcileStalled]` replaces every other condition (measured on
        /// kind), so neither a cut nor a pass that finishes writes anything
        /// under the stall manager: both read the served CRD first.
        #[tokio::test(start_paused = true)]
        async fn nothing_is_written_while_the_served_crd_is_atomic() {
            let (state, ctx) = settled().await;
            {
                let mut cluster = state.lock().unwrap();
                cluster.list_map = false;
                cluster.crd_list_map = false;
                cluster.silent.push(("GET", PARENT.to_string()));
            }
            let before = calls(&state).len();
            let err = reconcile_with_deadline(the_stack(&state), ctx.clone())
                .await
                .expect_err("a reconcile that never finishes is cut");
            assert!(matches!(err, Error::TimedOut(_)), "{err:?}");
            let cut = calls(&state)[before..].to_vec();
            assert!(
                cut.iter().any(|c| c.is("GET", CRD)),
                "mark reads the CRD: {cut:#?}"
            );
            assert!(
                !cut.iter().any(|c| c.is("PATCH", STATUS)),
                "mark writes nothing: {cut:#?}"
            );

            state.lock().unwrap().silent.clear();
            stall_into(&state, &[STALL_FIELD_MANAGER]);
            let before = calls(&state).len();
            reconcile_with_deadline(the_stack(&state), ctx)
                .await
                .expect("the pass finishes");
            let finished = calls(&state)[before..].to_vec();
            assert!(
                finished.iter().any(|c| c.is("GET", CRD)),
                "clear reads the CRD: {finished:#?}"
            );
            assert!(
                !finished.iter().any(|c| c.is("PATCH", STATUS)),
                "clear writes nothing: {finished:#?}"
            );
            assert!(stored_types(&state).contains(&STALLED.to_string()));
        }

        /// A pass that finishes removes `ReconcileStalled` with one empty
        /// apply under the stall manager, and every other condition stays.
        #[tokio::test]
        async fn a_pass_that_finishes_removes_reconcile_stalled_and_nothing_else() {
            let (state, ctx) = settled().await;
            let kept = stored_types(&state);
            stall_into(&state, &[STALL_FIELD_MANAGER]);
            let before = status_writes(&state).len();
            reconcile_with_deadline(the_stack(&state), ctx)
                .await
                .expect("the pass finishes");
            let writes = status_writes(&state)[before..].to_vec();
            assert_eq!(writes.len(), 1, "{writes:#?}");
            assert!(
                writes[0].uri.contains(STALL_QUERY) && writes[0].uri.contains("force=true"),
                "{}",
                writes[0].uri
            );
            assert_eq!(writes[0].body["status"], json!({ "conditions": [] }));
            assert_eq!(stored_types(&state), kept);
        }

        /// One nobody owns any more — what a rollback while stalled leaves
        /// behind — is adopted as it is, then let go, and goes.
        #[tokio::test]
        async fn a_reconcile_stalled_nobody_owns_is_adopted_then_let_go() {
            let (state, ctx) = settled().await;
            let kept = stored_types(&state);
            stall_into(&state, &[]);
            let before = status_writes(&state).len();
            reconcile_with_deadline(the_stack(&state), ctx)
                .await
                .expect("the pass finishes");
            let writes = status_writes(&state)[before..].to_vec();
            assert_eq!(writes.len(), 3, "let go, adopt, let go: {writes:#?}");
            assert!(writes.iter().all(|w| w.uri.contains(STALL_QUERY)));
            assert_eq!(writes[0].body["status"], json!({ "conditions": [] }));
            assert_eq!(
                writes[1].body["status"],
                json!({ "conditions": [stalled()] })
            );
            assert_eq!(writes[2].body["status"], json!({ "conditions": [] }));
            assert_eq!(stored_types(&state), kept);
        }

        /// One another manager owns stays, after a single empty apply: an
        /// adopt beside that owner would remove nothing, and repeated on
        /// every pass would be a write loop.
        #[tokio::test]
        async fn a_reconcile_stalled_another_manager_holds_is_left_in_place() {
            let (state, ctx) = settled().await;
            stall_into(&state, &["kubectl-edit"]);
            let before = status_writes(&state).len();
            reconcile_with_deadline(the_stack(&state), ctx)
                .await
                .expect("the pass finishes");
            let writes = status_writes(&state)[before..].to_vec();
            assert_eq!(writes.len(), 1, "{writes:#?}");
            assert_eq!(writes[0].body["status"], json!({ "conditions": [] }));
            assert!(stored_types(&state).contains(&STALLED.to_string()));
        }

        /// A pass that fails for another reason neither set the stall nor
        /// proved it over: no write under the stall manager.
        #[tokio::test]
        async fn a_pass_that_fails_otherwise_leaves_reconcile_stalled_alone() {
            let stack = with_stall(listed_stack_on("0.2.80", "0.2.80"), &[STALL_FIELD_MANAGER]);
            let (client, state) = scripted(listed(stack, Value::Null));
            let ctx = context(client, Arc::new(NoRegistry));
            let err = reconcile_with_deadline(the_stack(&state), ctx)
                .await
                .expect_err("a parent Application that does not parse fails the pass");
            assert!(!matches!(err, Error::TimedOut(_)), "{err:?}");
            assert!(
                status_writes(&state)
                    .iter()
                    .all(|c| !c.uri.contains(STALL_QUERY)),
                "{:#?}",
                calls(&state)
            );
        }

        /// A settled stack is re-applied once when `platform-controller`
        /// owns its conditions whole (a stack last written while the list
        /// was atomic) or co-owns `ReconcileStalled` (an older operator
        /// copied it into its apply): only its own apply puts either right,
        /// and after it the stall manager can write.
        #[tokio::test]
        async fn a_settled_stack_whose_conditions_are_not_owned_by_key_is_reapplied_once() {
            let mut with_stall_key: Vec<&str> = SETTLED.to_vec();
            with_stall_key.push(STALLED);
            for owned in [json!({}), by_key(&with_stall_key)] {
                let (state, ctx) = settled().await;
                controller_owns(&state, owned.clone());
                let before = status_writes(&state).len();
                reconcile(the_stack(&state), ctx.clone())
                    .await
                    .expect("the re-applying pass");
                let writes = status_writes(&state)[before..].to_vec();
                assert_eq!(writes.len(), 1, "{owned}: {writes:#?}");
                assert!(
                    writes[0].uri.contains(CONTROLLER_QUERY),
                    "{}",
                    writes[0].uri
                );
                assert!(!condition_types(&writes[0]).contains(&STALLED.to_string()));
                assert!(
                    crate::stall::controller_owns_conditions_by_key(&the_stack(&state)),
                    "{owned}"
                );
                reconcile(the_stack(&state), ctx)
                    .await
                    .expect("a settled pass");
                assert_eq!(status_writes(&state).len(), before + 1, "{owned}: once");
            }
        }

        /// The live upgrade end to end: a stall on a stack whose conditions
        /// `platform-controller` still owns whole stays through the pass that
        /// re-applies them (that pass read the old ownership), and goes with
        /// the next one.
        #[tokio::test]
        async fn a_stall_on_a_list_owned_whole_goes_once_the_controller_has_reapplied() {
            let (state, ctx) = settled().await;
            controller_owns(&state, json!({}));
            stall_into(&state, &[STALL_FIELD_MANAGER]);
            let before = status_writes(&state).len();
            reconcile_with_deadline(the_stack(&state), ctx.clone())
                .await
                .expect("the re-applying pass");
            let first = status_writes(&state)[before..].to_vec();
            assert_eq!(first.len(), 1, "{first:#?}");
            assert!(first[0].uri.contains(CONTROLLER_QUERY), "{}", first[0].uri);
            assert!(stored_types(&state).contains(&STALLED.to_string()));
            reconcile_with_deadline(the_stack(&state), ctx.clone())
                .await
                .expect("the next pass");
            let second = status_writes(&state)[before + 1..].to_vec();
            assert_eq!(second.len(), 1, "{second:#?}");
            assert!(second[0].uri.contains(STALL_QUERY), "{}", second[0].uri);
            assert!(!stored_types(&state).contains(&STALLED.to_string()));
            reconcile_with_deadline(the_stack(&state), ctx)
                .await
                .expect("a settled pass");
            assert_eq!(status_writes(&state).len(), before + 2);
        }

        /// `platform-controller` clones the status it read, `ReconcileStalled`
        /// included, and must not send it: co-owning it would keep it on the
        /// stack after the stall manager lets it go.
        #[tokio::test]
        async fn the_controllers_status_write_never_carries_reconcile_stalled() {
            let mut stack = with_stall(listed_stack_on("0.2.80", "0.2.80"), &[STALL_FIELD_MANAGER]);
            stack["status"]["lastUpstreamCheck"] = Value::Null;
            let parent = parent_on(&stack, "0.2.80");
            let (client, state) = scripted(listed(stack, parent));
            let upstream = Arc::new(Published {
                latest: "0.2.81",
                class: ChangeClass::Safe,
            });
            reconcile(the_stack(&state), context(client, upstream))
                .await
                .expect("the reconcile succeeds");
            let patch = status_patch(&calls(&state));
            assert!(
                !condition_types(&patch).contains(&STALLED.to_string()),
                "{:#}",
                patch.body
            );
            assert_every_condition_carried(&patch);
            assert!(stored_types(&state).contains(&STALLED.to_string()));
        }

        /// A settled stack is not rewritten, and the stall manager's
        /// condition appearing on it is no reason to rewrite it either.
        #[tokio::test]
        async fn reconcile_stalled_alone_is_no_reason_for_the_controller_to_write() {
            let (state, ctx) = settled().await;
            let settled_writes = status_writes(&state).len();
            reconcile(the_stack(&state), ctx.clone())
                .await
                .expect("a settled pass");
            assert_eq!(status_writes(&state).len(), settled_writes);
            stall_into(&state, &[STALL_FIELD_MANAGER]);
            reconcile(the_stack(&state), ctx)
                .await
                .expect("a pass that reads the stall");
            assert_eq!(
                status_writes(&state).len(),
                settled_writes,
                "ReconcileStalled alone is no reason to write"
            );
        }

        /// A duplicate condition type read back (possible on a stack written
        /// while the list was atomic) is written away, once, by a pass that
        /// would otherwise write nothing: the list-map apply that carried it
        /// would be refused on every pass.
        #[tokio::test]
        async fn a_duplicate_condition_type_is_written_away() {
            let (state, ctx) = settled().await;
            state.lock().unwrap().stack["status"]["conditions"]
                .as_array_mut()
                .expect("conditions")
                .push(prior_condition("Ready", "False", "Degraded"));
            let before = status_writes(&state).len();
            reconcile(the_stack(&state), ctx.clone())
                .await
                .expect("the reconcile succeeds");
            let writes = status_writes(&state)[before..].to_vec();
            assert_eq!(writes.len(), 1, "{writes:#?}");
            let types = condition_types(&writes[0]);
            assert_eq!(
                types.iter().filter(|t| *t == "Ready").count(),
                1,
                "{types:?}"
            );
            assert_every_condition_carried(&writes[0]);
            let ready = stored_types(&state)
                .into_iter()
                .filter(|t| t == "Ready")
                .count();
            assert_eq!(ready, 1);
            reconcile(the_stack(&state), ctx)
                .await
                .expect("a settled pass");
            assert_eq!(status_writes(&state).len(), before + 1, "once");
        }

        /// The first write after the upgrade, on a list `platform-controller`
        /// owns whole, can be one that retires a condition: here
        /// `BackupHealthy`, with backups off. Applied straight from the
        /// whole-list ownership, that apply would keep it, owned by nobody
        /// (measured on kind), and every later pass would re-apply in vain.
        /// The list is first re-applied as it was read, which takes it by
        /// key, and the write after it removes the condition.
        #[tokio::test]
        async fn a_condition_retired_by_the_first_write_after_the_upgrade_is_removed() {
            let mut stack = stack_on("0.2.80", "0.2.80");
            stack["status"]["conditions"]
                .as_array_mut()
                .expect("conditions")
                .push(prior_condition("BackupHealthy", "True", "Succeeded"));
            stack["metadata"]["managedFields"] = json!([status_entry(FIELD_MANAGER, json!({}))]);
            assert!(stack["spec"]["backup"].is_null(), "backups are off");
            let parent = parent_on(&stack, "0.2.80");
            let (client, state) = scripted(listed(stack, parent));
            let ctx = context(client, Arc::new(NoRegistry));
            reconcile(the_stack(&state), ctx.clone())
                .await
                .expect("the first pass after the upgrade");
            let writes = status_writes(&state);
            assert_eq!(writes.len(), 2, "{writes:#?}");
            assert!(
                writes.iter().all(|w| w.uri.contains(CONTROLLER_QUERY)),
                "{writes:#?}"
            );
            let backup = "BackupHealthy".to_string();
            assert!(
                condition_types(&writes[0]).contains(&backup),
                "the list as read"
            );
            assert!(!condition_types(&writes[1]).contains(&backup));
            let stored = stored_types(&state);
            assert!(!stored.contains(&backup), "{stored:?}");
            for t in SETTLED {
                assert!(stored.contains(&t.to_string()), "{t}: {stored:?}");
            }
            reconcile(the_stack(&state), ctx)
                .await
                .expect("a settled pass");
            assert_eq!(
                status_writes(&state).len(),
                2,
                "the next pass writes nothing"
            );
        }

        /// An older operator still running under the list-map CRD (between
        /// the CRD's sync wave and its own pod's replacement) applies by key
        /// from its whole-list ownership, so a condition it left out stays,
        /// owned by nobody, while `platform-controller` owns the rest by key.
        /// The first write that leaves it out too re-applies the status as
        /// read first, which adopts it, and the write after it removes it.
        #[tokio::test]
        async fn a_condition_nobody_holds_is_adopted_before_the_write_that_retires_it() {
            let mut stack = listed_stack_on("0.2.80", "0.2.80");
            stack["status"]["conditions"]
                .as_array_mut()
                .expect("conditions")
                .push(prior_condition("BackupHealthy", "True", "Succeeded"));
            let parent = parent_on(&stack, "0.2.80");
            let (client, state) = scripted(listed(stack, parent));
            let ctx = context(client, Arc::new(NoRegistry));
            reconcile(the_stack(&state), ctx.clone())
                .await
                .expect("the pass");
            let writes = status_writes(&state);
            assert_eq!(writes.len(), 2, "{writes:#?}");
            let backup = "BackupHealthy".to_string();
            assert!(
                condition_types(&writes[0]).contains(&backup),
                "the list as read"
            );
            assert!(!condition_types(&writes[1]).contains(&backup));
            assert!(!stored_types(&state).contains(&backup));
            reconcile(the_stack(&state), ctx)
                .await
                .expect("a settled pass");
            assert_eq!(
                status_writes(&state).len(),
                2,
                "the next pass writes nothing"
            );
        }
    }
}
