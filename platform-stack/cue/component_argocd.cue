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
		server: resources: {
			requests: memory: "24Mi"
			limits: memory:   "128Mi"
		}
		applicationSet: resources: {
			requests: memory: "24Mi"
			limits: memory:   "128Mi"
		}
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
				  if obj.status.phase == "Ready" then
				    hs.status = "Healthy"
				    hs.message = "Reconcile complete"
				    return hs
				  end
				end
				hs.status = "Progressing"
				hs.message = "Awaiting controller reconcile"
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
		repoServer: {
			// 2.16d: repo-server resources (measured 82Mi → req 66Mi / limit 256Mi).
			resources: {
				requests: memory: "66Mi"
				limits: memory:   "256Mi"
			}
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
				# Walk-fix #10 post-B.1.79a (chart 0.1.46, cue-cmp
				# v0.1.5): drop the `| grep -q .` filter from the
				# discover shell snippet. `grep -q` is silent, so
				# the command exited 0 on match but printed nothing
				# to stdout. Argo CD's CMP MatchRepository treats
				# the command as a match only when stdout is non-
				# empty (the `runCommand` return value, not the
				# exit code) — so every discover returned the
				# warning `Plugin command returned zero output` and
				# the sidecar fell back to default directory mode,
				# choking on `package.json` in landing/cms/ exactly
				# as before walk-fix #8.
				#
				# Fix: `find -print -quit` itself prints the first
				# matched path on success and nothing on miss; both
				# code paths exit 0. Stdout emptiness IS the signal.
				# Regression-guarded by `argocd-cue-cmp/test-
				# discover.sh` running in CI.
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
				          if [ "$(basename "$PWD")" = "apprafter" ]; then
				            find . -maxdepth 1 -type f -name '*.cue' -print -quit
				          else
				            find . -type f -name '*.cue' \\( -path '*/apprafter/*' -o -name 'apprafter*.cue' \\) -print -quit
				          fi
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
