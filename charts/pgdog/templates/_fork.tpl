{{/*
This chart comes from upstream (pgdogdev/helm, see UPSTREAM.md), which
follows upstream PgDog. This fork forked PgDog at 0.1.38 and doesn't
support some of the settings the upstream chart can render: its PgDog
would refuse to start with them. Rendering fails instead, naming the
values. Remove an entry when the feature is ported to the fork.
*/}}
{{- define "pgdog.forkUnsupported" -}}
{{- $v := .Values -}}
{{- $unsupported := list -}}
{{- if not $v.configSecret.name -}}
{{- range $key := list "listenBacklog" "queryLogStdout" "requireHealthcheckOnDiscovery" "logMinDurationParse" "logQuerySampleLength" "twoPhaseCommitWalDir" "twoPhaseCommitWalSegmentSize" "twoPhaseCommitWalFsyncInterval" "twoPhaseCommitWalCheckpointInterval" "authTokenCacheSize" "serverLifetimeJitter" "rewriteShardKeyUpdates" "autodiscovery" "vault" "otel" "qos" -}}
{{- if hasKey $v $key -}}{{- $unsupported = append $unsupported $key -}}{{- end -}}
{{- end -}}
{{- range $key := list "tlsClientCaCertificate" "tlsServerCertificate" "tlsServerPrivateKey" "queryParsers" -}}
{{- if index $v $key -}}{{- $unsupported = append $unsupported $key -}}{{- end -}}
{{- end -}}
{{- if and $v.queryStats $v.queryStats.enabled -}}{{- $unsupported = append $unsupported "queryStats.enabled" -}}{{- end -}}
{{- if and $v.control $v.control.enabled -}}{{- $unsupported = append $unsupported "control.enabled" -}}{{- end -}}
{{- if and $v.rewrite (hasKey $v.rewrite "nonDeterministicFunctions") -}}{{- $unsupported = append $unsupported "rewrite.nonDeterministicFunctions" -}}{{- end -}}
{{- if and $v.statefulSet.enabled $v.statefulSet.walPvc.enabled -}}{{- $unsupported = append $unsupported "statefulSet.walPvc.enabled (two-phase commit WAL)" -}}{{- end -}}
{{- range $v.databases -}}
{{- if or .tlsServerCertificate .tlsServerPrivateKey -}}{{- $unsupported = append $unsupported (printf "databases[%s].tlsServerCertificate/tlsServerPrivateKey" .name) -}}{{- end -}}
{{- end -}}
{{- range $v.shardedTables -}}
{{- if .mapping -}}{{- $unsupported = append $unsupported (printf "shardedTables[%s].mapping (use shardedMappings)" .name) -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- if and (not $v.usersSecret.name) (not $v.externalSecrets.enabled) -}}
{{- range $v.users -}}
{{- $user := . -}}
{{- range $key := list "authType" "identity" "readWriteSplit" "serverIamAssumeRole" "serverVaultPath" "vaultRefreshPercent" "vaultPath" -}}
{{- if hasKey $user $key -}}{{- $unsupported = append $unsupported (printf "users[%s].%s" $user.name $key) -}}{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- if $unsupported -}}
{{- fail (printf "not supported by this PgDog fork (bigleka/pgdog) yet: %s" (join ", " (uniq $unsupported))) -}}
{{- end -}}
{{- end -}}
