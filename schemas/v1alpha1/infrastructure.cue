// SPDX-License-Identifier: FSL-1.1-MIT

package v1alpha1

// Infrastructure describes the substrate the platform runs on
// (provider, nodes, network, firewall, OS image). Applied by
// `platform-cli`. See spec.md §3.7 and phase 1.2 / 5.2 / 6.2.
//
// Skeleton schema. Network / firewall / SSH-key / floating-IP
// fields are optional and read by the v0.1.5+ manifest parser;
// the v1alpha1 CRD admission webhook (phase 1.7) tightens
// validation later.
#Infrastructure: {
	#TypeMeta
	kind:     "Infrastructure"
	metadata: #ObjectMeta
	spec: {
		// Provider identifier (built-in: "hetzner-cloud",
		// "hetzner-robot", "aws"; community via
		// InfrastructureProviderPlugin).
		provider: string

		// Provider-specific region (e.g. "nbg1" for Hetzner).
		region?: string

		nodes: [...{
			role: "control-plane" | "worker" | "egress"
			// Provider-specific server type, e.g. "cx22".
			type:  string
			count: int & >=1
		}]

		// Optional private network. The v0.1.5 parser reads it as
		// a NetworkSpec; absent fields fall back to the CLI's
		// 10.0.0.0/16 / 10.0.0.0/24 / eu-central defaults.
		network?: {
			ip_range?: string
			subnet?: {
				ip_range?: string
				zone?:     string
			}
			// Floating IPs for static egress. Each entry is a short
			// name (e.g. "egress"); the CLI prefixes it with the
			// cluster name on the provider side. Wired by v0.1.6.
			floatingIPs?: [...string]
		}

		// Optional firewall. Each rule is a single ingress
		// declaration; `port` is a Hetzner port spec ("22",
		// "80-90", "any"). When absent, the CLI uses the SSH 22 +
		// HTTPS 443 ingress defaults.
		firewall?: {
			ingress?: [...{
				port:      string
				protocol?: "tcp" | "udp" | "icmp"
				source_ips?: [...string]
			}]
		}

		// Optional SSH keys. Each entry needs at least the
		// public_key. Without this list, the CLI keeps reading
		// APPRAFTER_SSH_PUBLIC_KEY from env (v0.1.3 behaviour).
		sshKeys?: [...{
			name?:      string
			public_key: string
		}]

		// OS image identifier (e.g. "ubuntu-24.04").
		osImage?: string

		// Optional Argo CD UI exposure + bootstrap. When `domain` is
		// set, `platform-cli cluster-bootstrap` provisions a Gateway
		// + HTTPRoute + cert-manager Certificate so the UI is
		// reachable at https://<domain>. When `bootstrapRepo` is
		// set, an Argo CD `Application` named `bootstrap` is applied
		// pointing at that Git repo (default path: `.`); subsequent
		// commits to the repo are auto-synced (`prune + selfHeal`).
		argocd?: {
			domain?:        string
			bootstrapRepo?: string
			bootstrapPath?: string
		}

		// Optional Backstage tier-1 deploy. When `domain` is set,
		// `platform-cli cluster-bootstrap` provisions a Namespace +
		// Deployment + Service + HTTPRoute + cert-manager
		// Certificate so the UI is reachable at https://<domain>.
		// `image` defaults to a placeholder; supply your own.
		backstage?: {
			domain?: string
			image?:  string
		}

		// AppRafter operator helm release. From v0.1.64 onwards
		// `cluster-bootstrap` installs the operator in
		// `apprafter-system` by default using the released ghcr.io
		// image. Set `enabled: false` to skip; override `image`
		// and/or `tag` for fork / dev builds. See ADR / changelog
		// for the §1.14 default-on flip.
		operator?: {
			enabled?: bool
			image?:   string
			tag?:     string
		}

		// AppRafter admission-webhook stack (Namespace + cert-manager
		// Certificate + Service + Deployment +
		// ValidatingWebhookConfiguration). From v0.1.64 onwards the
		// stack installs in `apprafter-system` by default using the
		// released ghcr.io image. Set `enabled: false` to skip;
		// override `image` and/or `tag` for fork / dev builds.
		admissionWebhook?: {
			enabled?: bool
			image?:   string
			tag?:     string
		}
	}
}
