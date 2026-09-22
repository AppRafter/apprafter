// SPDX-License-Identifier: FSL-1.1-Apache-2.0

package platformstack

// Argo CD's own Application. Self-managing: Argo CD reconciles
// its own chart definition, prune=false so a stale chart can't
// delete the controllers responsible for re-installing it (a
// foot-gun the v0.1.x bootstrap already avoids by installing
// Argo CD via direct `helm upgrade --install` rather than
// through Argo CD itself; here we make the self-management
// explicit via `prune: false`).
//
// Pinned to v7.7.7 — same as v0.1.x cluster-bootstrap.
_components: argocd: #Component & {
	name:      "argocd"
	enabled:   bool | *true
	namespace: "argocd"
	source: {
		repoURL: "https://argoproj.github.io/argo-helm"
		chart:   "argo-cd"
	}
	// version: set by B.1.71b invariant in loader_values.cue
	// (`_components.argocd.version: _loaderValues.argocd.chartVersion`)

	// `values:` is `_loaderValues.argocd.values` unified with the
	// chart-only adopt-time extras below. The loader-side
	// subset (replicas, dex/redis-ha/notifications off, OCI
	// repo registration, default AppProject) lives in
	// `loader_values.cue` so the CLI's `build.rs` can lift it
	// out as a `const &str` for `cluster-bootstrap`. The
	// extras (cue-cmp sidecar + its ConfigMap) only matter
	// once Argo CD is up, so they're chart-only.
	//
	// Single replicas on tier-1 (cpx22 RAM budget). Tier 2+
	// overlays scale these up; Dex stays off until OIDC SSO
	// lands in Phase 3.
	//
	// `redis-ha.enabled: false` is critical on single-node
	// k3s — the upstream chart's redis-ha StatefulSet sets
	// `requiredDuringSchedulingIgnoredDuringExecution`
	// podAntiAffinity across 3 redis pods, which never
	// schedule on one node.
	//
	// OCI Helm repository registration: without this the
	// `argocd-repo-server` shells out to
	// `helm pull --repo oci://ghcr.io/apprafter <chart>`,
	// which is malformed for OCI registries — `helm pull`
	// for OCI requires `helm pull oci://<repo>/<chart>` form.
	// Argo CD bridges that by reading `enableOCI: "true"`
	// off this registration and rewriting the pull command.
	// URL is BARE (no `oci://` scheme); Argo CD adds the
	// scheme based on `enableOCI`.
	//
	// Argo CD chart 7.7.7 does NOT auto-create the `default`
	// AppProject, and Argo CD 2.13.1 server does NOT recreate
	// it on startup either. Without the `configs.projects.default`
	// block, every Application with `project: default` (incl.
	// the root `platform` Application the CLI loader applies)
	// fails with `Application referencing project default
	// which does not exist`. Walk-found bug v0.1.103 → v0.1.104.
	values: _loaderValues.argocd.values & {
		// 2.16d resource requests/limits (measured RSS×0.8 request /
		// tight mem limit / modest cpu request / no cpu limit — see
		// docs/measurements/2.16d-baseline-*.md). The argo-cd chart
		// (7.7.7) splits every workload under its own key: the
		// application-controller is a StatefulSet (`controller.resources`),
		// the repo-server carries its below (with the cue-cmp sidecar), and
		// server / applicationSet / redis each take a `resources` key.
		// No argo pod stays BestEffort.
		controller: resources: {
			requests: {
				cpu:    "50m"
				memory: "288Mi"
			}
			limits: memory: "512Mi"
		}
		// 2.16f: force Go to GC harder + release freed heap back to the OS.
		// The controller forks nothing, so 256/512 = 50% of the cgroup is
		// Go's alone. GOMEMLIMIT accepts only B/KiB/MiB/GiB/TiB - "256Mi"
		// (k8s spelling) or "256MB" (SI) fatal-error the runtime at startup.
		controller: env: [
			{name: "GOMEMLIMIT", value: "256MiB"},
			{name: "GOGC", value: "50"},
		]
		server: resources: {
			requests: memory: "24Mi"
			limits: memory:   "128Mi"
		}
		applicationSet: resources: {
			requests: memory: "24Mi"
			limits: memory:   "128Mi"
		}
		// 2.16f: 7.7.7 has no `enabled` gate; replicas:0 keeps the object in
		// desired under prune:false (no orphan / no permanent OutOfSync). The
		// applicationSet.resources block above stays inert for a future
		// multi-env re-enable (a one-line flip that keeps the no-BestEffort
		// invariant). Verified unused: AppProjects (not ApplicationSets)
		// provide the platform's grouping.
		applicationSet: replicas: 0
		redis: resources: {
			requests: memory: "16Mi"
			limits: memory:   "64Mi"
		}
		// Custom resource-health Lua scripts merged into
		// `argocd-cm`. Walk-fix B.1.77: surface the
		// Application CR's `AwaitingMigrationApproval` phase
		// (spec.md §3.8 + ADR 0027) in the Argo CD UI as
		// `Degraded` with the MigrationPlan name. Without
		// this, Argo CD treats every CR without a built-in
		// health check as `Progressing` indefinitely and the
		// operator never notices the pause from the UI.
		//
		// Argo CD chart 7.7.7 path: `configs.cm.<key>`. Key
		// shape `resource.customizations.health.<group>_<Kind>`
		// is Argo CD's documented schema for custom health
		// scripts.
		configs: cm: {
			// 2.16f: stop the app-controller tracking churny non-app
			// resources - shrinks the live-resource-cache + kills their
			// CPU diffs. Kind-scoped (never `*` on cilium.io -> keeps the
			// operator's per-app egress CiliumNetworkPolicy); never the
			// VPA CR itself (an app tree-child carrying the 2.16e
			// recommendation signal). Argo CD 2.13.1 ships no built-in
			// exclusions -> the savings are additive.
			"resource.exclusions": """
				- apiGroups: [""]
				  kinds: ["Endpoints", "Event"]
				- apiGroups: ["events.k8s.io"]
				  kinds: ["Event"]
				- apiGroups: ["discovery.k8s.io"]
				  kinds: ["EndpointSlice"]
				- apiGroups: ["coordination.k8s.io"]
				  kinds: ["Lease"]
				- apiGroups: ["metrics.k8s.io"]
				  kinds: ["*"]
				- apiGroups: ["cilium.io"]
				  kinds: ["CiliumIdentity", "CiliumEndpoint"]
				- apiGroups: ["autoscaling.k8s.io"]
				  kinds: ["VerticalPodAutoscalerCheckpoint"]
				"""

			// ADR 0059: the pinned branch is keyed on the pin
			// marker ALONE and sits ABOVE both `Ready` and the
			// `Progressing` fall-through -- not nested under a
			// phase check. Nested, the tile would read
			// `Progressing` exactly while a rollback rolls pods,
			// which is the one moment someone is looking at it.
			//
			// It sits BELOW `AwaitingMigrationApproval` on
			// purpose: that state needs an operator action, a pin
			// is a deliberate hold, and the more urgent one wins
			// when an app is somehow both.
			//
			// `Suspended`, never `Degraded` -- a pinned app is held,
			// not broken, and `Degraded` would trip health-gated
			// automation and alerting. Two limits worth knowing,
			// both from gitops-engine at the shipped version:
			// `Suspended` is the SECOND-healthiest code, so it
			// overrides `Healthy` and nothing else -- the pin is
			// invisible on the tile whenever a sibling managed
			// resource is `Progressing`, including a repository
			// that renders several apps into one Argo Application.
			// And a permanently-`Suspended` managed resource hangs
			// a sync operation whose task set spans more than one
			// wave or phase; the shipped app shape (one CR, no
			// waves, no hooks, no managedNamespaceMetadata) does
			// not, and that is an invariant to keep.
			// 0.2.77: the fall-through below used to catch THREE
			// held phases and report all of them as `Progressing /
			// "Awaiting controller reconcile"`. The operator writes
			// five phases; this script named two. `AwaitingResourceClaim`,
			// `EnvSecretMissing` and `InvalidEffectiveSpec` all landed on
			// a message saying the controller had not reconciled yet —
			// when it had, repeatedly, and was deliberately holding with a
			// `Ready=False` condition whose `reason` IS the phase and
			// whose `message` names the missing claim, the missing env
			// var or the renderer's diagnostic. The two terminal ones
			// never clear on their own, so the tile read "in flight"
			// forever for a state that needed a person.
			//
			// This is the `argoproj.io_Application` finding of 0.2.75 one
			// layer down, and it costs more since that release: a
			// `Progressing` child now HOLDS every later wave instead of
			// being stepped over.
			//
			// So the fall-through now reads the condition the operator
			// already writes. The split is by whether the state clears
			// ITSELF: `ResourceClaimPending` does (a claim provisions in
			// a minute or two) and stays `Progressing`; everything else
			// `False` is `Degraded`, which is fail-CLOSED on purpose — a
			// phase added later and forgotten here shows up as noise
			// rather than disappearing into "in flight", which is exactly
			// the failure being fixed.
			//
			// The bare `Progressing` remains for a CR with no status at
			// all: freshly applied, not yet reconciled. There the message
			// is true.
			"resource.customizations.health.apprafter.io_Application": """
				hs = {}
				if obj.status ~= nil and obj.status.phase ~= nil then
				  if obj.status.phase == "AwaitingMigrationApproval" then
				    hs.status = "Degraded"
				    hs.message = "Application paused; awaiting MigrationPlan approval"
				    if obj.status.conditions ~= nil then
				      for _, c in ipairs(obj.status.conditions) do
				        if c.type == "MigrationPending" then
				          hs.message = c.message or hs.message
				          break
				        end
				      end
				    end
				    return hs
				  end
				end
				if obj.status ~= nil and obj.status.image ~= nil and obj.status.image.pinned ~= nil then
				  local ref = obj.status.image.pinned.resolved
				  if ref ~= nil then
				    hs.status = "Suspended"
				    local tag = "its tag"
				    if obj.status.image.tag ~= nil then
				      tag = obj.status.image.tag
				    end
				    hs.message = "Pinned to " .. ref .. " by rollback; not following " .. tag
				    return hs
				  end
				end
				if obj.status ~= nil and obj.status.phase == "Ready" then
				  hs.status = "Healthy"
				  hs.message = "Reconcile complete"
				  return hs
				end
				if obj.status ~= nil and obj.status.conditions ~= nil then
				  for _, c in ipairs(obj.status.conditions) do
				    if c.type == "Ready" and c.status == "False" then
				      if c.reason == "ResourceClaimPending" then
				        hs.status = "Progressing"
				      else
				        hs.status = "Degraded"
				      end
				      hs.message = c.message or c.reason or "held by the controller"
				      return hs
				    end
				  end
				end
				hs.status = "Progressing"
				hs.message = "Awaiting controller reconcile"
				return hs
				"""

			// 0.2.77: `SharedDatabase` and `SharedVolume` had NO health
			// assessment, and gitops-engine counts a resource without one
			// as finished the moment its apply returns. So a database
			// that never provisioned — a `vector` the operand image does
			// not carry, no matching ServiceProvider, a Postgres cluster
			// that never answered — sat on the tile as `Healthy` while
			// every application bound to it waited on a claim that could
			// not bind. The one resource that could name the problem was
			// the one reporting success.
			//
			// Same split as the Application script above, applied to the
			// reason vocabulary these two controllers actually write:
			// `AwaitingCluster` / `AwaitingDatabase` clear by themselves
			// (their own constants say so) and read `Progressing`;
			// `ExtensionUnavailable`, `NotInOperandImage`, `NoProvider`,
			// `UnsupportedType`, `InsufficientCapacity` and `InUse` need
			// a person and read `Degraded`. Matching on the `Awaiting`
			// PREFIX rather than on a list keeps a future
			// `Awaiting<something>` on the self-clearing side, which is
			// the direction that stays quiet when it guesses wrong.
			//
			// `CapacityWarning` is a separate condition type and is not
			// read here: a volume at 85% is a warning about the future,
			// not a statement that the volume is unusable now.
			"resource.customizations.health.apprafter.io_SharedDatabase": """
				hs = {}
				if obj.status ~= nil and obj.status.ready == true then
				  hs.status = "Healthy"
				  local backing = obj.status.database or obj.status.instance
				  if backing ~= nil then
				    hs.message = "Ready (" .. tostring(backing) .. ")"
				  else
				    hs.message = "Ready"
				  end
				  return hs
				end
				if obj.status ~= nil and obj.status.conditions ~= nil then
				  for _, c in ipairs(obj.status.conditions) do
				    if c.type == "Ready" and c.status == "False" then
				      if c.reason ~= nil and string.sub(c.reason, 1, 8) == "Awaiting" then
				        hs.status = "Progressing"
				      else
				        hs.status = "Degraded"
				      end
				      hs.message = c.message or c.reason or "not ready"
				      return hs
				    end
				  end
				end
				hs.status = "Progressing"
				hs.message = "Awaiting provisioning"
				return hs
				"""

			"resource.customizations.health.apprafter.io_SharedVolume": """
				hs = {}
				if obj.status ~= nil and obj.status.ready == true then
				  hs.status = "Healthy"
				  if obj.status.pvcRef ~= nil then
				    hs.message = "Ready (" .. tostring(obj.status.pvcRef) .. ")"
				  else
				    hs.message = "Ready"
				  end
				  return hs
				end
				if obj.status ~= nil and obj.status.conditions ~= nil then
				  for _, c in ipairs(obj.status.conditions) do
				    if c.type == "Ready" and c.status == "False" then
				      if c.reason ~= nil and string.sub(c.reason, 1, 8) == "Awaiting" then
				        hs.status = "Progressing"
				      else
				        hs.status = "Degraded"
				      end
				      hs.message = c.message or c.reason or "not ready"
				      return hs
				    end
				  end
				end
				hs.status = "Progressing"
				hs.message = "Awaiting provisioning"
				return hs
				"""

			// 2.13/argo-upgrade-approval-surface: custom health
			// for the MigrationPlan CR. The platform-stack root
			// Application anchors the MigrationPlan into its
			// resource tree (ownerRef to the chart's
			// `platform-migration-anchor` ConfigMap), so this
			// health LABELS that tree node with the upgrade
			// details: it drives the in-tree row state and the
			// Approve-button discovery. It does NOT bubble up to
			// the root Application -- Argo CD aggregates an App's
			// health from its MANAGED resource set
			// (`status.resources`), not from arbitrary
			// ownerReference tree children, and the anchored
			// MigrationPlan is a live tree node but is not managed
			// (the live walk confirmed a Suspended plan leaves the
			// root App Healthy). The ROOT-level "an update is
			// pending" signal comes solely from the
			// `argoproj.io_Application` banner below, which reads
			// the operator's `apprafter.io/upgrade-*` annotations.
			//
			// Reads `spec.trigger.{from,to}`,
			// `spec.risks.classification`, `status.phase` — all
			// fields the operator already populates. `->` (not the
			// unicode arrow) avoids encoding/lint surprises in
			// argocd-cm.
			// 2.16b-sec (ADR 0052): keep the single-trigger headline
			// (`from->to (class)`) and APPEND a security-axis drill-in
			// so an approver sees the FULL blast radius, not just the
			// `pick_primary` headline (kills approve-laundering): the
			// `spec.risks.classifications[]` rollup as badges + a
			// per-change list from `spec.changes[]`. Every table access
			// is nil- + `type(x)=="table"`-guarded and every value is
			// `tostring`-coerced, so a legacy plan with no rollup
			// fields falls back cleanly to the headline-only message.
			"resource.customizations.health.apprafter.io_MigrationPlan": """
				hs = {}
				local phase = ""
				if obj.status ~= nil and obj.status.phase ~= nil then phase = obj.status.phase end
				local from, to, class = "?", "?", "?"
				local detail = ""
				if obj.spec ~= nil then
				  if obj.spec.trigger ~= nil then
				    from = tostring(obj.spec.trigger.from or from)
				    to   = tostring(obj.spec.trigger.to or to)
				  end
				  if obj.spec.risks ~= nil and obj.spec.risks.classification ~= nil then
				    class = tostring(obj.spec.risks.classification)
				  end
				  -- 2.16b-sec (ADR 0052): classifications[] rollup rendered as badges.
				  if obj.spec.risks ~= nil and type(obj.spec.risks.classifications) == "table" then
				    local badges = ""
				    for _, c in ipairs(obj.spec.risks.classifications) do
				      badges = badges .. "[" .. tostring(c or "?") .. "]"
				    end
				    if badges ~= "" then detail = detail .. " risks: " .. badges end
				  end
				  -- changes[] drill-in: per-change type/field/class + from->to. The wire
				  -- field for the trigger kind is `.type` (MigrationChange.trigger is
				  -- #[serde(rename="type")]), NOT `.trigger`. All values string-coerced.
				  if type(obj.spec.changes) == "table" then
				    local n = 0
				    for _, ch in ipairs(obj.spec.changes) do
				      if type(ch) == "table" then
				        n = n + 1
				        detail = detail .. " * " .. tostring(ch.type or "?") .. " " .. tostring(ch.field or "?") .. " (" .. tostring(ch.classification or "?") .. "): " .. tostring(ch.from or "?") .. "->" .. tostring(ch.to or "?")
				      end
				    end
				    if n > 0 then detail = " |" .. tostring(n) .. " change(s):" .. detail end
				  end
				end
				if phase == "pending-approval" or phase == "" then
				  hs.status = "Suspended"
				  hs.message = "Upgrade " .. from .. "->" .. to .. " (" .. class .. ") awaiting approval - click Approve, or run 'apprafter migration approve " .. (obj.metadata.name or "<name>") .. "'" .. detail
				  return hs
				end
				if phase == "approved" or phase == "executing" then
				  hs.status = "Progressing"
				  hs.message = "Upgrade " .. from .. "->" .. to .. " approved; applying" .. detail
				  return hs
				end
				if phase == "completed" then
				  hs.status = "Healthy"
				  hs.message = "Upgrade " .. from .. "->" .. to .. " complete"
				  return hs
				end
				if phase == "rejected" then
				  hs.status = "Degraded"
				  hs.message = "Upgrade " .. from .. "->" .. to .. " rejected"
				  return hs
				end
				hs.status = "Progressing"
				hs.message = "MigrationPlan phase: " .. phase
				return hs
				"""

			// ADR 0048 (revised — kind+Argo-validated, 2026-06-12):
			// surface a pending platform upgrade on the ROOT platform
			// App's own TILE. The prior approach — a custom health on
			// `argoproj.io_Application` reading an annotation on the
			// root App — was EMPIRICALLY DISPROVEN: Argo applies that
			// customization only to Application resources appearing as
			// CHILDREN in another app's tree, never to a top-level
			// app's OWN tile (whose health is the worst-of aggregate of
			// its managed `.status.resources`), so the root App stayed
			// Healthy despite the annotation.
			//
			// Validated fix: the operator stamps
			// `apprafter.io/upgrade-pending=true` (+ from/to/class/plan)
			// on the chart-MANAGED `platform-migration-anchor`
			// ConfigMap — it IS in the root App's `.status.resources`,
			// so its custom health aggregates into the root tile. This
			// `ConfigMap` health returns Suspended for it → the root App
			// tile rolls up to Suspended (purple "pause/attention", not
			// red "broken") in the Applications LIST, nudging the
			// operator to open + Approve. Confirmed live: the operator's
			// SSA annotation survives Argo syncs and causes no OutOfSync
			// (no ignoreDifferences needed); the SET→CLEAR cycle is clean.
			//
			// CRITICAL: this runs for EVERY ConfigMap cluster-wide.
			// ConfigMaps carry no built-in health; the else-branch
			// returns Healthy (a ConfigMap is inert data) — Healthy is
			// the BEST status, so it never worsens any other app's
			// aggregate. Only the operator-stamped anchor goes Suspended.
			// The key is `ConfigMap` (core/empty group → NO leading
			// underscore; `_ConfigMap` silently yields nil — verified on
			// a live Argo). `->` is ASCII (not the unicode arrow).
			"resource.customizations.health.ConfigMap": """
				hs = {}
				local a = nil
				if obj.metadata ~= nil then a = obj.metadata.annotations end
				if a ~= nil and a["apprafter.io/upgrade-pending"] == "true" then
				  hs.status = "Suspended"
				  hs.message = "platform update " .. (a["apprafter.io/upgrade-from"] or "?") .. "->" .. (a["apprafter.io/upgrade-to"] or "?") .. " pending approval (" .. (a["apprafter.io/upgrade-class"] or "?") .. ") - open this app and Approve the MigrationPlan, or run 'apprafter migration approve " .. (a["apprafter.io/upgrade-plan"] or "<plan>") .. "'"
				  return hs
				end
				hs.status = "Healthy"
				return hs
				"""

			// B.1.79: Argo CD resource action buttons for
			// MigrationPlan. Operators can `Approve` / `Reject`
			// directly from the Argo CD UI alongside the CLI
			// path (`apprafter migration approve <name>`). Argo
			// CD merges the returned object back via the
			// apiserver; status.phase mutations route through
			// the status subresource automatically.
			//
			// Reject for application-scope plans is denied by
			// the admission webhook per ADR 0027; the apiserver
			// denial bubbles up to the UI with the verbatim
			// webhook message. Discovery disables BOTH actions
			// once the plan leaves `pending-approval` so stale
			// buttons cannot double-fire.
			"resource.customizations.actions.apprafter.io_MigrationPlan": """
				discovery.lua: |
				  actions = {}
				  local phase = ""
				  if obj.status ~= nil and obj.status.phase ~= nil then
				    phase = obj.status.phase
				  end
				  local decidable = phase == "" or phase == "pending-approval"
				  actions["approve"] = {["disabled"] = not decidable}
				  actions["reject"]  = {["disabled"] = not decidable}
				  return actions
				definitions:
				- name: approve
				  action.lua: |
				    if obj.status == nil then obj.status = {} end
				    obj.status.phase = "approved"
				    return obj
				- name: reject
				  action.lua: |
				    if obj.status == nil then obj.status = {} end
				    obj.status.phase = "rejected"
				    return obj
				"""
		}

		// argocd-repo-server runs the cue-cmp sidecar that
		// renders user app repositories' `apprafter*.cue`
		// files into Kubernetes YAML at sync time (ADR 0029).
		// Image tag is pulled from `_components.argocd-cue-cmp`
		// so a chart-level bump of the cue-cmp version is a
		// one-line edit in that file alone.
		//
		// Volumes layout matches Argo CD's CMP sidecar
		// contract: `var-files` is the shared sandbox where
		// repo-server mounts the user repo for the sidecar
		// to read, `cmp-tmp` is per-render scratch. The
		// sidecar runs as UID 999 (same as the upstream
		// argocd-repo-server image) so file ownership lines
		// up across containers.
		//
		// `repoServer.replicas` already comes from
		// `_loaderValues.argocd` above; only the chart-only
		// extras live here.
		//
		// Bound the repo-server's cold-start render fan-out. Chart
		// 7.7.7 defaults `reposerver.parallelism.limit` to 0, which
		// means no limit. After a cold start (node reboot, Argo CD
		// upgrade, loss of the Redis manifest cache) the controller
		// does not re-render Applications whose `reconciledAt` is
		// fresh; it re-compares all of them together when the 180s
		// `timeout.reconciliation` fires. At 0 the repo-server then
		// runs every Application's work at once: 7-9 `helm pull` /
		// `helm template` children (11-25Mi anon each, cilium the
		// largest) plus a full-history `git fetch` per git source
		// (Argo CD 2.13 has no depth option; the platform monorepo's
		// `git index-pack` alone is ~59Mi). That peak grows with every
		// chart or git Application added.
		//
		// Measured 2026-09-22 on disk-backed kind (k8s 1.36.4, Argo CD
		// 2.13.1, 13 Applications including a Cilium 1.16.5 render),
		// repo-server anon peak on a full cold start, limit lifted to
		// 2Gi so no run was cut short:
		//   0 (chart default)  127-158Mi   7-9 helm children at once
		//   4                   90-101Mi
		//   2                   67-76Mi    +2-3s to every app compared
		//   1                   67-90Mi    no lower, +10-13s
		// 1 buys nothing: one `git index-pack` plus the ~30Mi Go process
		// is the floor. The semaphore (runRepoOperation) covers helm
		// chart extraction/template and the git checkout, so at 2 the
		// peak is the two heaviest operations plus the Go process.
		// Re-run on kind with the 11 platform Applications, one cluster,
		// 0.2.79 values then these: parallelism 0 / 256Mi peaked at
		// 180Mi anon with 9 helm children at once (the limit sweep below
		// saw 192Mi OOM-kill 1 run in 2); parallelism 2 / 384Mi peaked at 92 and
		// 98Mi (the argo-cd + cilium chart pulls; the monorepo
		// index-pack), never more than 2 helm children, no OOM.
		//
		// What to watch: queued work waits inside the controller's 60s
		// repo-server RPC deadline (`controller.repo.server.timeout.seconds`).
		// A cluster with dozens of Applications, or slow git sources
		// holding both slots, would show ComparisonError retries and a
		// high `argocd_repo_pending_request_total`; 3-4 is the next step.
		// The same coupling in an OUTAGE: helm and git children run
		// without the request's context, bounded only by
		// ARGOCD_EXEC_TIMEOUT (90s), so a stalled registry or git host
		// (ghcr.io serves the platform, operator, webhook and dragonfly
		// charts) can hold both slots after its callers gave up, and
		// Applications unrelated to it then ComparisonError until it
		// recovers. At the old unlimited setting that coupling did not
		// exist; it is the price of bounding the peak.
		//
		// Value type: the chart renders every `configs.params` value
		// with `toString`, so this int lands as the string "2" the
		// ConfigMap needs (the chart's own default is the int 0).
		//
		// ROLLOUT: every `configs.params` key feeds the chart's
		// `checksum/cmd-params` pod annotation on the controller,
		// server and repo-server (and the applicationset / dex
		// templates), so changing ANY key here restarts every Argo CD
		// pod except Redis at once. That is intended, and cheaper than
		// it sounds: the restarted controller re-compares everything
		// at the next 180s tick, and with Redis kept those compares are
		// answered from its manifest cache (measured on kind, 0.2.79 ->
		// this change: no helm or git child at all, 44Mi anon). What
		// the restart does render (an Application whose source moved in
		// the same release) renders under the new settings, because
		// the replacement repo-server reads the new ConfigMap and gets
		// the limit below in the same rollout. A full cold render needs
		// the Redis cache gone as well (node reboot, Redis restart).
		configs: params: "reposerver.parallelism.limit": 2

		repoServer: {
			// Two different numbers, measured two different ways.
			//
			// REQUEST 66Mi is the WORN-IN steady state (2.16d: 82Mi
			// working set x 0.8; 2.16f saw 42-114Mi settled). Scheduling
			// is unchanged.
			//
			// LIMIT has to cover the COLD full render, which the worn-in
			// number says nothing about. Measured 2026-09-22 on
			// disk-backed kind (13 Applications incl. Cilium) at
			// parallelism 0: anon 94-158Mi on a cold start, 179Mi on a
			// hard refresh of every Application (9 helm children at
			// once), and an OOM threshold of ~190-200Mi (a 192Mi and a
			// 160Mi limit each OOMed 1 run in 2, 128Mi always). The old
			// 256Mi sat only ~55-65Mi above that threshold, and at
			// parallelism 0 the peak rises with every chart or git
			// Application added.
			// 384Mi is ~2x over that parallelism-0 worst case and ~4x over
			// the parallelism-2 cold peak (67-98Mi anon across both runs).
			//
			// It also clears a node whose pod storage is memory-backed (a
			// tmpfs root, e.g. the sandbox microVM): there every file the
			// pod writes (git clones, helm charts, the ~176Mi argocd
			// binary copyutil puts in the `var-files` emptyDir) is
			// unreclaimable shmem charged to the POD, whose limit is the
			// sum of its container limits. That pod needs 386-404Mi:
			// 256+128 = 384Mi OOM-looped, 384+128 = 512Mi was 4/4 green.
			//
			// A limit reserves nothing; the only cost is a higher
			// overcommit ceiling on the node.
			resources: {
				requests: memory: "66Mi"
				limits: memory:   "384Mi"
			}
			// GOMEMLIMIT/GOGC stay. The repo-server's own Go heap is
			// ~30-40Mi, far under 128MiB, but its helm children inherit
			// this environment (util/helm/cmd.go sets `cmd.Env =
			// os.Environ()`), and together they take ~28Mi anon (~31Mi
			// memory.peak) off the parallelism-0 cold peak; measured with
			// both removed. GOMEMLIMIT spelling: see `controller.env`.
			//
			// A PlatformStack override of `repoServer.env` replaces this
			// list wholesale (the chart's mergeOverwrite does not merge
			// lists), so it must restate both entries.
			//
			// Deliberately NOT set: git's own memory caps
			// (GIT_CONFIG_COUNT/KEY_n/VALUE_n with
			// core.deltaBaseCacheLimit=16m + pack.threads=1; Argo CD runs
			// git with HOME=/dev/null, so the environment is the only
			// knob). They cut the monorepo fetch 65 -> 19Mi in the
			// v2.13.1 image, but user repositories may be large and
			// single-threaded packing slows them, and the parallelism
			// limit above already bounds the peak.
			env: [
				{name: "GOMEMLIMIT", value: "128MiB"},
				{name: "GOGC", value: "50"},
			]
			extraContainers: [{
				name:  "cue-cmp"
				image: "\(_components."argocd-cue-cmp".values.image.repository):\(_components."argocd-cue-cmp".values.image.tag)"
				command: ["/var/run/argocd/argocd-cmp-server"]
				securityContext: {
					runAsNonRoot: true
					runAsUser:    999
				}
				// Readiness gate on the CMP socket — fixes the
				// Source-Type=Directory startup race (RCA of the
				// intermittent gitops-walk red). The upstream
				// argocd-cmp-server binds a unix socket under the
				// shared `plugins` volume, and argocd-repo-server
				// discovers this plugin by globbing `*.sock` in that
				// same dir. WITHOUT a readiness probe the POD reports
				// Ready the moment repo-server's own probe passes —
				// which can be BEFORE this sidecar's socket exists. In
				// that window repo-server silently falls back to its
				// built-in Directory source type, renders a CUE app
				// repo as "zero raw YAML", and reports Synced/Healthy
				// with NOTHING applied — the AppRafter Application CR
				// never materializes and Argo has no diff to re-render
				// out of the empty-but-Synced state. Gating this
				// sidecar's readiness on the SAME `*.sock` predicate
				// repo-server uses means the pod is Ready — and only
				// then receives repo-server Service traffic — once the
				// plugin is discoverable, closing the race in-cluster
				// (not just in the e2e harness). Probe-pass ≡ CMP
				// functional, so it can't brick a working sidecar; if
				// cmp-server later dies and the socket vanishes the pod
				// drops out of the repo-server endpoints (GitOps
				// pauses) instead of silently Directory-falling-back —
				// the safe failure mode.
				readinessProbe: {
					exec: command: ["sh", "-c", "for s in /home/argocd/cmp-server/plugins/*.sock; do [ -S \"$s\" ] && exit 0; done; exit 1"]
					initialDelaySeconds: 2
					periodSeconds:       3
					timeoutSeconds:      2
					failureThreshold:    30
				}
				// 2.16d: the cue-cmp CMP sidecar (measured 56Mi). A
				// resource-less sidecar caps the repo-server pod at
				// Burstable-without-a-limit for that container; give it its
				// own request+limit so the whole pod is bounded.
				resources: {
					requests: {
						cpu:    "25m"
						memory: "48Mi"
					}
					limits: memory: "128Mi"
				}
				volumeMounts: [{
					mountPath: "/var/run/argocd"
					name:      "var-files"
				}, {
					mountPath: "/home/argocd/cmp-server/plugins"
					name:      "plugins"
				}, {
					mountPath: "/tmp"
					name:      "cmp-tmp"
				}, {
					mountPath: "/home/argocd/cmp-server/config/plugin.yaml"
					subPath:   "plugin.yaml"
					name:      "cue-cmp-config"
				}]
			}]
			volumes: [{
				name: "cue-cmp-config"
				configMap: name: "cue-cmp-plugin-config"
			}, {
				name: "cmp-tmp"
				emptyDir: {}
			}]
		}

		// The cue-cmp sidecar above mounts a ConfigMap named
		// `cue-cmp-plugin-config` at
		// `/home/argocd/cmp-server/config/plugin.yaml`. The
		// ConfigMap itself was MISSING in chart 0.1.10 —
		// `kubelet` reported `MountVolume.SetUp failed for
		// volume "cue-cmp-config": configmap
		// "cue-cmp-plugin-config" not found`, the new
		// repo-server pod stuck in `Init:0/1`, and the Argo
		// CD self-adopt Application reported `Synced/Degraded`
		// (walk-found bug v0.1.106 → v0.1.107).
		//
		// Shipping the ConfigMap via the upstream chart's
		// `extraObjects` value puts it in the same release
		// as the repo-server Deployment. Content is verbatim
		// from `argocd-cue-cmp/plugin.yaml`; if that file
		// evolves (e.g. `discover.find.glob` flips), this
		// block MUST be edited in lockstep until a future
		// `cue cmd` step in the chart renderer reads
		// argocd-cue-cmp/plugin.yaml directly.
		extraObjects: [{
			apiVersion: "v1"
			kind:       "ConfigMap"
			metadata: {
				name:      "cue-cmp-plugin-config"
				namespace: "argocd"
			}
			data: "plugin.yaml": """
				# SPDX-License-Identifier: FSL-1.1-Apache-2.0
				#
				# MIRROR of `argocd-cue-cmp/plugin.yaml`, from its
				# `apiVersion:` line down. Argo CD mounts this ConfigMap OVER
				# the copy baked into the sidecar image (the `subPath:
				# plugin.yaml` mount above), so THIS is what actually runs in
				# a cluster: the file is the source of truth, this block is
				# the delivery vehicle. Editing only the file ships the old
				# snippet to every cluster.
				#
				# Guarded by `scripts/check-cue-cmp-mirror.sh` (wired into
				# `just lint` and the `cue` job of `.github/workflows/
				# lint.yml`). It renders this value and diffs the PARSED
				# document against the file, so this prose preamble may
				# differ from the file's, but nothing that reaches the CMP at
				# runtime can.
				#
				# Every backslash below is DOUBLED. A CUE multi-line block
				# reads `\\(` as interpolation and rejects a bare `\\;` with
				# `unknown escape sequence`; both are `cue vet` errors, not
				# silent diffs.
				#
				# Snippet semantics: ADR 0063 — discovery is convention AND
				# content, and the above-cwd signal is ARGOCD_APP_SOURCE_PATH,
				# never the absolute $PWD — amending ADR 0029.
				apiVersion: argoproj.io/v1alpha1
				kind: ConfigManagementPlugin
				metadata:
				  name: cue
				spec:
				  discover:
				    find:
				      command:
				        - sh
				        - -c
				        - |
				          # Convention gate (ADR 0029, amended by ADR 0063). This
				          # repository is ours iff the REPO-RELATIVE path to a `.cue`
				          # file carries an `apprafter` DIRECTORY component, or the
				          # file's name starts with `apprafter` — AND that file
				          # actually contains an AppRafter manifest marker.
				          #
				          # The path splits in two:
				          #   * BELOW cwd — visible to `find .`.
				          #   * AT or ABOVE cwd — the registered `spec.source.path`,
				          #     which argocd-repo-server hands the discover command as
				          #     ARGOCD_APP_SOURCE_PATH.
				          #
				          # NEVER glob the absolute $PWD. The CMP workdir is
				          # `${ARGOCD_CMP_WORKDIR:-os.TempDir()}/_cmp_server/<uuid>/<path>`
				          # and its base is set by TWO environment variables —
				          # ARGOCD_CMP_WORKDIR and, since Argo calls os.TempDir(),
				          # TMPDIR. An `apprafter` component in the BASE stops the
				          # convention half of the gate applying, for every path in the
				          # repository. `${PWD##*/}` is safe (exactly one component:
				          # the last component of source.path, or the uuid at the repo
				          # root) and is kept for direct, off-cluster invocation.
				          #
				          # Argo CD reads STDOUT, not the exit code (walk-fix #10), and
				          # `find -exec grep -l … +` exits non-zero when the last grep
				          # matched nothing — hence the explicit `exit 0`.
				          RE='apprafter\\.io/(schemas/)?v1alpha1'
				          sp="${ARGOCD_APP_SOURCE_PATH:-}"
				          # A `..` component would let a crafted source.path
				          # (`apprafter/../apps/api`) satisfy the gate while Argo
				          # resolves the cwd elsewhere. Drop it; the below-cwd arm
				          # still decides.
				          case "/$sp/" in */../*) sp="" ;; esac
				          above=0
				          case "/${sp#/}/" in */apprafter/*) above=1 ;; esac
				          case "${PWD##*/}" in apprafter) above=1 ;; esac
				          if [ "$above" = 1 ]; then
				            set -- -name '*.cue'
				          else
				            set -- -name '*.cue' \\( -path '*/apprafter/*' -o -name 'apprafter*.cue' \\)
				          fi
				          # `cue.mod/` holds the schema this sidecar injects on every
				          # render, and `apprafter_claim_gen.cue` is its generated
				          # binding — both carry the marker, so an already-rendered
				          # checkout would otherwise self-match forever.
				          find . -type f ! -path '*/cue.mod/*' ! -name 'apprafter_claim_gen.cue' "$@" \\
				            -exec grep -lE "$RE" {} + 2>/dev/null \\
				            | head -n 1
				          exit 0
				  # Render command. `entrypoint.sh` lives in the sidecar
				  # image and wraps `cue export` so CUE compile errors
				  # surface as structured single-line summaries in the
				  # Argo CD UI (full stderr remains available in the sync
				  # log). Sync details: see ADR 0029 §Rationale.
				  generate:
				    command: [sh, "-c"]
				    args:
				      - /usr/local/bin/entrypoint.sh
				"""
		}]
	}
	syncPolicy: {
		automated: {
			// Self-managing: NEVER prune. A stale upstream chart
			// must not delete the controllers responsible for
			// re-installing it; manual cleanup is the contract.
			prune:    false
			selfHeal: bool | *true
		}
		syncOptions: [...string] | *["CreateNamespace=true", "ServerSideApply=true"]
	}

	// Self-adopt early — before cert-manager and the operator
	// charts try to reconcile, so the OCI repo registration
	// (above) is in place by the time those charts pull from
	// `ghcr.io/apprafter`.
	syncWave: -15

	// Same Kubernetes 1.31+ field skew as
	// `component_cilium.cue`. Argo CD's own Deployments
	// (server, repo-server, applicationset, controller,
	// notifications) ALL surface
	// `status.terminatingReplicas` on k3s v1.35.
	ignoreDifferences: [
		{
			group: "apps"
			kind:  "Deployment"
			jsonPointers: ["/status/terminatingReplicas"]
		},
		{
			group: "apps"
			kind:  "StatefulSet"
			jsonPointers: ["/status/terminatingReplicas"]
		},
	]
}
