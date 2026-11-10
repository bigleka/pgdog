#!/bin/sh
# Installs pgdog from the repository in <dir> on every supported
# distribution, with signature checks on.
#   REPO_URL=<url> packaging/test/repo.sh <dir> [arch]
# REPO_URL is the URL <dir> was built for (packaging/repo/build.sh); the
# test installs from <dir> instead.
# ENGINE selects docker or podman (default: podman if present).
set -eu
: "${REPO_URL:?set REPO_URL}"

dir="$(realpath "$1")"
arch="${2:-amd64}"
here="$(cd "$(dirname "$0")" && pwd)"
engine="${ENGINE:-$(command -v podman >/dev/null && echo podman || echo docker)}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../../pgdog/Cargo.toml" | head -n1)"
platform="linux/$arch"

# IMAGES overrides the list, e.g. to leave out almalinux:10, whose
# x86-64-v3 build doesn't start on older CPUs.
images="${IMAGES:-docker.io/library/debian:12 docker.io/library/debian:13
    docker.io/library/ubuntu:22.04 docker.io/library/ubuntu:24.04
    docker.io/rockylinux/rockylinux:9 docker.io/library/almalinux:10
    docker.io/library/alpine:3.22}"

for image in $images; do
    echo "### $image"
    "$engine" run --rm --platform "$platform" -v "$dir":/repo:ro -v "$here":/test:ro "$image" \
        sh /test/repo-install.sh "$version" "$REPO_URL"
done
echo "repository OK"
