#!/bin/bash
# Checks that PgDog accepts the configuration the chart renders for every
# test/values-*.yaml, with `pgdog configcheck`. The chart comes from
# upstream and can render settings this fork lacks: those must fail in the
# chart (templates/_fork.tpl), never in PgDog.
#   test/configcheck.sh [image]
#   PGDOG_BIN=target/release/pgdog test/configcheck.sh
# Needs helm and yq. With PGDOG_BIN, runs that binary, e.g. one built from
# the same commit as the chart; otherwise docker or podman (ENGINE) and an
# image, by default the one the chart deploys.
set -eo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHART_DIR="$TEST_DIR/.."

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for values_file in "$TEST_DIR"/values-*.yaml; do
  name=$(basename "$values_file" .yaml | sed 's/values-//')
  mkdir -p "$work/$name"
  manifests=$(helm template t "$CHART_DIR" -f "$values_file")
  # A values file may bring its own config or users Secret: check what the chart renders.
  echo "$manifests" | yq -r 'select(.kind == "ConfigMap" and .metadata.name == "t-pgdog") | .data["pgdog.toml"]' > "$work/$name/pgdog.toml"
  echo "$manifests" | yq -r 'select(.kind == "Secret" and .metadata.name == "t-pgdog") | .data["users.toml"] // ""' | base64 -d > "$work/$name/users.toml"
done
chmod -R a+rX "$work"

# Checks every directory under $1 with the pgdog binary $2.
# shellcheck disable=SC2016 # expands where it runs
check='
  failed=0
  for dir in "$1"/*; do
    if out=$("$2" --config "$dir/pgdog.toml" --users "$dir/users.toml" configcheck 2>&1); then
      echo "  OK   $(basename "$dir")"
    else
      echo "  FAIL $(basename "$dir")"; echo "$out" | grep -i error | head -3; failed=1
    fi
  done
  exit $failed'

if [ -n "$PGDOG_BIN" ]; then
  echo "==> pgdog configcheck with $PGDOG_BIN"
  sh -c "$check" check "$work" "$(realpath "$PGDOG_BIN")"
else
  engine="${ENGINE:-$(command -v podman >/dev/null && echo podman || echo docker)}"
  image="${1:-$(helm template t "$CHART_DIR" | yq -r 'select(.kind == "Deployment") | .spec.template.spec.containers[0].image')}"
  echo "==> pgdog configcheck with $image"
  "$engine" run --rm -v "$work":/c:ro "$image" sh -c "$check" check /c pgdog
fi
