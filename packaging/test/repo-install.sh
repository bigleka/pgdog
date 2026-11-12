#!/bin/sh
# Installs pgdog from the package repository mounted at /repo, the way
# packaging/repo/README.md tells users to, with signature checks on.
# Runs inside a distribution container; see repo.sh.
#   repo-install.sh <version> <repo-url>
# <repo-url> is the URL the repository was built for; it is mounted here.
set -eu

version="$1"
published="$2"
url=file:///repo
local_repo() { sed "s|$published|$url|g" /repo/pgdog.repo; }

fail() { echo "FAIL: $*" >&2; exit 1; }

# Each family first tries with a key that did not sign the repository:
# that must fail, or the check that follows proves nothing.
if command -v apt-get >/dev/null; then
    export DEBIAN_FRONTEND=noninteractive
    list=/etc/apt/sources.list.d/pgdog.list
    set -- /usr/share/keyrings/*-archive-keyring.gpg; other="$1"
    echo "deb [signed-by=$other] $url/deb stable main" > "$list"
    if apt-get update -qq -o APT::Update::Error-Mode=any >/dev/null 2>&1; then
        fail "apt accepted the repository signed by another key"
    fi

    install -d -m 0755 /etc/apt/keyrings
    cp /repo/pgdog.asc /etc/apt/keyrings/pgdog.asc
    echo "deb [signed-by=/etc/apt/keyrings/pgdog.asc] $url/deb stable main" > "$list"
    apt-get update -qq -o APT::Update::Error-Mode=any >/dev/null \
        || fail "apt update with the repository key"
    apt-get install -y -qq --no-install-recommends pgdog >/dev/null \
        || fail "apt install"
    installed=$(dpkg-query -W -f '${Version}' pgdog)
elif command -v dnf >/dev/null; then
    set -- /etc/pki/rpm-gpg/RPM-GPG-KEY-*; other="$1"
    local_repo | sed "s|^gpgkey=.*|gpgkey=file://$other|" > /etc/yum.repos.d/pgdog.repo
    if dnf install -y -q pgdog >/dev/null 2>&1; then
        fail "dnf installed a package signed by another key"
    fi
    dnf clean all -q >/dev/null

    local_repo > /etc/yum.repos.d/pgdog.repo
    dnf install -y -q pgdog >/dev/null || fail "dnf install"
    installed=$(rpm -q --qf '%{VERSION}-%{RELEASE}' pgdog)
elif command -v apk >/dev/null; then
    echo "/repo/apk" >> /etc/apk/repositories
    if apk add -q pgdog >/dev/null 2>&1; then
        fail "apk installed from an index it has no key for"
    fi

    cp /repo/pgdog-bigleka.rsa.pub /etc/apk/keys/
    apk add -q pgdog >/dev/null || fail "apk add"
    installed=$(apk info -e -v pgdog 2>/dev/null | sed 's/^pgdog-//')
else
    fail "no known package manager"
fi

case "$installed" in
    "$version"-*) ;;
    *) fail "installed $installed, expected $version" ;;
esac
pgdog --version | grep -q "PgDog v$version " || fail "pgdog --version"
echo "OK $installed"
