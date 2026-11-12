#!/bin/bash
# End-to-end test of the chart in the current kubectl context (CI: kind).
# Installs Postgres and the chart with two PgDogs and the bundled Valkey,
# then checks that a query is cached, that a hit is served by PgDog, and
# that Valkey holds no readable result.
#   test/e2e/run.sh [image tag]
set -euo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHART_DIR="$E2E_DIR/../.."
ns=pgdog-e2e
tag="${1:-main}"

kubectl create namespace "$ns" --dry-run=client -o yaml | kubectl apply -f -
kubectl -n "$ns" apply -f "$E2E_DIR/postgres.yaml"
kubectl -n "$ns" create secret generic pgdog-cache-key \
  --from-literal=encryption-key="e2e-$(date +%s%N)" --dry-run=client -o yaml | kubectl apply -f -
kubectl -n "$ns" rollout status deployment/postgres --timeout=180s

helm upgrade --install pgdog "$CHART_DIR" -n "$ns" -f "$E2E_DIR/values.yaml" \
  --set image.tag="$tag" --wait --timeout 180s
kubectl -n "$ns" rollout status deployment/pgdog-valkey --timeout=120s

psql() {
  kubectl -n "$ns" run "psql-$RANDOM" --rm -i --restart=Never --quiet \
    --image=docker.io/library/postgres:17 --env=PGPASSWORD=secret -- \
    psql -h pgdog -p 6432 -U app app -At -c "$1"
}
metric() {
  # Sum over both PgDogs.
  local total=0
  for pod in $(kubectl -n "$ns" get pods -l app.kubernetes.io/component=pgdog -o name); do
    value=$(kubectl -n "$ns" exec "$pod" -c pgdog -- curl -s localhost:9090/metrics \
      | awk -v m="$1" '$1 == m { print $2 }')
    total=$((total + ${value:-0}))
  done
  echo "$total"
}
fail() { echo "FAIL: $*"; kubectl -n "$ns" logs -l app.kubernetes.io/component=pgdog -c pgdog --tail=30 || true; exit 1; }

psql "CREATE TABLE IF NOT EXISTS clientes (id int PRIMARY KEY, cpf text)" >/dev/null
psql "INSERT INTO clientes VALUES (1, 'CPF-123.456.789-00') ON CONFLICT DO NOTHING" >/dev/null
query="SELECT cpf FROM clientes WHERE id = 1"
for _ in 1 2 3 4; do
  [ "$(psql "$query")" = "CPF-123.456.789-00" ] || fail "wrong result"
done
hits=$(metric result_cache_hits)
echo "result_cache_hits=$hits result_cache_misses=$(metric result_cache_misses)"
# Four reads, spread over two PgDogs sharing one cache: at most one miss
# each before the entry is in Valkey.
[ "$hits" -ge 2 ] || fail "expected cache hits, got $hits"

entries=$(kubectl -n "$ns" exec deploy/pgdog-valkey -- sh -c \
  "valkey-cli --scan --pattern 'pgdog:result_cache:v3:*' | while read -r k; do valkey-cli --raw GET \"\$k\"; done")
[ -n "$entries" ] || fail "no entry in Valkey"
if grep -qa "CPF-123" <<<"$entries"; then fail "result readable in Valkey"; fi

psql "UPDATE clientes SET cpf = 'outro' WHERE id = 1" >/dev/null
[ "$(psql "$query")" = "outro" ] || fail "write through PgDog didn't invalidate the cache"
echo "e2e OK"
