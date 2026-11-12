#!/bin/sh
# Runs install.sh for every package in <dir> on the distributions it targets.
#   packaging/test/run.sh <dir> [arch]
# ENGINE selects docker or podman (default: podman if present).
set -eu

dir="$(realpath "$1")"
arch="${2:-amd64}"
here="$(cd "$(dirname "$0")" && pwd)"
engine="${ENGINE:-$(command -v podman >/dev/null && echo podman || echo docker)}"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../../pgdog/Cargo.toml" | head -n1)"

case "$arch" in
    amd64) deb="$arch" rpm=x86_64 apk=x86_64 ;;
    arm64) deb="$arch" rpm=aarch64 apk=aarch64 ;;
    *) echo "unknown arch $arch" >&2; exit 1 ;;
esac

run() {
    image="$1" pkg="$2"
    [ -f "$dir/$pkg" ] || { echo "missing $dir/$pkg" >&2; exit 1; }
    echo "### $image: $pkg"
    "$engine" run --rm -v "$dir":/pkgs:ro -v "$here":/test:ro "$image" \
        sh /test/install.sh "/pkgs/$pkg" "$version"
}

run docker.io/library/debian:12 "pgdog_${version}-1_${deb}.deb"
run docker.io/library/debian:13 "pgdog_${version}-1_${deb}.deb"
run docker.io/library/ubuntu:22.04 "pgdog_${version}-1_${deb}.deb"
run docker.io/library/ubuntu:24.04 "pgdog_${version}-1_${deb}.deb"
run docker.io/rockylinux/rockylinux:9 "pgdog-${version}-1.${rpm}.rpm"
run docker.io/library/almalinux:10 "pgdog-${version}-1.${rpm}.rpm"
run docker.io/library/alpine:3.22 "pgdog_${version}-r1_${apk}.apk"
echo "all packages OK"
