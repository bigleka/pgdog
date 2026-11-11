#!/bin/bash
set -eo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHART_DIR="$TEST_DIR/.."

echo "==> Linting Helm chart..."
helm lint "$CHART_DIR"

for values_file in "$TEST_DIR"/values-*.yaml; do
  name=$(basename "$values_file" .yaml | sed 's/values-//')
  echo ""
  echo "==> Templating and validating $name..."
  helm template test-release "$CHART_DIR" -f "$values_file" | kubeconform -strict -ignore-missing-schemas -summary
done

# Validate multiple passwords renders valid TOML
echo ""
echo "==> Validating multiple passwords TOML output..."
users_toml=$(helm template test-release "$CHART_DIR" -f "$TEST_DIR/values-multiple-passwords.yaml" \
  | yq -r 'select(.kind == "Secret" and .metadata.name == "test-release-pgdog") | .data["users.toml"]' \
  | base64 -d)

if echo "$users_toml" | grep -q 'passwords = \["one", "two"\]'; then
  echo "  passwords array rendered correctly"
else
  echo "  FAIL: passwords array not rendered correctly"
  echo "  Got: $users_toml"
  exit 1
fi

if echo "$users_toml" | grep -q 'password = "single_password"'; then
  echo "  single password rendered correctly"
else
  echo "  FAIL: single password not rendered correctly"
  echo "  Got: $users_toml"
  exit 1
fi

echo "==> Validating PodDisruptionBudget zero values..."

pdb_spec() {
  helm template test-release "$CHART_DIR" -f "$1" \
    | yq -r 'select(.kind == "PodDisruptionBudget") | .spec | del(.selector)'
}

min_available=$(pdb_spec "$TEST_DIR/values-pdb-min-available-zero.yaml")

if [ "$min_available" = "minAvailable: 0" ]; then
  echo "  minAvailable: 0 rendered correctly"
else
  echo "  FAIL: minAvailable: 0 not rendered correctly"
  echo "  Got: $min_available"
  exit 1
fi

max_unavailable=$(pdb_spec "$TEST_DIR/values-pdb-max-unavailable-zero.yaml")

if [ "$max_unavailable" = "maxUnavailable: 0" ]; then
  echo "  maxUnavailable: 0 rendered correctly"
else
  echo "  FAIL: maxUnavailable: 0 not rendered correctly"
  echo "  Got: $max_unavailable"
  exit 1
fi

echo ""
echo "==> Validating pod extension points..."
pod_spec() {
  helm template test-release "$CHART_DIR" -f "$@" \
    | yq -o=json -I=0 'select(.kind == "Deployment") | .spec.template.spec | {"automount": .automountServiceAccountToken, "shareProcessNamespace": .shareProcessNamespace, "containers": (.containers | map(.name)), "lifecycle": .containers[0].lifecycle}'
}
service_ports() {
  helm template test-release "$CHART_DIR" -f "$@" \
    | yq -o=json -I=0 'select(.kind == "Service" and .metadata.name == "test-release-pgdog") | .spec.ports | map(.name)'
}
expect() {
  if [ "$2" != "$3" ]; then
    echo "  FAIL: $1"
    echo "  Expected: $3"
    echo "  Got:      $2"
    exit 1
  fi
}
expect_fail() {
  if helm template test-release "$CHART_DIR" "${@:2}" >/dev/null 2>&1; then
    echo "  FAIL: rendered although $1"
    exit 1
  fi
}

expect "extension points render as set" "$(pod_spec "$TEST_DIR/values-pod-extensions.yaml")" \
  '{"automount":false,"shareProcessNamespace":true,"containers":["pgdog","reloader"],"lifecycle":{"preStop":{"exec":{"command":["/bin/sh","-c","sleep 30"]}}}}'
expect "defaults render none of them" "$(pod_spec "$TEST_DIR/values-default.yaml")" \
  '{"automount":null,"shareProcessNamespace":null,"containers":["pgdog"],"lifecycle":null}'
