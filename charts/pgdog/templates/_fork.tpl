{{/*
This chart comes from upstream (pgdogdev/helm, see UPSTREAM.md), which
follows upstream PgDog, including settings this fork's PgDog (on upstream
0.1.64) doesn't have: it would refuse to start with them. Rendering fails
instead, naming the values. Remove an entry when the fork gets the setting.
*/}}
{{- define "pgdog.forkUnsupported" -}}
{{- $v := .Values -}}
{{- $unsupported := list -}}
{{- if not $v.configSecret.name -}}
{{- range $key := list "rewriteShardKeyUpdates" "autodiscovery" "qos" -}}
{{- if hasKey $v $key -}}{{- $unsupported = append $unsupported $key -}}{{- end -}}
{{- end -}}
{{- if and $v.queryStats $v.queryStats.enabled -}}{{- $unsupported = append $unsupported "queryStats.enabled" -}}{{- end -}}
{{- if and $v.control $v.control.enabled -}}{{- $unsupported = append $unsupported "control.enabled" -}}{{- end -}}
{{- end -}}
{{- if $unsupported -}}
{{- fail (printf "not supported by this PgDog fork (bigleka/pgdog) yet: %s" (join ", " (uniq $unsupported))) -}}
{{- end -}}
{{- end -}}
