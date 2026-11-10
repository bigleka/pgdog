#!/bin/sh
# Create the pgdog system user and group.
set -e

if ! getent group pgdog >/dev/null; then
    if command -v addgroup >/dev/null && ! command -v groupadd >/dev/null; then
        addgroup -S pgdog
    else
        groupadd --system pgdog
    fi
fi

if ! getent passwd pgdog >/dev/null; then
    if command -v adduser >/dev/null && ! command -v useradd >/dev/null; then
        adduser -S -D -H -h /nonexistent -s /sbin/nologin -G pgdog -g PgDog pgdog
    else
        useradd --system --gid pgdog --no-create-home --home-dir /nonexistent \
            --shell /usr/sbin/nologin --comment PgDog pgdog
    fi
fi