expect "preStopSleepSeconds still renders its preStop" "$(pod_spec "$TEST_DIR/values-default.yaml" --set preStopSleepSeconds=25 | yq -o=json -I=0 .lifecycle)" \
  '{"preStop":{"sleep":{"seconds":25}}}'
expect "automountServiceAccountToken true renders" "$(pod_spec "$TEST_DIR/values-default.yaml" --set automountServiceAccountToken=true | yq .automount)" 'true'
expect "extraContainers follow the prometheus sidecar" "$(pod_spec "$TEST_DIR/values-pod-extensions.yaml" --set prometheusPort=9091 | yq -o=json -I=0 .containers)" \
  '["pgdog","prom","reloader"]'

expect "exposeMetrics false drops the metrics port" "$(service_ports "$TEST_DIR/values-pod-extensions.yaml")" '["pgdog"]'
expect "metrics port is on the Service by default" "$(service_ports "$TEST_DIR/values-default.yaml")" '["pgdog","openmetrics"]'
# helm upgrade --reuse-values keeps the old release's values, which lack the key.
expect "missing exposeMetrics keeps the metrics port" "$(service_ports "$TEST_DIR/values-default.yaml" --set service.exposeMetrics=null)" '["pgdog","openmetrics"]'
helm template test-release "$CHART_DIR" -f "$TEST_DIR/values-monitoring.yaml" --set service.exposeMetrics=null >/dev/null
helm template test-release "$CHART_DIR" -f "$TEST_DIR/values-monitoring.yaml" --set statefulSet.enabled=true --set service.exposeMetrics=false >/dev/null

expect_fail "serviceMonitor has no Service metrics port" -f "$TEST_DIR/values-monitoring.yaml" --set service.exposeMetrics=false
expect_fail "lifecycle and preStopSleepSeconds are both set" -f "$TEST_DIR/values-pod-extensions.yaml" --set preStopSleepSeconds=25
helm template test-release "$CHART_DIR" --set-json 'lifecycle={"postStart":{"exec":{"command":["true"]}}}' >/dev/null
expect_fail "lifecycle.preStop has no explicit terminationGracePeriodSeconds" -f "$TEST_DIR/values-pod-extensions.yaml" --set terminationGracePeriodSeconds=null
echo "  lifecycle, extraContainers, shareProcessNamespace, automountServiceAccountToken and service.exposeMetrics render as set, and only when set"


# --- bigleka/pgdog fork --------------------------------------------------

echo ""
echo "==> Validating settings this fork doesn't support yet..."
# Upstream's tests for them, kept verbatim: move one back up when its
# feature is ported, and drop it from templates/_fork.tpl.
for values_file in "$TEST_DIR"/unsupported/values-*.yaml; do
  name=$(basename "$values_file" .yaml | sed 's/values-//')
  if out=$(helm template test-release "$CHART_DIR" -f "$values_file" 2>&1); then
    echo "  FAIL: $name rendered, but this fork's PgDog would refuse its configuration"
    exit 1
  fi
  if ! grep -q "not supported by this PgDog fork" <<<"$out"; then
    echo "  FAIL: $name failed for another reason: $out"
    exit 1
  fi
done
# Each one alone: name in the error | helm arguments.
while IFS='|' read -r expected args; do
  [ -z "$expected" ] && continue
  # shellcheck disable=SC2086
  if out=$(eval helm template test-release "$CHART_DIR" $args 2>&1); then
    echo "  FAIL: rendered with $args"
    exit 1
  fi
  if ! grep -qF "not supported by this PgDog fork" <<<"$out" || ! grep -qF "$expected" <<<"$out"; then
    echo "  FAIL: $args: expected \"$expected\" in: $out"
    exit 1
  fi
