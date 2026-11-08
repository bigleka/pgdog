#!/usr/bin/env bash
# Start every Redis topology the result cache supports, for tests:
#   standalone  127.0.0.1:6379
#   cluster     127.0.0.1:7000-7002 (3 primaries, no replicas)
#   sentinel    127.0.0.1:26379, masters "pgdog-cache" (127.0.0.1:6380) and
#               "pgdog-failover" (127.0.0.1:6384 + replica :6385, failed over by tests)
#   twemproxy   127.0.0.1:22121, sharding over 127.0.0.1:6381 and :6382
set -euo pipefail
DIR=${REDIS_TOPOLOGIES_DIR:-/tmp/redis-topologies}
mkdir -p "$DIR"

# Retry "$@" until it succeeds, for up to 30s.
wait_for() {
    local deadline=$((SECONDS + 30))
    until "$@" >/dev/null 2>&1; do
        (( SECONDS < deadline )) || { echo "redis-topologies: timed out waiting for: $*" >&2; exit 1; }
        sleep 0.2
    done
}

# Start a Redis on port $1 with extra args, unless one already answers there
# (CI runners start redis-server on 6379 when the package is installed).
plain() {
    redis-cli -p "$1" ping >/dev/null 2>&1 && return 0
    redis-server --port "$1" --bind 127.0.0.1 --save "" --appendonly no --daemonize yes --dir "$DIR" "${@:2}" >/dev/null
}

plain 6379

for port in 7000 7001 7002; do
    plain $port --cluster-enabled yes --cluster-config-file "$DIR/nodes-$port.conf"
done
for port in 6379 7000 7001 7002; do
    wait_for redis-cli -p $port ping
done
if ! redis-cli -p 7000 cluster info | grep -q cluster_state:ok; then
    redis-cli --cluster create 127.0.0.1:7000 127.0.0.1:7001 127.0.0.1:7002 \
        --cluster-replicas 0 --cluster-yes >/dev/null
fi
wait_for bash -c "redis-cli -p 7000 cluster info | grep -q cluster_state:ok"

plain 6380
plain 6384
plain 6385 --replicaof 127.0.0.1 6384
# A Sentinel may already run on 26379 (the Ubuntu redis-sentinel package
# starts one), so register our primaries at runtime instead of in its config.
cat > "$DIR/sentinel.conf" <<CONF
port 26379
bind 127.0.0.1
CONF
redis-cli -p 26379 ping >/dev/null 2>&1 || redis-server "$DIR/sentinel.conf" --sentinel --daemonize yes >/dev/null
wait_for redis-cli -p 26379 ping
monitor() {
    local name=$1 port=$2
    if ! redis-cli -p 26379 sentinel master "$name" >/dev/null 2>&1 \
        || redis-cli -p 26379 sentinel master "$name" | grep -q "No such master"; then
        redis-cli -p 26379 sentinel monitor "$name" 127.0.0.1 "$port" 1 >/dev/null
    fi
    redis-cli -p 26379 sentinel set "$name" down-after-milliseconds 1000 >/dev/null
    redis-cli -p 26379 sentinel set "$name" failover-timeout 5000 >/dev/null
}
monitor pgdog-cache 6380
monitor pgdog-failover 6384

plain 6381
plain 6382
cat > "$DIR/nutcracker.yml" <<CONF
cache:
  listen: 127.0.0.1:22121
  hash: fnv1a_64
  distribution: ketama
  redis: true
  auto_eject_hosts: false
  servers:
    - 127.0.0.1:6381:1
    - 127.0.0.1:6382:1
CONF
redis-cli -p 22121 ping >/dev/null 2>&1 \
    || nutcracker -c "$DIR/nutcracker.yml" -d -o "$DIR/nutcracker.log" -p "$DIR/nutcracker.pid"

# Exits 0 even for an unknown master: check it returns an address.
wait_for bash -c "redis-cli -p 26379 sentinel get-master-addr-by-name pgdog-cache | grep -q 6380"
# Failover needs Sentinel to know the replica (it discovers them via INFO).
# After an earlier failover the roles may be swapped: either port will do.
wait_for bash -c "redis-cli -p 26379 sentinel replicas pgdog-failover | grep -qE '638[45]'"
wait_for redis-cli -p 22121 set pgdog:probe 1
