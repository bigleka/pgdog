#!/bin/sh
# APK index. Runs in an Alpine container started by build.sh: /pkgs
# packages, /out the repository, /keys/pgdog-bigleka.rsa the signing key.
# apk looks for <repository>/<arch>/APKINDEX.tar.gz.
set -eu

apk add -q abuild openssl >/dev/null

key=/keys/pgdog-bigleka.rsa
openssl rsa -in "$key" -pubout -out /out/pgdog-bigleka.rsa.pub 2>/dev/null

for arch in x86_64 aarch64; do
    dir="/out/apk/$arch"
    mkdir -p "$dir"
    # apk fetches <name>-<version>.apk; nfpm writes <name>_<version>_<arch>.apk.
    for pkg in /pkgs/*_"$arch".apk; do
        [ -e "$pkg" ] || continue
        cp "$pkg" "$dir/$(basename "$pkg" "_$arch.apk" | sed 's/_/-/')".apk
    done
    cd "$dir"
    set -- *.apk
    if [ -e "$1" ]; then
        # The packages' own signatures are checked on install; this only
        # lists them.
        apk index --quiet --allow-untrusted --output APKINDEX.tar.gz "$@"
    else
        apk index --quiet --output APKINDEX.tar.gz
    fi
    # abuild-sign names the signature after the public key it expects
    # users to hold: /etc/apk/keys/pgdog-bigleka.rsa.pub.
    abuild-sign -q -k "$key" -p pgdog-bigleka.rsa.pub APKINDEX.tar.gz
done

chown -R "$OWNER" /out
