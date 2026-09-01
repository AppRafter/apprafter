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
	  - https://github.com/apprafter/apprafter/tree/master/platform-stack
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
// Override merge (B.1.73): when `.Values.overrides.<name>` is
// set, the template overlays it onto the chart's per-component
// declaration:
//
//   - `overrides.<name>.enabled` REPLACES `component.enabled`
//     (deciding whether the child Application is emitted at
//     all).
//   - `overrides.<name>.pin` REPLACES `component.version` for
//     the rendered `spec.source.targetRevision`.
//   - `overrides.<name>.values` DEEP-MERGES onto
//     `component.values` (override wins on collisions). Helm's
//     `mergeOverwrite` does a recursive merge in that direction.
//
// Override writes go through PlatformController's SSA patch
// on the parent platform Application — the CR
// `PlatformStack.spec.overrides` is its source. Operators
// editing values directly through `kubectl edit application`
// trigger `UnauthorizedSourceModification=True` on the
// PlatformStack.
//
// Note the double-curly braces: this string is itself a Go
// template that Helm will execute at install time, so we keep
// the `{{ }}` literal. CUE happily ships this verbatim.
// `_appProjectsTemplate` — emits one `kind: AppProject` per
// entry in `.Values.appProjects`. All projects render at
// sync-wave -30 — earlier than even Cilium (-20) so the
// projects exist before any Application referencing them is
// applied by Argo CD. Walk-fix #2 post-B.1.79a; see
// `app_projects.cue` for the why and the `_appProjects` map
// for the contents.
//
// Note the double-curly braces: this string is itself a Go
// template Helm executes at install time, so we keep the
// `{{ }}` literal. CUE ships it verbatim.
_appProjectsTemplate: """
	{{/*
	  SPDX-License-Identifier: FSL-1.1-Apache-2.0
	  Rendered by `cue cmd render`. Do not edit.
	  Iterates over .Values.appProjects and emits one
	  kind: AppProject per entry at sync-wave -30.
	*/}}
	{{- range $name, $project := .Values.appProjects }}
	---
	apiVersion: argoproj.io/v1alpha1
	kind: AppProject
	metadata:
	  name: {{ $name | quote }}
	  namespace: argocd
	  annotations:
	    argocd.argoproj.io/sync-wave: "-30"
	  labels:
	    apprafter.io/managed-by: apprafter
	    apprafter.io/source: platform-stack
	spec:
	  description: {{ $project.description | quote }}
	  sourceRepos:
	{{ toYaml $project.sourceRepos | indent 4 }}
	  destinations:
	{{ toYaml $project.destinations | indent 4 }}
	  clusterResourceWhitelist:
	{{ toYaml $project.clusterResourceWhitelist | indent 4 }}
	  namespaceResourceWhitelist:
	{{ toYaml $project.namespaceResourceWhitelist | indent 4 }}
	{{- end }}

	"""

// `_serviceProvidersTemplate` — emits one `kind: ServiceProvider`
// (apprafter.io/v1alpha1) per entry in `.Values.serviceProviders`.
// Seeds the launch-default backends so the 2.3 scheduler has a
// provider to match on a fresh cluster. Carries
// `SkipDryRunOnMissingResource=true` so Argo CD tolerates the
// window before the apprafter-operator child Application installs
// the ServiceProvider CRD, retrying until it exists; the positive
// per-entry sync-wave keeps the CR after that child Application's
// wave-0 sync.
//
// Note the double-curly braces: this string is itself a Go
// template Helm executes at install time, so we keep the `{{ }}`
// literal. CUE ships it verbatim.
_serviceProvidersTemplate: """
	{{/*
	  SPDX-License-Identifier: FSL-1.1-Apache-2.0
	  Rendered by `cue cmd render`. Do not edit.
	  Iterates over .Values.serviceProviders and emits one
	  kind: ServiceProvider per entry.
	*/}}
	{{- range $name, $sp := .Values.serviceProviders }}
	---
	apiVersion: apprafter.io/v1alpha1
	kind: ServiceProvider
	metadata:
	  name: {{ $name | quote }}
	  namespace: {{ $sp.namespace | quote }}
	  annotations:
	    argocd.argoproj.io/sync-wave: {{ $sp.syncWave | quote }}
	    argocd.argoproj.io/sync-options: SkipDryRunOnMissingResource=true
	  labels:
	{{ toYaml $sp.labels | indent 4 }}
	    apprafter.io/managed-by: apprafter
	    apprafter.io/source: platform-stack
	spec:
	  type: {{ $sp.type | quote }}
	  backend: {{ $sp.backend | quote }}
	  config:
	{{ toYaml $sp.config | indent 4 }}
	{{- end }}

	"""

