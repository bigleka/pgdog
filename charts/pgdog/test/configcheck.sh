#!/bin/bash
# Checks that PgDog accepts the configuration the chart renders for every
# test/values-*.yaml, with `pgdog configcheck` in the chart's image. The
# chart comes from upstream and can render settings this fork lacks: those
# must fail in the chart (templates/_fork.tpl), never in PgDog.
#   test/configcheck.sh [image]
# Needs helm and yq, and docker or podman (ENGINE). Default image: the one
# the chart deploys.
set -eo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHART_DIR="$TEST_DIR/.."
engine="${ENGINE:-$(command -v podman >/dev/null && echo podman || echo docker)}"
image="${1:-$(helm template t "$CHART_DIR" | yq -r 'select(.kind == "Deployment") | .spec.template.spec.containers[0].image')}"

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

echo "==> pgdog configcheck with $image"
# shellcheck disable=SC2016 # expands in the container
"$engine" run --rm -v "$work":/c:ro "$image" sh -c '
  failed=0
  for dir in /c/*; do
    if out=$(pgdog --config "$dir/pgdog.toml" --users "$dir/users.toml" configcheck 2>&1); then
      echo "  OK   $(basename "$dir")"
    else
      echo "  FAIL $(basename "$dir")"; echo "$out" | grep -i error | head -3; failed=1
    fi
  done
  exit $failed'
