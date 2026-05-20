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
				apiVersion: argoproj.io/v1alpha1
				kind: ConfigManagementPlugin
				metadata:
				  name: cue
				spec:
				  discover:
				    find:
				      glob: "**/apprafter*.cue"
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