// `_gatewayTemplate` — emits the public-ingress Gateway and its
// HTTP→HTTPS redirect (1.83a). Host-network mode: the Gateway's
// Envoy binds the node host ports 80/443 directly (cilium
// `gatewayAPI.hostNetwork.enabled`), so there is NO LoadBalancer
// Service and hence no LB-IPAM IPPool / L2-announcement backing.
// The whole block is guarded by `{{- if .Values.gateway.allowedDomains }}`
// so a default tier-1 render (empty `allowedDomains`) produces
// NO resources — the Gateway only materialises once an operator
// registers at least one domain. The `gateway-api-crds` (wave
// -25) + cilium `gatewayAPI` (wave -20) components run first, so
// at this template's default sync-wave 0 the Gateway API CRDs and
// the cilium gateway controller are already present.
//
// Per allowed domain the Gateway gets two HTTPS listeners (apex +
// wildcard), each named with a DOT-FREE sanitised host so the
// listener name stays a valid k8s section name; a single HTTP
// listener + an HTTPRoute redirect everything to HTTPS (301).
//
// Note the double-curly braces: this string is itself a Go
// template Helm executes at install time, so we keep the `{{ }}`
// literal. CUE ships it verbatim.
_gatewayTemplate: """
	{{/* SPDX-License-Identifier: FSL-1.1-Apache-2.0
	     Rendered by `cue cmd render`. Do not edit.
	     Public ingress (1.83a): host-network mode — the Gateway's Envoy binds the
	     node host ports 80/443 directly (cilium gatewayAPI.hostNetwork); no
	     LoadBalancer Service / LB-IPAM / L2. Emitted only when allowedDomains is
	     non-empty. */}}
	{{- if .Values.gateway.allowedDomains }}
	{{- $gw := .Values.gateway }}
	---
	apiVersion: gateway.networking.k8s.io/v1
	kind: Gateway
	metadata:
	  name: platform
	  namespace: apprafter-system
	  labels:
	    apprafter.io/platform-gateway: "true"
	  annotations:
	    argocd.argoproj.io/sync-options: SkipDryRunOnMissingResource=true
	spec:
	  gatewayClassName: cilium
	  listeners:
	  {{- range $e := $gw.allowedDomains }}
	  {{- $san := $e.domain | replace "." "-" }}
	  - name: {{ printf "https-apex-%s" $san | quote }}
	    port: 443
	    protocol: HTTPS
	    hostname: {{ $e.domain | quote }}
	    tls:
	      mode: Terminate
	      certificateRefs:
	      - kind: Secret
	        name: {{ $e.importedCertRef | quote }}
	        namespace: apprafter-system
	    allowedRoutes:
	      namespaces:
	        from: All
	  - name: {{ printf "https-wild-%s" $san | quote }}
	    port: 443
	    protocol: HTTPS
	    hostname: {{ printf "*.%s" $e.domain | quote }}
	    tls:
	      mode: Terminate
	      certificateRefs:
	      - kind: Secret
	        name: {{ $e.importedCertRef | quote }}
	        namespace: apprafter-system
	    allowedRoutes:
	      namespaces:
	        from: All
	  {{- end }}
	  - name: http
	    port: 80
	    protocol: HTTP
	    allowedRoutes:
	      namespaces:
	        from: All
	---
	apiVersion: gateway.networking.k8s.io/v1
	kind: HTTPRoute
	metadata:
	  name: platform-http-redirect
	  namespace: apprafter-system
	spec:
	  parentRefs:
	  - name: platform
	    sectionName: http
	  rules:
	  - filters:
	    - type: RequestRedirect
	      requestRedirect:
	        scheme: https
	        statusCode: 301
	{{- end }}

	"""

