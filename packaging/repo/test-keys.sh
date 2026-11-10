#!/bin/sh
# Generates throwaway signing keys, for building and testing signed
# packages and repositories without the real ones (pull requests, local
# runs). Needs gpg and openssl.
#   packaging/repo/test-keys.sh <dir>
# Writes <dir>/gpg.asc (GPG private key) and <dir>/apk.rsa (RSA private key).
set -eu

mkdir -p "$1"
dir="$(realpath "$1")"
GNUPGHOME="$(mktemp -d)"
export GNUPGHOME
trap 'rm -rf "$GNUPGHOME"' EXIT

gpg --batch --quiet --passphrase '' \
    --quick-gen-key "PgDog test key <test@example.invalid>" rsa3072 sign 1d
gpg --batch --armor --export-secret-keys > "$dir/gpg.asc"
openssl genrsa -out "$dir/apk.rsa" 2048 2>/dev/null
chmod 0600 "$dir/gpg.asc" "$dir/apk.rsa"
