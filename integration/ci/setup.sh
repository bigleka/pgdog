#!/usr/bin/env bash
# Bootstrap PostgreSQL + pgdog's test fixtures on a Blacksmith runner.
# Called from workflow jobs after install-deps.sh.
#
# Usage: integration/ci/setup.sh [--with-toxi]
set -euo pipefail

SCRIPT_DIR="$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )"
REPO_ROOT="$( cd "${SCRIPT_DIR}/../.." && pwd )"

# Use whatever pg cluster is installed on the runner unless overridden.
if [[ -z "${PSQL_VERSION:-}" ]]; then
    PSQL_VERSION=$(ls -d /etc/postgresql/*/main 2>/dev/null \
        | sed 's|.*/postgresql/\([0-9]\+\)/main|\1|' \
        | sort -n | tail -1 || true)
fi
if [[ -z "${PSQL_VERSION:-}" ]]; then
    echo "No PostgreSQL cluster found under /etc/postgresql" >&2
    exit 1
fi

with_toxi=false
for arg in "$@"; do
    case "$arg" in
        --with-toxi) with_toxi=true ;;
        *) echo "unknown arg: $arg" >&2; exit 1 ;;
    esac
done

sudo pg_ctlcluster "$PSQL_VERSION" main start

# Result cache tests: standalone Redis, Redis Cluster, Sentinel and twemproxy.
bash "${REPO_ROOT}/integration/ci/redis-topologies.sh"

USER_NAME="$(id -un)"
sudo -u postgres createuser --superuser --login "$USER_NAME"
sudo -u postgres createdb "$USER_NAME"

# asyncpg's type introspection query costs above jit_above_cost, and JIT
# makes it about 100x slower on a cold backend (100 ms vs 1 ms locally).
# On busy 2-vCPU runners that exceeds the 2 s query_timeout. Takes effect
# on new connections, no restart needed.
psql -d postgres -c "ALTER SYSTEM SET jit TO off" -c "SELECT pg_reload_conf()"

bash "${REPO_ROOT}/integration/setup.sh" \
    || (sudo pg_ctlcluster "$PSQL_VERSION" main restart && bash "${REPO_ROOT}/integration/setup.sh")

if [[ "$with_toxi" == true ]]; then
    bash "${REPO_ROOT}/integration/toxi/setup.sh"
fi
