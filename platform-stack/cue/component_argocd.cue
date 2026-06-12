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
			// health both labels the tree node with the upgrade
			// details AND, via Argo CD's health aggregation of
			// the anchored node, bubbles the root Application to
			// non-healthy whenever an upgrade is pending — the
			// root-level "an update is pending" signal, for free.
			//
			// Reads `spec.trigger.{from,to}`,
			// `spec.risks.classification`, `status.phase` — all
			// fields the operator already populates. `->` (not the
			// unicode arrow) avoids encoding/lint surprises in
			// argocd-cm.
			"resource.customizations.health.apprafter.io_MigrationPlan": """
				hs = {}
				local phase = ""
				if obj.status ~= nil and obj.status.phase ~= nil then phase = obj.status.phase end
				local from, to, class = "?", "?", "?"
				if obj.spec ~= nil then
				  if obj.spec.trigger ~= nil then
				    from = obj.spec.trigger.from or from
				    to   = obj.spec.trigger.to or to
				  end
				  if obj.spec.risks ~= nil and obj.spec.risks.classification ~= nil then
				    class = obj.spec.risks.classification
				  end
				end
				if phase == "pending-approval" or phase == "" then
				  hs.status = "Suspended"
				  hs.message = "Upgrade " .. from .. "->" .. to .. " (" .. class .. ") awaiting approval - click Approve, or run 'apprafter migration approve " .. (obj.metadata.name or "<name>") .. "'"
				  return hs
				end
				if phase == "approved" or phase == "executing" then
				  hs.status = "Progressing"
				  hs.message = "Upgrade " .. from .. "->" .. to .. " approved; applying"
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

			// 2.13/argo-upgrade-approval-surface (ADR 0048):
			// custom health for Argo CD's own `Application` kind.
			// The operator annotates the platform-stack root
			// Application with `apprafter.io/upgrade-pending=true`
			// (+ from/to/class/plan) when a platform upgrade is
			// awaiting approval, surfacing a root-level "platform
			// update pending approval" banner directly on the App
			// tile in the Argo CD UI.
			//
			// CRITICAL: this OVERRIDES Argo CD's built-in health
			// for EVERY Argo Application cluster-wide — the
			// platform root App, all child Applications, AND every
			// `apprafter app add` user app. The non-banner branch
			// MUST forward Argo's own computed `obj.status.health`
			// verbatim or it silently mis-reports every app. `->`
			// is ASCII (not the unicode arrow) on purpose. A live
			// walk gates whether this ships and validates the
			// pass-through; it may be dropped.
			"resource.customizations.health.argoproj.io_Application": """
				hs = {}
				local a = nil
				if obj.metadata ~= nil then a = obj.metadata.annotations end
				if a ~= nil and a["apprafter.io/upgrade-pending"] == "true" then
				  hs.status = "Suspended"
				  hs.message = "platform update " .. (a["apprafter.io/upgrade-from"] or "?") .. "->" .. (a["apprafter.io/upgrade-to"] or "?") .. " pending approval (" .. (a["apprafter.io/upgrade-class"] or "?") .. ") - expand the tree and Approve the MigrationPlan, or run 'apprafter migration approve " .. (a["apprafter.io/upgrade-plan"] or "<plan>") .. "'"
				  return hs
				end
				if obj.status ~= nil and obj.status.health ~= nil and obj.status.health.status ~= nil and obj.status.health.status ~= "" then
				  hs.status = obj.status.health.status
				  hs.message = obj.status.health.message
				  return hs
				end
				hs.status = "Progressing"
				hs.message = "Initializing"
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
			extraContainers: [{
				name:  "cue-cmp"
				image: "\(_components."argocd-cue-cmp".values.image.repository):\(_components."argocd-cue-cmp".values.image.tag)"
				command: ["/var/run/argocd/argocd-cmp-server"]
				securityContext: {
					runAsNonRoot: true
					runAsUser:    999
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