// `_backupTemplate` — emits the opt-in off-site scheduled-backup
// component (2.6d-4): a ServiceAccount + scoped ClusterRole/-Binding, a
// nightly backup CronJob, a weekly `restic check` CronJob, and a
// CiliumNetworkPolicy pinning the runner pods' egress. The WHOLE block
// is guarded by `{{- if .Values.backup.enabled }}` so a default tier-1
// render (`backup.enabled: false`) produces NO resources — off-site
// backup only materialises once an operator runs `apprafter backup
// enable`, which flips `PlatformStack.spec.backup.enabled` and the
// PlatformController projects it onto `.Values.backup`.
//
// Credentials NEVER live in chart values — the CronJobs receive the
// operator-sealed Secret named `.Values.backup.credentialRef.name` via
// explicit `env[*].valueFrom.secretKeyRef` entries. The Secret holds
// NEUTRAL canonical keys (`S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`,
// `RESTIC_PASSWORD`, and optionally `S3_REGION`); the template maps
// them to the `AWS_*` / `RESTIC_PASSWORD` names restic expects. Region
// is `optional: true` — many S3-compatible stores don't need one.
//
// RBAC is scoped to the runner's ACTUAL read-set (chunk-2 code:
// `kube_rs_exec.rs` + `status.rs`): list/get the AppRafter + Argo
// `Application`s, `SharedVolume`s, `PlatformStack`, `ResourceClaim`s,
// CNPG `Cluster`s, `SealedSecret`s cluster-wide; get core `Secret`s;
// create/get/delete helper `pods` + create `pods/exec`; and
// create/get/update/patch ONLY the `apprafter-backup-status` ConfigMap.
// It deliberately does NOT grant write on `platformstacks` — a
// compromised backup pod must never reach the platform upgrade control
// (k8s has no field-level RBAC; the status write is a ConfigMap, not a
// CR annotation). `pods`/`pods/exec` is unavoidably cluster-wide: k8s
// RBAC cannot scope exec to "only the runner's own helper pods".
//
// The `check` CronJob runs `restic` directly (the runner image bundles
// restic + a shell) rather than the runner binary — the chunk-2 runner
// has no check-only mode, so this keeps the periodic repository check
// operator-independent and needs no runner change.
//
// Note the double-curly braces: this string is itself a Go template
// Helm executes at install time, so we keep the `{{ }}` literal. CUE
// ships it verbatim.
_backupTemplate: """
	{{/* SPDX-License-Identifier: FSL-1.1-Apache-2.0
	     Rendered by `cue cmd render`. Do not edit.
	     Off-site scheduled backup (2.6d-4): opt-in, default-off. Emitted only when
	     .Values.backup.enabled. ServiceAccount + scoped ClusterRole/-Binding, a
	     nightly backup CronJob (runner binary), a weekly `restic check` CronJob
	     (restic directly), and a CiliumNetworkPolicy fixing the runner pods'
	     egress (DNS + kube-apiserver + world:443 for S3/webhook). Credentials come
	     ONLY from the operator-sealed Secret via explicit env secretKeyRef — never
	     chart values; Secret holds neutral S3_* keys mapped to AWS_* for restic.
	     RBAC matches the chunk-2 runner's actual reads and MUST NOT grant write on
	     platformstacks. */}}
	{{- if .Values.backup.enabled }}
	{{- $b := .Values.backup }}
	---
	apiVersion: v1
	kind: ServiceAccount
	metadata:
	  name: apprafter-backup
	  namespace: apprafter-system
	  labels:
	    apprafter.io/managed-by: apprafter
	    apprafter.io/source: platform-stack
	---
	# Scoped to the runner's ACTUAL read-set (chunk-2 code). NOTE: cluster-wide
	# pods/exec is unavoidable — k8s RBAC cannot scope exec to the runner's own
	# helper pods. Deliberately powerful SA (backup legitimately reads everything);
	# it must NOT be able to write platformstacks (no field-level RBAC → a
	# compromised backup pod must not reach the platform upgrade control).
	apiVersion: rbac.authorization.k8s.io/v1
	kind: ClusterRole
	metadata:
	  name: apprafter-backup
	  labels:
	    apprafter.io/managed-by: apprafter
	    apprafter.io/source: platform-stack
	rules:
	# CRs the engine's list/get sweep reads (serialized into the backup manifest).
	- apiGroups: ["apprafter.io"]
	  resources: ["applications", "sharedvolumes", "platformstacks", "resourceclaims", "sourcecredentials"]
	  verbs: ["get", "list"]
	- apiGroups: ["argoproj.io"]
	  resources: ["applications"]
	  verbs: ["get", "list"]
	- apiGroups: ["postgresql.cnpg.io"]
	  resources: ["clusters"]
	  verbs: ["get", "list"]
	- apiGroups: ["bitnami.com"]
	  resources: ["sealedsecrets"]
	  verbs: ["get", "list"]
	# Core Secrets — the app/connection creds the sweep captures + reseals.
	# `list` (not just get): the sweep enumerates each app namespace's secrets.
	- apiGroups: [""]
	  resources: ["secrets"]
	  verbs: ["get", "list"]
	# Ephemeral helper pods (pg_dump / tar). `patch` because the runner
	# server-side-APPLIES the helper Pod (SSA is a PATCH, not a create);
	# `pods/exec` needs `get` because kube-rs's WebSocket exec is a GET upgrade
	# (kubectl exec POSTs, hence the original create-only rule — the in-cluster
	# runner's verb differs). exec is namespace-wide (k8s limit).
	- apiGroups: [""]
	  resources: ["pods"]
	  verbs: ["create", "get", "patch", "delete"]
	- apiGroups: [""]
	  resources: ["pods/exec"]
	  verbs: ["create", "get"]
	# The status ConfigMap ONLY (M-r3-2): create-or-update the single
	# apprafter-backup-status CM. `create` CANNOT be restricted by resourceNames
	# (the object name is unknown at authorization time — a resourceNames rule
	# that includes `create` silently forbids it), so `create` is its own
	# unrestricted rule and the mutating verbs stay locked to the one object —
	# still NOT a platformstacks write.
	- apiGroups: [""]
	  resources: ["configmaps"]
	  verbs: ["create"]
	- apiGroups: [""]
	  resources: ["configmaps"]
	  resourceNames: ["apprafter-backup-status"]
	  verbs: ["get", "update", "patch"]
	---
	apiVersion: rbac.authorization.k8s.io/v1
	kind: ClusterRoleBinding
	metadata:
	  name: apprafter-backup
	  labels:
	    apprafter.io/managed-by: apprafter
	    apprafter.io/source: platform-stack
	roleRef:
	  apiGroup: rbac.authorization.k8s.io
	  kind: ClusterRole
	  name: apprafter-backup
	subjects:
	- kind: ServiceAccount
	  name: apprafter-backup
	  namespace: apprafter-system
	---
	apiVersion: batch/v1
	kind: CronJob
	metadata:
	  name: apprafter-backup
	  namespace: apprafter-system
	  labels:
	    apprafter.io/managed-by: apprafter
	    apprafter.io/source: platform-stack
	spec:
	  schedule: {{ $b.schedule | default "0 3 * * *" | quote }}
	  {{- with $b.timeZone }}
	  # 2.22g / D2: without this the schedule runs in the
	  # kube-controller-manager's zone, and nothing anywhere says which.
	  timeZone: {{ . | quote }}
	  {{- end }}
	  concurrencyPolicy: Forbid
	  successfulJobsHistoryLimit: 3
	  failedJobsHistoryLimit: 3
	  jobTemplate:
	    spec:
	      template:
	        metadata:
	          labels:
	            apprafter.io/backup-runner: "true"
	        spec:
	          serviceAccountName: apprafter-backup
	          restartPolicy: Never
	          containers:
	          - name: runner
	            image: {{ $b.image | quote }}
	            env:
	            - name: RESTIC_PASSWORD
	              valueFrom:
	                secretKeyRef:
	                  name: {{ $b.credentialRef.name | quote }}
	                  key: RESTIC_PASSWORD
	            - name: AWS_ACCESS_KEY_ID
	              valueFrom:
	                secretKeyRef:
	                  name: {{ $b.credentialRef.name | quote }}
	                  key: S3_ACCESS_KEY_ID
	            - name: AWS_SECRET_ACCESS_KEY
	              valueFrom:
	                secretKeyRef:
	                  name: {{ $b.credentialRef.name | quote }}
	                  key: S3_SECRET_ACCESS_KEY
	            - name: AWS_DEFAULT_REGION
	              valueFrom:
	                secretKeyRef:
	                  name: {{ $b.credentialRef.name | quote }}
	                  key: S3_REGION
	                  optional: true
	            - name: APPRAFTER_BACKUP_REPO
	              value: {{ $b.bucket | quote }}
	            - name: APPRAFTER_CLUSTER_ID
	              value: {{ .Release.Name | quote }}
	            - name: APPRAFTER_BACKUP_STAGING_MODE
	              value: {{ $b.stagingMode | default "monolithic" | quote }}
	            - name: APPRAFTER_BACKUP_ENFORCE
	              value: {{ $b.retention.enforce | default "operator" | quote }}
	            {{- if $b.retention.keepDaily }}
	            - name: APPRAFTER_BACKUP_KEEP_DAILY
	              value: {{ $b.retention.keepDaily | quote }}
	            {{- end }}
	            {{- if $b.retention.keepWeekly }}
	            - name: APPRAFTER_BACKUP_KEEP_WEEKLY
	              value: {{ $b.retention.keepWeekly | quote }}
	            {{- end }}
	            {{- if $b.retention.keepMonthly }}
	            - name: APPRAFTER_BACKUP_KEEP_MONTHLY
	              value: {{ $b.retention.keepMonthly | quote }}
	            {{- end }}
	            {{- if $b.failureWebhook }}
	            - name: APPRAFTER_BACKUP_FAILURE_WEBHOOK
	              value: {{ $b.failureWebhook | quote }}
	            {{- end }}
	            resources:
	              requests:
	                cpu: 100m
	                memory: 256Mi
	              limits:
	                memory: 512Mi
	            volumeMounts:
	            - name: staging
	              mountPath: /staging
	          volumes:
	          - name: staging
	            emptyDir:
	              sizeLimit: {{ $b.stagingSizeLimit | default "10Gi" | quote }}
	---
	{{- if $b.checkSchedule }}
	# 2.22g / D2: an EMPTY checkSchedule omits this CronJob entirely — that is
	# what `apprafter backup --check off` writes. Before this guard there was no
	# off switch, so the documentation taught `--check-cron "0 6 31 2 *"` — the
	# 31st of February, a date that never arrives — as the supported answer.
	# The `| default` below is dead while this guard holds, and is kept only
	# so a hand-written values file without the key still renders.
	apiVersion: batch/v1
	kind: CronJob
	metadata:
	  name: apprafter-backup-check
	  namespace: apprafter-system
	  labels:
	    apprafter.io/managed-by: apprafter
	    apprafter.io/source: platform-stack
	spec:
	  schedule: {{ $b.checkSchedule | default "0 6 * * 0" | quote }}
	  {{- with $b.timeZone }}
	  timeZone: {{ . | quote }}
	  {{- end }}
	  concurrencyPolicy: Forbid
	  successfulJobsHistoryLimit: 3
	  failedJobsHistoryLimit: 3
	  jobTemplate:
	    spec:
	      template:
	        metadata:
	          labels:
	            apprafter.io/backup-runner: "true"
	        spec:
	          serviceAccountName: apprafter-backup
	          restartPolicy: Never
	          containers:
	          - name: check
	            image: {{ $b.image | quote }}
	            # The chunk-2 runner binary has no check-only mode, so the check Job
	            # runs restic directly (the runner image bundles restic + a shell).
	            # restic reads RESTIC_PASSWORD + AWS_* from the explicit secretKeyRef
	            # entries below (Secret holds neutral S3_* keys) and the s3: repo from
	            # APPRAFTER_BACKUP_REPO.
	            command: ["sh", "-c"]
	            args:
	            - >-
	              restic -r "$APPRAFTER_BACKUP_REPO" unlock;
	              restic -r "$APPRAFTER_BACKUP_REPO" check{{ if $b.checkReadData }} --read-data{{ end }}
	            env:
	            - name: RESTIC_PASSWORD
	              valueFrom:
	                secretKeyRef:
	                  name: {{ $b.credentialRef.name | quote }}
	                  key: RESTIC_PASSWORD
	            - name: AWS_ACCESS_KEY_ID
	              valueFrom:
	                secretKeyRef:
	                  name: {{ $b.credentialRef.name | quote }}
	                  key: S3_ACCESS_KEY_ID
	            - name: AWS_SECRET_ACCESS_KEY
	              valueFrom:
	                secretKeyRef:
	                  name: {{ $b.credentialRef.name | quote }}
	                  key: S3_SECRET_ACCESS_KEY
	            - name: AWS_DEFAULT_REGION
	              valueFrom:
	                secretKeyRef:
	                  name: {{ $b.credentialRef.name | quote }}
	                  key: S3_REGION
	                  optional: true
	            - name: APPRAFTER_BACKUP_REPO
	              value: {{ $b.bucket | quote }}
	            resources:
	              requests:
	                cpu: 100m
	                memory: 256Mi
	              limits:
	                memory: 512Mi
	{{- end }}
	---
	# Egress policy for the backup + check runner pods (m-r3-1). A Cilium FQDN
	# policy without a DNS-allow rule silently fails to resolve and the backup
	# hangs with no clear symptom, so DNS is MANDATORY. S3 endpoint is not known at
	# render time (it's a template value), so we open world:443 pragmatically; the
	# failureWebhook host (also 443) is covered by the same rule.
	apiVersion: cilium.io/v2
	kind: CiliumNetworkPolicy
	metadata:
	  name: apprafter-backup-egress
	  namespace: apprafter-system
	  labels:
	    apprafter.io/managed-by: apprafter
	    apprafter.io/source: platform-stack
	spec:
	  endpointSelector:
	    matchLabels:
	      apprafter.io/backup-runner: "true"
	  egress:
	  # DNS → kube-dns (UDP+TCP/53) with FQDN visibility. REQUIRED (m-r3-1):
	  # without the dns visibility rule a toFQDNs policy silently never resolves.
	  - toEndpoints:
	    - matchLabels:
	        io.kubernetes.pod.namespace: kube-system
	        k8s-app: kube-dns
	    toPorts:
	    - ports:
	      - port: "53"
	        protocol: UDP
	      - port: "53"
	        protocol: TCP
	      rules:
	        dns:
	        - matchPattern: "*"
	  # The runner talks to the kube-apiserver (list CRs, exec helper pods, write
	  # the status ConfigMap).
	  - toEntities:
	    - kube-apiserver
	  # S3 endpoint (and, when set, the failureWebhook host) on 443.
	  # NOTE: tighten to toFQDNs when the endpoint is known.
	  - toEntities:
	    - world
	    toPorts:
	    - ports:
	      - port: "443"
	        protocol: TCP
	{{- end }}

	"""

