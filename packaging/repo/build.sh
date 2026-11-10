#!/bin/sh
# Builds the signed APT, YUM and APK repositories from every package in
# <packages-dir> (all versions, all architectures) into <out-dir>, a static
# tree to serve over HTTPS. See packaging/repo/README.md.
#   packaging/repo/build.sh <packages-dir> <out-dir>
# Environment:
#   REPO_URL           where <out-dir> will be served, e.g.
#                      https://bigleka.github.io/pgdog (written to pgdog.repo)
#   REPO_GPG_KEY_FILE  armored GPG private key: signs the APT and YUM indexes
#   REPO_APK_KEY_FILE  RSA private key (PEM): signs the APK index
#   ENGINE             docker or podman (default: podman if present)
set -eu

: "${REPO_URL:?set REPO_URL}"
: "${REPO_GPG_KEY_FILE:?set REPO_GPG_KEY_FILE}"
: "${REPO_APK_KEY_FILE:?set REPO_APK_KEY_FILE}"

pkgs="$(realpath "$1")"
mkdir -p "$2"
out="$(realpath "$2")"
here="$(cd "$(dirname "$0")" && pwd)"
engine="${ENGINE:-$(command -v podman >/dev/null && echo podman || echo docker)}"

# Keys go in a private directory the containers can read.
keys="$(mktemp -d)"
trap 'rm -rf "$keys"' EXIT
cp "$REPO_GPG_KEY_FILE" "$keys/gpg.asc"
cp "$REPO_APK_KEY_FILE" "$keys/pgdog-bigleka.rsa"
chmod 0644 "$keys"/*

find "$out" -mindepth 1 -delete

# The containers run as root; hand the files back to the caller. Under
# rootless podman, root in the container already is the caller.
case "$engine" in
    *podman*) owner=0:0 ;;
    *) owner="$(id -u):$(id -g)" ;;
esac

run() {
    image="$1" script="$2"
    "$engine" run --rm \
        -v "$pkgs":/pkgs:ro -v "$out":/out -v "$keys":/keys:ro -v "$here":/repo:ro \
        -e REPO_URL="$REPO_URL" -e OWNER="$owner" \
        "$image" sh "/repo/$script"
}

run docker.io/library/debian:13 index-deb-rpm.sh
run docker.io/library/alpine:3.22 index-apk.sh
find "$out" -type f | sed "s|^$out/||" | sort