done <<'CASES'
listenBacklog|--set listenBacklog=1024
queryLogStdout|--set queryLogStdout=true
requireHealthcheckOnDiscovery|--set requireHealthcheckOnDiscovery=true
logMinDurationParse|--set logMinDurationParse=1
logQuerySampleLength|--set logQuerySampleLength=1
twoPhaseCommitWalDir|--set twoPhaseCommitWalDir=/wal
twoPhaseCommitWalSegmentSize|--set twoPhaseCommitWalSegmentSize=1
twoPhaseCommitWalFsyncInterval|--set twoPhaseCommitWalFsyncInterval=1
twoPhaseCommitWalCheckpointInterval|--set twoPhaseCommitWalCheckpointInterval=1
authTokenCacheSize|--set authTokenCacheSize=1
serverLifetimeJitter|--set serverLifetimeJitter=1
rewriteShardKeyUpdates|--set rewriteShardKeyUpdates=error
autodiscovery|--set autodiscovery.enabled=true
vault|--set vault.url=https://vault --set vault.authMethod=kubernetes
otel|--set otel.endpoint=https://otel
qos|--set qos.enabled=true
tlsClientCaCertificate|--set tlsClientCaCertificate=/ca.pem
tlsServerCertificate|--set tlsServerCertificate=/cert.pem
tlsServerPrivateKey|--set tlsServerPrivateKey=/key.pem
queryParsers|--set-json 'queryParsers=[{"database":"app","level":"on"}]'
queryStats.enabled|--set queryStats.enabled=true
control.enabled|--set control.enabled=true
rewrite.nonDeterministicFunctions|--set rewrite.nonDeterministicFunctions=error
statefulSet.walPvc.enabled|--set statefulSet.enabled=true --set statefulSet.walPvc.enabled=true
databases[app].tlsServerCertificate|--set-json 'databases=[{"name":"app","host":"db","tlsServerCertificate":"/c.pem"}]'
shardedTables[t].mapping|--set-json 'shardedTables=[{"name":"t","database":"app","column":"id","mapping":[{"kind":"list","values":[1],"shard":0}]}]'
users[app].authType|--set-json 'users=[{"name":"app","database":"app","password":"p","authType":"md5"}]'
users[app].identity|--set-json 'users=[{"name":"app","database":"app","password":"p","identity":"x"}]'
users[app].readWriteSplit|--set-json 'users=[{"name":"app","database":"app","password":"p","readWriteSplit":"include_primary"}]'
users[app].serverIamAssumeRole|--set-json 'users=[{"name":"app","database":"app","password":"p","serverIamAssumeRole":"arn"}]'
users[app].serverVaultPath|--set-json 'users=[{"name":"app","database":"app","password":"p","serverVaultPath":"x"}]'
users[app].vaultRefreshPercent|--set-json 'users=[{"name":"app","database":"app","password":"p","vaultRefreshPercent":50}]'
users[app].vaultPath|--set-json 'users=[{"name":"app","database":"app","password":"p","vaultPath":"x"}]'
CASES
echo "  upstream-only settings are refused, by name, each one alone"

echo ""
echo "==> Validating the result cache..."
pgdog_toml() {
  helm template test-release "$CHART_DIR" "$@" \
    | yq -r 'select(.kind == "ConfigMap" and .metadata.name == "test-release-pgdog") | .data["pgdog.toml"]'
}
kinds() {
  helm template test-release "$CHART_DIR" "$@" | yq -r 'select(.kind) | .kind + "/" + .metadata.name' | sort | tr '\n' ' '
}
cache_env() {
  helm template test-release "$CHART_DIR" "$@" \
    | yq -o=json -I=0 'select(.kind == "Deployment") | .spec.template.spec.containers[0].env[] | select(.name == "PGDOG_RESULT_CACHE_ENCRYPTION_KEY")'
}
contains() {
  if ! grep -qxF "$3" <<<"$2"; then
    echo "  FAIL: $1: missing line: $3"
    echo "$2"
    exit 1
  fi
}

