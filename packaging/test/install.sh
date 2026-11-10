#!/bin/sh
# Installs a package in a clean distro container and checks it works:
# files, user, permissions, the service starting as pgdog, a client
# connecting, reload with SIGHUP, upgrade keeping edited config, removal.
#   install.sh <package> <version>
# Runs as root inside the container; see run.sh.
set -eu

pkg="$1"
version="$2"

step() { echo "==> $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

step "install $pkg"
case "$pkg" in
    *.deb)
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq "$pkg" postgresql-client systemd >/dev/null
        remove() { apt-get purge -y -qq pgdog >/dev/null; }
        reinstall() { apt-get install -y -qq --reinstall "$pkg" >/dev/null; }
        service=/usr/lib/systemd/system/pgdog.service
        ;;
    *.rpm)
        dnf install -y -q "$pkg" postgresql systemd >/dev/null
        remove() { dnf remove -y -q pgdog >/dev/null; }
        reinstall() { dnf reinstall -y -q "$pkg" >/dev/null; }
        service=/usr/lib/systemd/system/pgdog.service
        ;;
    *.apk)
        apk add -q --allow-untrusted "$pkg" postgresql-client openrc
        remove() { apk del -q pgdog; }
        reinstall() { apk fix -q --reinstall pgdog; }
        service=/etc/init.d/pgdog
        ;;
    *) fail "unknown package type: $pkg" ;;
esac

step "files"
pgdog --version | grep -F "PgDog v$version " || fail "--version isn't $version"
[ -f "$service" ] || fail "missing $service"
[ -f /etc/pgdog/pgdog.toml ] || fail "missing pgdog.toml"
# Minimized images (Ubuntu) drop /usr/share/doc on install.
if [ -e /etc/dpkg/dpkg.cfg.d/excludes ]; then
    dpkg-deb -c "$pkg" | grep -q '/usr/share/doc/pgdog/examples/pgdog.toml' \
        || fail "missing examples"
else
    [ -f /usr/share/doc/pgdog/examples/pgdog.toml ] || fail "missing examples"
fi

if [ "${pkg##*.}" != apk ]; then
    step "systemd units"
    # Only our units: the distro's own may not verify in a container.
    systemd-analyze verify "$service" pgdog@test.service 2>&1 \
        | grep -i pgdog | tee /tmp/verify.log
    [ ! -s /tmp/verify.log ] || fail "systemd-analyze verify"
fi

step "user and permissions"
id pgdog
[ "$(stat -c '%a %U %G' /etc/pgdog/users.toml)" = "640 root pgdog" ] \
    || fail "users.toml: $(stat -c '%a %U %G' /etc/pgdog/users.toml)"
[ "$(stat -c '%a %U %G' /etc/pgdog)" = "750 root pgdog" ] \
    || fail "/etc/pgdog: $(stat -c '%a %U %G' /etc/pgdog)"

admin() {
    PGPASSWORD="$1" psql -h 127.0.0.1 -p 6432 -U admin -d admin -tA \
        -c "SHOW VERSION" 2>&1
}

step "start as pgdog"
cat >> /etc/pgdog/users.toml <<'TOML'

[admin]
name = "admin"
user = "admin"
password = "first"
TOML
su -s /bin/sh -c 'pgdog --config /etc/pgdog/pgdog.toml --users /etc/pgdog/users.toml \
    > /tmp/pgdog.log 2>&1 & echo $! > /tmp/pgdog.pid' pgdog
pid="$(cat /tmp/pgdog.pid)"
i=0
until admin first | grep -q "v$version"; do
    i=$((i + 1))
    [ "$i" -lt 50 ] || { cat /tmp/pgdog.log; fail "pgdog didn't accept connections"; }
    sleep 0.2
done
[ "$(stat -c %U "/proc/$pid")" = pgdog ] || fail "not running as pgdog"
admin first

step "reload"
sed -i 's/password = "first"/password = "second"/' /etc/pgdog/users.toml
kill -HUP "$pid"
i=0
until admin second | grep -q "v$version"; do
    i=$((i + 1))
    [ "$i" -lt 50 ] || { cat /tmp/pgdog.log; fail "reload didn't apply"; }
    sleep 0.2
done
kill -0 "$pid" || fail "pgdog exited on SIGHUP"

step "stop with SIGINT"
kill -INT "$pid"
i=0
while kill -0 "$pid" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -lt 50 ] || { cat /tmp/pgdog.log; fail "pgdog didn't stop on SIGINT"; }
    sleep 0.2
done

if [ "${pkg##*.}" = apk ]; then
    step "OpenRC service"
    # No init in the container: run the script alone, without dependencies.
    mkdir -p /run/openrc && touch /run/openrc/softlevel
    sed -i 's/password = "second"/password = "third"/' /etc/pgdog/users.toml
    /etc/init.d/pgdog --nodeps start
    i=0
    until admin third | grep -q "v$version"; do
        i=$((i + 1))
        [ "$i" -lt 50 ] || { cat /var/log/pgdog/pgdog.log; fail "service didn't start"; }
        sleep 0.2
    done
    [ "$(stat -c %U "/proc/$(cat /run/pgdog.pid)")" = pgdog ] || fail "service not running as pgdog"
    sed -i 's/password = "third"/password = "second"/' /etc/pgdog/users.toml
    /etc/init.d/pgdog --nodeps reload
    i=0
    until admin second | grep -q "v$version"; do
        i=$((i + 1))
        [ "$i" -lt 50 ] || fail "service reload didn't apply"
        sleep 0.2
    done
    /etc/init.d/pgdog --nodeps stop
    grep -q "PgDog is shutting down" /var/log/pgdog/pgdog.log || fail "no graceful shutdown"
fi

step "reinstall keeps edited config"
echo "# edited" >> /etc/pgdog/pgdog.toml
reinstall
grep -q "# edited" /etc/pgdog/pgdog.toml || fail "pgdog.toml overwritten"
grep -q 'password = "second"' /etc/pgdog/users.toml || fail "users.toml overwritten"
[ "$(stat -c '%a %U %G' /etc/pgdog/users.toml)" = "640 root pgdog" ] \
    || fail "users.toml after reinstall: $(stat -c '%a %U %G' /etc/pgdog/users.toml)"

step "remove"
remove
[ ! -e /usr/bin/pgdog ] || fail "binary left behind"
[ ! -e "$service" ] || fail "service left behind"

echo "OK $pkg"