// `_migrationAnchorTemplate` — emits the platform MigrationPlan tree
// anchor ConfigMap (ADR 0048). STATIC: always emitted, no iteration.
// The PlatformController sets each platform MigrationPlan's
// ownerReference to this ConfigMap so the plan (operator-created
// in-cluster, not repo-synced) is pulled into the platform-stack root
// Application's Argo CD resource tree, where its approve/reject action
// buttons become clickable. MUST be in apprafter-system (same namespace
// as the plan) — a cross-namespace ownerReference makes k8s GC silently
// delete the plan. sync-wave 5 lands it AFTER the operator/webhook
// create apprafter-system at wave 0 — mirroring the positive sync-wave
// the top-level ServiceProvider seeds carry. This is a top-level
// umbrella resource (a Go template emitted by the renderer), NOT a child
// Application, so the ConfigMap is a resource of the platform-stack ROOT
// Application — exactly where the MigrationPlan needs to be anchored.
//
// Note the comment is a literal Go-template comment Helm strips at
// install time; CUE ships the whole string verbatim.
_migrationAnchorTemplate: """
	{{/* SPDX-License-Identifier: FSL-1.1-Apache-2.0
	     Rendered by `cue cmd render`. Do not edit.
	     Tree anchor (ADR 0048): the PlatformController sets each platform
	     MigrationPlan's ownerReference to this ConfigMap so the plan (operator-
	     created, not repo-synced) is pulled into the platform-stack root
	     Application's resource tree, where its approve/reject buttons become
	     clickable. MUST be in apprafter-system (same namespace as the plan) —
	     a cross-namespace ownerRef makes k8s GC silently delete the plan.
	     sync-wave 5: after the operator/webhook create apprafter-system (wave 0). */}}
	---
	apiVersion: v1
	kind: ConfigMap
	metadata:
	  name: platform-migration-anchor
	  namespace: apprafter-system
	  annotations:
	    argocd.argoproj.io/sync-wave: "5"
	  labels:
	    apprafter.io/role: platform-migration-anchor
	    apprafter.io/managed-by: apprafter
	    apprafter.io/source: platform-stack
	data:
	  purpose: platform MigrationPlan tree anchor (see ADR 0048)

	"""

