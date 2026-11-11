#!/usr/bin/env bash
# Result cache benchmark with pgbench. Each workload runs against Postgres
# directly, PgDog without the cache, PgDog with it, and with it encrypted,
# and the results are printed as a Markdown table.
#
#   bench/result_cache/run.sh
#
# Workloads:
#   lookup  one row by primary key, keys skewed (zipfian): many repeat
#   report  aggregate over one branch's accounts (a scan): few distinct keys
#   mixed   report, with 1 tpcb-like write transaction in 10, which writes
#           the scanned table: every write invalidates the reports
#
# Needs pgbench, psql and curl, Postgres where PGUSER can create databases,
# and Redis. In the dev container, both are already running.
#
# Environment (defaults in brackets):
#   PGDOG_BIN  PgDog binary [builds target/release/pgdog]
#   PGHOST, PGPORT, PGUSER, PGPASSWORD  Postgres [127.0.0.1, 5432, pgdog, pgdog]
#   REDIS_URL  [redis://127.0.0.1:6379]
#   SCALE      pgbench scale, 100k accounts each [10]
#   CLIENTS, JOBS  pgbench -c and -j [16, 4]
#   DURATION   seconds measured per run [30]
#   WARMUP     seconds run before each measurement [10]
#   WORKLOADS, TARGETS  subsets to run [all]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PGHOST="${PGHOST:-127.0.0.1}" PGPORT="${PGPORT:-5432}"
export PGUSER="${PGUSER:-pgdog}" PGPASSWORD="${PGPASSWORD:-pgdog}"
REDIS_URL="${REDIS_URL:-redis://127.0.0.1:6379}"
SCALE="${SCALE:-10}"
CLIENTS="${CLIENTS:-16}"
JOBS="${JOBS:-4}"
DURATION="${DURATION:-30}"
WARMUP="${WARMUP:-10}"
WORKLOADS="${WORKLOADS:-lookup report mixed}"
TARGETS="${TARGETS:-postgres pgdog cache cache-encrypted}"
DB=pgdog_bench
PGDOG_PORT=6440
METRICS_PORT=9940

if [ -z "${PGDOG_BIN:-}" ]; then
  cargo build --release --bin pgdog --manifest-path "$ROOT/Cargo.toml"
  PGDOG_BIN="${CARGO_TARGET_DIR:-$ROOT/target}/release/pgdog"
fi

work="$(mktemp -d)"
pgdog_pid=""
cleanup() {
  [ -n "$pgdog_pid" ] && kill "$pgdog_pid" 2>/dev/null && wait "$pgdog_pid" 2>/dev/null
  rm -rf "$work"
}
trap cleanup EXIT

cat > "$work/lookup.sql" <<'EOF'
\set aid random_zipfian(1, 100000 * :scale, 1.1)
SELECT abalance FROM pgbench_accounts WHERE aid = :aid;
EOF
cat > "$work/report.sql" <<'EOF'
\set bid random(1, :scale)
SELECT count(*), sum(abalance), avg(abalance) FROM pgbench_accounts WHERE bid = :bid;
EOF

workload_args() {
  case "$1" in
    lookup) echo "-f $work/lookup.sql" ;;
    report) echo "-f $work/report.sql" ;;
    mixed) echo "-f $work/report.sql@9 -b tpcb-like@1" ;;
  esac
}

echo "==> pgbench -i -s $SCALE ($DB)" >&2
psql -q -d postgres -c "DROP DATABASE IF EXISTS $DB" -c "CREATE DATABASE $DB"
pgbench -i -q -s "$SCALE" "$DB" >&2

cat > "$work/users.toml" <<EOF
[[users]]
name = "$PGUSER"
password = "$PGPASSWORD"
database = "$DB"
EOF

