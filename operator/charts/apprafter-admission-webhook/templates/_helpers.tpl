{{/* SPDX-License-Identifier: MIT */}}
{{- define "apprafter-admission-webhook.name" -}}
admission-webhook
{{- end }}

{{- define "apprafter-admission-webhook.fullname" -}}
admission-webhook
{{- end }}

{{- define "apprafter-admission-webhook.labels" -}}
app.kubernetes.io/name: {{ include "apprafter-admission-webhook.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
apprafter: "true"
{{- end }}

{{- define "apprafter-admission-webhook.selectorLabels" -}}
app: {{ include "apprafter-admission-webhook.name" . }}
{{- end }}

{{- define "apprafter-admission-webhook.tlsSecretName" -}}
admission-webhook-tls
{{- end }}
