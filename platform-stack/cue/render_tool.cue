// SPDX-License-Identifier: FSL-1.1-Apache-2.0

// platform-stack chart renderer. Run with:
//
//   cue cmd render ./platform-stack/cue/...
//   # or, via the helper Makefile:
//   make -C platform-stack render
//
// The command reads `tier1` + `tier2` + `compatibility` from the
// regular `platformstack` package files and emits a complete
// Helm umbrella chart under `dist/platform-stack-<version>/`:
//
//   Chart.yaml
//   values.yaml                          (defaults to tier-1)
//   values.schema.json                   (Helm-native shape validation)
//   compatibility.yaml
//   templates/applications.yaml          (Go template iterating .Values.components)
//   examples/values.solo.yaml            (tier-1 explicit)
//   examples/values.team.yaml            (tier-2 explicit)
//
// CUE picks up this file because it ends in `_tool.cue`. The
// production package files don't import `tool/file` / `tool/exec`
// so a plain `cue vet` / `cue eval` keeps working without
// pulling tool-dependency machinery into the user-visible
// surface.

package platformstack

import (
	"tool/file"
	"encoding/yaml"
	"encoding/json"
)

// `_chartName` is the published chart name. Pinned here so a
// future rebrand flips one constant rather than every emitted
// filename / header.
_chartName: "platform-stack"

// `_distDir` resolves once and is reused by every task. Build
// the path here (not inline) so changing the chart-name layout
// is a one-line edit.
_distDir: "dist/\(_chartName)-\(tier1.version)"

// `_chartYaml` is the rendered chart metadata. We don't shell
// out to `text/template` for the simple substitution — direct
// string interpolation keeps the dependency surface tiny.
//
// `Chart.yaml.tmpl` at `platform-stack/Chart.yaml.tmpl` is the
// human-readable reference for this content; if the two ever
// diverge the template is the source of truth. We do not read
// the template at render time because CUE's `tool/file.Read`
// would couple the renderer to the project's filesystem layout
// for a file whose content is small and stable.
_chartYaml: """
	# Rendered by `cue cmd render`. Do not edit.
	# SPDX-License-Identifier: FSL-1.1-Apache-2.0
	apiVersion: v2
	name: \(_chartName)
	description: |
	  AppRafter platform-stack: a curated bundle of Cilium,
	  cert-manager, Argo CD, apprafter-operator, admission-webhook,
	  and (optionally) Backstage, rendered as Argo CD Applications
	  from CUE source.
	type: application
	version: "\(tier1.version)"
	appVersion: "\(tier1.version)"
	home: https://apprafter.io
	sources:
	  - https://github.com/apprafter/apprafter/tree/main/platform-stack
	maintainers:
	  - name: AppRafter Authors
	    url: https://apprafter.io
	keywords:
	  - apprafter
	  - platform
	  - argocd
	  - cilium
	  - cert-manager
	  - kubernetes
	annotations:
	  apprafter.io/change-class: "\(compatibility[tier1.version].change)"
	  apprafter.io/operator-version: "\(compatibility[tier1.version].operatorVersion)"

	"""

// `_applicationsTemplate` is the umbrella chart's ONLY template.
// It iterates over `.Values.components` and emits one Argo CD
// Application per enabled entry. The chart is "data-driven" —
// adding a component means adding a CUE declaration and
// re-rendering; the template doesn't need touching.
//
// Tier overlays gate components via `enabled: bool`. The
// template skips disabled entries, so the rendered manifest
// contains only what the operator's tier wants.
//
// `valuesObject` is the Helm 3+ way to pass nested values
// through to a sub-chart (here, the upstream Cilium / Argo CD
// / cert-manager chart); the alternative `helm.parameters`
// would force key/value flattening.
//
// Note the double-curly braces: this string is itself a Go
// template that Helm will execute at install time, so we keep
// the `{{ }}` literal. CUE happily ships this verbatim.
_applicationsTemplate: """
	{{/*
	  SPDX-License-Identifier: FSL-1.1-Apache-2.0
	  Rendered by `cue cmd render`. Do not edit.
	  Iterates over .Values.components, emitting one Argo CD
	  Application per enabled entry.
	*/}}
	{{- range $name, $component := .Values.components }}
	{{- if $component.enabled }}
	---
	apiVersion: argoproj.io/v1alpha1
	kind: Application
	metadata:
	  name: {{ $name | quote }}
	  namespace: argocd
	  annotations:
	    argocd.argoproj.io/sync-wave: {{ $component.syncWave | quote }}
	  labels:
	    apprafter.io/component: {{ $name | quote }}
	    apprafter.io/tier: {{ $.Values.tier | quote }}
	    apprafter.io/channel: {{ $.Values.channel | quote }}
	spec:
	  project: default
	  source:
	    repoURL: {{ $component.source.repoURL | quote }}
	    {{- with $component.source.chart }}
	    chart: {{ . | quote }}
	    {{- end }}
	    {{- with $component.source.path }}
	    path: {{ . | quote }}
	    {{- end }}
	    targetRevision: {{ $component.version | quote }}
	    {{- if $component.source.chart }}
	    helm:
	      valuesObject:
	{{ toYaml $component.values | indent 8 }}
	    {{- end }}
	  destination:
	    server: https://kubernetes.default.svc
	    namespace: {{ $component.namespace | quote }}
	  syncPolicy:
	{{ toYaml $component.syncPolicy | indent 4 }}
	  {{- with $component.ignoreDifferences }}
	  ignoreDifferences:
	{{ toYaml . | indent 4 }}
	  {{- end }}
	{{- end }}
	{{- end }}

	"""

