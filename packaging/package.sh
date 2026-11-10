#!/bin/sh
# Builds the .deb, .rpm and .apk for one architecture with nfpm.
#   packaging/package.sh <arch> <glibc-binary> <musl-binary> <out-dir>
# <arch> is amd64 or arm64. Needs `nfpm` on PATH; run from anywhere.
set -eu

ARCH="$1"
glibc="$(realpath "$2")"
musl="$(realpath "$3")"
out="$4"
mkdir -p "$out"
out="$(realpath "$out")"

cd "$(dirname "$0")"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' ../pgdog/Cargo.toml | head -n1)"
export VERSION ARCH

package() {
    install -D -m 0755 "$1" .build/pgdog
    shift
    for packager in "$@"; do
        "${NFPM:-nfpm}" package --config nfpm.yaml --packager "$packager" --target "$out"
    done
}

package "$glibc" deb rpm
package "$musl" apk
rm -rf .build
ls -l "$out"