config=$(pgdog_toml -f "$TEST_DIR/values-result-cache.yaml")
for line in \
  '[result_cache]' \
  'enabled = true' \
  'redis_mode = "sentinel"' \
  'redis_urls = ["redis://sentinel-0.cache.svc:26379", "redis://sentinel-1.cache.svc:26379"]' \
  'redis_sentinel_master = "mymaster"' \
  'expire_seconds = 60' \
  'max_entry_bytes = 1048576' \
  'key_prefix = "app:cache"' \
  'xfetch_beta = 1.5' \
  'adaptive_ttl_max_seconds = 300' \
  'singleflight_lock_ms = 2000' \
  'read_after_write_window_ms = 1500' \
  'redis_connect_timeout_ms = 250' \
  'redis_response_timeout_ms = 300' \
  'redis_connect_retries = 2' \
  'redis_unavailable_backoff_ms = 10000' \
  'cache_safe_schema_list = ["public"]' \
  'cache_unsafe_table_list = ["audit_.*"]'; do
  contains "every setting renders" "$config" "$line"
done
expect "the encryption key comes from the Secret" "$(cache_env -f "$TEST_DIR/values-result-cache.yaml")" \
  '{"name":"PGDOG_RESULT_CACHE_ENCRYPTION_KEY","valueFrom":{"secretKeyRef":{"name":"pgdog-cache-key","key":"encryption-key"}}}'
expect "no Valkey unless asked for" "$(kinds -f "$TEST_DIR/values-result-cache.yaml" | grep -c valkey || true)" '0'

contains "the bundled Valkey is the default Redis" "$(pgdog_toml -f "$TEST_DIR/values-result-cache-valkey.yaml")" \
  'redis_url = "redis://test-release-pgdog-valkey:6379"'
expect "the bundled Valkey gets a Deployment and a Service" \
  "$(kinds -f "$TEST_DIR/values-result-cache-valkey.yaml" | grep -o '[A-Za-z]*/test-release-pgdog-valkey' | tr '\n' ' ')" \
  'Deployment/test-release-pgdog-valkey Service/test-release-pgdog-valkey '
expect "Valkey never evicts keys without a TTL" \
  "$(helm template test-release "$CHART_DIR" -f "$TEST_DIR/values-result-cache-valkey.yaml" \
    | yq -o=json -I=0 'select(.kind == "Deployment" and .metadata.name == "test-release-pgdog-valkey") | .spec.template.spec.containers[0].args')" \
  '["--save","","--appendonly","no","--maxmemory","200mb","--maxmemory-policy","volatile-lru"]'
contains "redisUrl wins over the bundled Valkey" \
  "$(pgdog_toml -f "$TEST_DIR/values-result-cache-valkey.yaml" --set resultCache.redisUrl=redis://mine:6379)" \
  'redis_url = "redis://mine:6379"'

expect "disabled: no [result_cache]" "$(pgdog_toml -f "$TEST_DIR/values-result-cache.yaml" --set resultCache.enabled=false | grep -c result_cache || true)" '0'
expect "disabled: no encryption key" "$(cache_env -f "$TEST_DIR/values-result-cache.yaml" --set resultCache.enabled=false)" ''
expect "disabled: no Valkey" "$(kinds -f "$TEST_DIR/values-result-cache-valkey.yaml" --set resultCache.enabled=false | grep -c valkey || true)" '0'
expect "default image is this fork's" \
  "$(helm template test-release "$CHART_DIR" | yq -r 'select(.kind == "Deployment") | .spec.template.spec.containers[0].image')" \
  "ghcr.io/bigleka/pgdog:$(yq -r .appVersion "$CHART_DIR/Chart.yaml")"
contains "metrics keep the names the dashboard and alerts use" "$(pgdog_toml)" 'openmetrics_namespace = ""'
echo "  [result_cache], the encryption key Secret and the bundled Valkey render as set, and only when enabled"

echo ""
echo "==> All tests passed!"
