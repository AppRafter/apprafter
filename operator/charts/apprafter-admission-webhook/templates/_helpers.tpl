{{/* SPDX-License-Identifier: MIT */}}
{{- define "apprafter-admission-webhook.name" -}}
admission-webhook
{{- end }}

{{- define "apprafter-admission-webhook.fullname" -}}
admission-webhook
{{- end }}

{{/*
Selector labels — the stable subset that goes on `Deployment.spec.selector.matchLabels`
and `Service.spec.selector`. MUST be a subset of `.labels` below; otherwise the
Kubernetes API rejects the Deployment with `selector does not match template labels`.
*/}}
{{- define "apprafter-admission-webhook.selectorLabels" -}}
app.kubernetes.io/name: {{ include "apprafter-admission-webhook.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Full label set used on `metadata.labels` of every resource AND on
`Deployment.spec.template.metadata.labels`. Includes the selector labels via
`include`, plus version / chart / managed-by metadata that selectors must NOT
match against (they change on upgrade).
*/}}
{{- define "apprafter-admission-webhook.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
{{ include "apprafter-admission-webhook.selectorLabels" . }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
apprafter: "true"
{{- end }}

{{- define "apprafter-admission-webhook.tlsSecretName" -}}
admission-webhook-tls
{{- end }}
