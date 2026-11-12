#!/bin/sh
set -e

# users.toml holds passwords: readable by the service, not by everyone.
chown root:pgdog /etc/pgdog /etc/pgdog/users.toml
chmod 0750 /etc/pgdog
chmod 0640 /etc/pgdog/users.toml

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || true
    # Upgrade: pick up the new binary. Fresh installs aren't started: the
    # default config has no databases.
    if systemctl is-active --quiet pgdog; then
        systemctl try-restart pgdog >/dev/null 2>&1 || true
    fi
fi
