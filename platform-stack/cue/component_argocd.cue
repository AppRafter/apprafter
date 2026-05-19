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
	version: "7.7.7"
	values: {
		// Single replicas on tier-1 (cpx22 RAM budget). Tier
		// 2+ overlays scale these up; Dex stays off until
		// OIDC SSO lands in Phase 3.
		//
		// `redis-ha.enabled: false` is critical on single-node
		// k3s — the upstream chart's redis-ha StatefulSet sets
		// `requiredDuringSchedulingIgnoredDuringExecution`
		// podAntiAffinity across 3 redis pods, which never
		// schedule on one node. The loader install
		// (`commands/cluster_bootstrap.rs::argocd_loader_values_yaml`)
		// disables it too; this overlay keeps them aligned so
		// the chart's self-reconcile doesn't re-enable it on
		// adoption.
		controller: replicas: int | *1
		"redis-ha": enabled:  bool | *false
		server: replicas:     int | *1
		server: service: type: string | *"ClusterIP"
		applicationSet: replicaCount: int | *1
		notifications: enabled:       bool | *false
		dex: enabled:                 bool | *false

		// OCI Helm repository registration. Without this the
		// `argocd-repo-server` shells out to
		// `helm pull --repo oci://ghcr.io/apprafter <chart>`,
		// which is malformed for OCI registries — `helm pull`
		// for OCI requires `helm pull oci://<repo>/<chart>` form.
		// Argo CD bridges that by reading `enableOCI: "true"`
		// off this registration and rewriting the pull
		// command. URL is BARE (no `oci://` scheme); Argo CD
		// adds the scheme based on `enableOCI`. Matches the
		// loader-side block in
		// `cli-providers::k8s::argocd_loader_values_yaml` so
		// the chart's self-reconcile keeps the repo registered
		// when it adopts the loader Argo CD release.
		configs: repositories: apprafter: {
			url:       "ghcr.io/apprafter"
			type:      "helm"
			enableOCI: "true"
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
		repoServer: {
			replicas: int | *1
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
}