// `_valuesSchema` is the JSON-schema Helm uses to validate the
// values document supplied via `helm install -f`. We hand-roll
// it (rather than auto-generating from `#PlatformValues`)
// because Helm's schema dialect is JSON-Schema-2019 with
// vendor extensions, while CUE's JSON-schema export targets
// the older draft-07. The mismatch isn't worth chasing for a
// small, stable shape.
_valuesSchema: {
	"$schema": "https://json-schema.org/draft/2020-12/schema"
	type:      "object"
	required: ["version", "tier", "channel", "components"]
	properties: {
		version: {
			type:    "string"
			pattern: "^[0-9]+\\.[0-9]+\\.[0-9]+(-[0-9A-Za-z.-]+)?$"
		}
		tier: {
			type: "integer"
			enum: [1, 2, 3, 4]
		}
		channel: {
			type: "string"
			enum: ["stable", "edge"]
		}
		components: {
			type: "object"
			patternProperties: {
				"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$": {
					type: "object"
					required: ["name", "enabled", "namespace", "source", "version"]
					properties: {
						name: {type: "string"}
						enabled: {type: "boolean"}
						namespace: {type: "string"}
						source: {
							type: "object"
							required: ["repoURL"]
							properties: {
								repoURL: {type: "string"}
								chart: {type: "string"}
								path: {type: "string"}
							}
						}
						version: {type: "string"}
						values: {type: "object"}
						syncPolicy: {type: "object"}
						syncWave: {type: "integer"}
						ignoreDifferences: {
							type: "array"
							items: {type: "object"}
						}
					}
				}
			}
			additionalProperties: false
		}
	}
}

// `_readmeContent` ships next to the rendered chart so consumers
// who pull from the OCI registry see a brief pointer back to
// the canonical docs without having to know the AppRafter
// monorepo exists.
_readmeContent: """
	# platform-stack-\(tier1.version)

	Rendered umbrella Helm chart for the AppRafter platform layer.
	**Do not edit this directory by hand** — it is generated from
	the CUE source at
	[`platform-stack/cue/`](https://github.com/apprafter/apprafter/tree/main/platform-stack/cue)
	by `cue cmd render` and published to
	`oci://ghcr.io/apprafter/platform-stack`.

	## What's inside

	- `Chart.yaml`           — chart metadata.
	- `values.yaml`          — defaults (tier-1 / solo).
	- `values.schema.json`   — Helm-native schema.
	- `templates/`           — single template iterating over `.Values.components`.
	- `examples/`            — per-tier ready-to-use values files.
	- `compatibility.yaml`   — change classification metadata.

	## Quick install

	```sh
	helm install platform oci://ghcr.io/apprafter/platform-stack \\
	    --version \(tier1.version) \\
	    --values values.yaml          # or examples/values.team.yaml for tier 2+
	```

	Consult the AppRafter operator guide for the full bootstrap
	flow (`apprafter bootstrap-all`).

	"""

command: render: {
	mkdist: file.Mkdir & {
		path:          _distDir
		createParents: true
	}

	mktemplates: file.Mkdir & {
		path:          "\(_distDir)/templates"
		createParents: true
		$dep:          mkdist.$done
	}

	mkexamples: file.Mkdir & {
		path:          "\(_distDir)/examples"
		createParents: true
		$dep:          mkdist.$done
	}

	chartYaml: file.Create & {
		filename: "\(_distDir)/Chart.yaml"
		contents: _chartYaml
		$dep:     mkdist.$done
	}

	// `values.yaml` defaults to the tier-1 (solo) overlay —
	// matches the v0.1.x cluster-bootstrap baseline so an
	// operator running `helm install platform-stack` without
	// `--values` ends up with the same component set the
	// pre-rewrite path installed.
	valuesYaml: file.Create & {
		filename: "\(_distDir)/values.yaml"
		contents: yaml.Marshal(tier1)
		$dep:     mkdist.$done
	}

	valuesSchemaJson: file.Create & {
		filename: "\(_distDir)/values.schema.json"
		contents: json.Indent(json.Marshal(_valuesSchema), "", "  ")
		$dep:     mkdist.$done
	}

	soloExample: file.Create & {
		filename: "\(_distDir)/examples/values.solo.yaml"
		contents: yaml.Marshal(tier1)
		$dep:     mkexamples.$done
	}

	teamExample: file.Create & {
		filename: "\(_distDir)/examples/values.team.yaml"
		contents: yaml.Marshal(tier2)
		$dep:     mkexamples.$done
	}

	appsTemplate: file.Create & {
		filename: "\(_distDir)/templates/applications.yaml"
		contents: _applicationsTemplate
		$dep:     mktemplates.$done
	}

	compatibilityYaml: file.Create & {
		filename: "\(_distDir)/compatibility.yaml"
		contents: yaml.Marshal(compatibility)
		$dep:     mkdist.$done
	}

	readme: file.Create & {
		filename: "\(_distDir)/README.md"
		contents: _readmeContent
		$dep:     mkdist.$done
	}
}
