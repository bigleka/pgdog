#!/bin/sh
set -e

# $1: "remove"/"upgrade" (deb), remaining package count (rpm: 0 on erase).
# apk only runs this on removal.
case "${1:-remove}" in
    remove|purge|0)
        if [ -d /run/systemd/system ]; then
            systemctl disable --now pgdog >/dev/null 2>&1 || true
        elif command -v rc-service >/dev/null; then
            rc-service pgdog stop >/dev/null 2>&1 || true
            rc-update del pgdog >/dev/null 2>&1 || true
        fi
        ;;
esac
