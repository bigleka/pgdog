#!/usr/bin/env bash
# Start every Redis topology the result cache supports, for tests:
#   standalone  127.0.0.1:6379
#   cluster     127.0.0.1:7000-7002 (3 primaries, no replicas)
#   sentinel    127.0.0.1:26379, master "pgdog-cache" at 127.0.0.1:6380
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
cat > "$DIR/sentinel.conf" <<CONF
port 26379
bind 127.0.0.1
sentinel monitor pgdog-cache 127.0.0.1 6380 1
sentinel down-after-milliseconds pgdog-cache 1000
sentinel failover-timeout pgdog-cache 5000
CONF
redis-cli -p 26379 ping >/dev/null 2>&1 || redis-server "$DIR/sentinel.conf" --sentinel --daemonize yes >/dev/null

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

wait_for redis-cli -p 26379 sentinel get-master-addr-by-name pgdog-cache
wait_for redis-cli -p 22121 set pgdog:probe 1