# PgDog for the target, listening on $PGDOG_PORT.
start_pgdog() {
  local target=$1
  cat > "$work/pgdog.toml" <<EOF
[general]
host = "127.0.0.1"
port = $PGDOG_PORT
openmetrics_port = $METRICS_PORT
workers = 4
default_pool_size = $CLIENTS

[[databases]]
name = "$DB"
host = "$PGHOST"
port = $PGPORT
EOF
  if [ "$target" != pgdog ]; then
    cat >> "$work/pgdog.toml" <<EOF

[result_cache]
enabled = true
redis_url = "$REDIS_URL"
key_prefix = "pgdog:bench:$target:$$:$RANDOM"
expire_seconds = 60
EOF
  fi
  if [ "$target" = cache-encrypted ]; then
    echo "encryption_key = \"$(head -c 32 /dev/urandom | base64)\"" >> "$work/pgdog.toml"
  fi

  "$PGDOG_BIN" --config "$work/pgdog.toml" --users "$work/users.toml" > "$work/pgdog.log" 2>&1 &
  pgdog_pid=$!
  for _ in $(seq 100); do
    pg_isready -q -h 127.0.0.1 -p "$PGDOG_PORT" && return
    sleep 0.1
  done
  echo "PgDog didn't start:" >&2
  cat "$work/pgdog.log" >&2
  exit 1
}

stop_pgdog() {
  kill "$pgdog_pid" 2>/dev/null || true
  wait "$pgdog_pid" 2>/dev/null || true
  pgdog_pid=""
}

metric() {
  curl -s "http://127.0.0.1:$METRICS_PORT/metrics" |
    awk -v name="$1" '$1 == name || index($1, name "{") == 1 { sum += $2 } END { print sum + 0 }'
}

# One measured pgbench run: prints "tps latency_ms".
measure() {
  local port=$1 workload=$2 out
  # shellcheck disable=SC2046 # several arguments
  pgbench -n -M prepared -c "$CLIENTS" -j "$JOBS" -T "$WARMUP" -h 127.0.0.1 -p "$port" \
    $(workload_args "$workload") "$DB" > /dev/null 2>&1
  # shellcheck disable=SC2046
  out=$(pgbench -n -M prepared -c "$CLIENTS" -j "$JOBS" -T "$DURATION" -h 127.0.0.1 -p "$port" \
    $(workload_args "$workload") "$DB" 2>&1)
  echo "$out" | awk '/^tps = / { tps = $3 } /^latency average = / { lat = $4 } END { print tps, lat }'
}

declare -A tps_of
results=()
for workload in $WORKLOADS; do
  for target in $TARGETS; do
    echo "==> $workload: $target" >&2
    hit_ratio="-"
    if [ "$target" = postgres ]; then
      read -r tps lat < <(measure "$PGPORT" "$workload")
    else
      start_pgdog "$target"
      read -r tps lat < <(measure "$PGDOG_PORT" "$workload")
      if [ "$target" != pgdog ]; then
        # Since PgDog started: warm-up included.
        hits=$(metric result_cache_hits)
        misses=$(metric result_cache_misses)
        hit_ratio=$(awk -v h="$hits" -v m="$misses" 'BEGIN { if (h + m) printf "%.0f%%", 100 * h / (h + m); else print "-" }')
      fi
      stop_pgdog
    fi
    tps_of[$workload:$target]=$tps
    results+=("$workload|$target|$tps|$lat|$hit_ratio")
  done
done

echo
echo "pgbench -M prepared, scale $SCALE, $CLIENTS clients, $JOBS threads, ${DURATION}s per run after ${WARMUP}s of warm-up."
echo
echo "| Workload | Target | TPS | vs Postgres | Avg latency (ms) | Cache hits |"
echo "|---|---|---:|---:|---:|---:|"
for row in "${results[@]}"; do
  IFS='|' read -r workload target tps lat hit_ratio <<< "$row"
  base="${tps_of[$workload:postgres]:-}"
  ratio=$(awk -v t="$tps" -v b="$base" 'BEGIN { if (b > 0) printf "%.2fx", t / b; else print "-" }')
  printf '| %s | %s | %.0f | %s | %.3f | %s |\n' "$workload" "$target" "$tps" "$ratio" "$lat" "$hit_ratio"
done