_applicationsTemplate: """
	{{/*
	  SPDX-License-Identifier: FSL-1.1-Apache-2.0
	  Rendered by `cue cmd render`. Do not edit.
	  Iterates over .Values.components, applying any
	  per-component override from .Values.overrides, and emits
	  one Argo CD Application per enabled entry.
	*/}}
	{{- $overrides := default (dict) $.Values.overrides }}
	{{- range $name, $component := .Values.components }}
	{{- $override := default (dict) (index $overrides $name) }}
	{{- $enabled := $component.enabled }}
	{{- if hasKey $override "enabled" }}
	{{- $enabled = $override.enabled }}
	{{- end }}
	{{- if $enabled }}
	{{- $version := $component.version }}
	{{- if hasKey $override "pin" }}
	{{- $version = $override.pin }}
	{{- end }}
	{{- $componentValues := default (dict) $component.values }}
	{{- $overrideValues := default (dict) $override.values }}
	{{- $values := mergeOverwrite (deepCopy $componentValues) $overrideValues }}
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
	  project: {{ default "platform" $component.project | quote }}
	  source:
	    repoURL: {{ $component.source.repoURL | quote }}
	    {{- with $component.source.chart }}
	    chart: {{ . | quote }}
	    {{- end }}
	    {{- with $component.source.path }}
	    path: {{ . | quote }}
	    {{- end }}
	    targetRevision: {{ $version | quote }}
	    {{- if $component.source.chart }}
	    helm:
	      valuesObject:
	{{ toYaml $values | indent 8 }}
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
		overrides: {
			type: "object"
			patternProperties: {
				"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$": {
					type: "object"
					properties: {
						pin: {type: "string"}
						values: {type: "object"}
						enabled: {type: "boolean"}
					}
					additionalProperties: false
				}
			}
			additionalProperties: false
		}
		// Unlike `appProjects` (deliberately omitted from this
		// schema — it's umbrella-internal data), the seed shape IS
		// validated at Helm-install time: operators may hand-author
		// extra providers via `--set`/values, so a malformed entry
		// should fail install rather than render a broken CR.
		serviceProviders: {
			type: "object"
			patternProperties: {
				"^[a-z0-9][a-z0-9-]{0,62}[a-z0-9]$": {
					type: "object"
					required: ["labels", "type", "backend", "config"]
					properties: {
						namespace: {type: "string"}
						labels: {type: "object"}
						type: {type: "string"}
						backend: {type: "string"}
						config: {type: "object"}
						syncWave: {type: "integer"}
					}
				}
			}
			additionalProperties: false
		}
		// Off-site scheduled-backup config (2.6d-4). Default-off; the
		// operator's PlatformController projects
		// `PlatformStack.spec.backup` onto this key. Secrets never live
		// here — only `credentialRef.name` names a Secret in
		// `apprafter-system`. Validated at Helm-install time so a
		// malformed override (e.g. a non-enum `stagingMode`) fails
		// install rather than rendering a broken CronJob.
		backup: {
			type: "object"
			properties: {
				enabled: {type: "boolean"}
				image: {type: "string"}
				schedule: {type: "string"}
				bucket: {type: "string"}
				credentialRef: {
					type: "object"
					properties: name: {type: "string"}
				}
				stagingMode: {
					type: "string"
					enum: ["monolithic", "sequential"]
				}
				stagingSizeLimit: {type: "string"}
				retention: {
					type: "object"
					properties: {
						keepDaily: {type: "integer"}
						keepWeekly: {type: "integer"}
						keepMonthly: {type: "integer"}
						enforce: {
							type: "string"
							enum: ["operator", "cluster"]
						}
					}
				}
				checkSchedule: {type: "string"}
				checkReadData: {type: "boolean"}
				failureWebhook: {type: "string"}
			}
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
	[`platform-stack/cue/`](https://github.com/apprafter/apprafter/tree/master/platform-stack/cue)
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

	appProjectsTemplate: file.Create & {
		filename: "\(_distDir)/templates/appprojects.yaml"
		contents: _appProjectsTemplate
		$dep:     mktemplates.$done
	}

	serviceProvidersTemplate: file.Create & {
		filename: "\(_distDir)/templates/serviceproviders.yaml"
		contents: _serviceProvidersTemplate
		$dep:     mktemplates.$done
	}

	gatewayTemplate: file.Create & {
		filename: "\(_distDir)/templates/gateway.yaml"
		contents: _gatewayTemplate
		$dep:     mktemplates.$done
	}

	backupTemplate: file.Create & {
		filename: "\(_distDir)/templates/backup.yaml"
		contents: _backupTemplate
		$dep:     mktemplates.$done
	}

	migrationAnchorTemplate: file.Create & {
		filename: "\(_distDir)/templates/platform-migration-anchor.yaml"
		contents: _migrationAnchorTemplate
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
