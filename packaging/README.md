# Packages

`.deb`, `.rpm` and `.apk` for amd64 and arm64, attached to every
[GitHub Release](https://github.com/bigleka/pgdog/releases).

## Supported distributions

| Package | Distributions | Init |
|---------|---------------|------|
| `.deb` | Debian 12+, Ubuntu 22.04+ | systemd |
| `.rpm` | RHEL, Rocky, AlmaLinux 9+ | systemd |
| `.apk` | Alpine 3.22+ | OpenRC |

The deb and rpm share one binary, built on Rocky Linux 9: it needs glibc 2.34
and OpenSSL 3, which older releases (Debian 11, Ubuntu 20.04, RHEL 8) don't
have. The apk is built against Alpine's musl. Plugins must be built for the
same libc as the package.

## Install

From the package repository, signed and with upgrades through the package
manager. `https://bigleka.github.io/pgdog` serves every released version.

Debian / Ubuntu:

```bash
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL https://bigleka.github.io/pgdog/pgdog.asc -o /etc/apt/keyrings/pgdog.asc
echo "deb [signed-by=/etc/apt/keyrings/pgdog.asc] https://bigleka.github.io/pgdog/deb stable main" \
    | sudo tee /etc/apt/sources.list.d/pgdog.list
sudo apt update && sudo apt install pgdog
```

RHEL / Rocky / Alma:

```bash
sudo curl -fsSL https://bigleka.github.io/pgdog/pgdog.repo -o /etc/yum.repos.d/pgdog.repo
sudo dnf install pgdog
```

Alpine:

```bash
wget -qO /etc/apk/keys/pgdog-bigleka.rsa.pub https://bigleka.github.io/pgdog/pgdog-bigleka.rsa.pub
echo https://bigleka.github.io/pgdog/apk >> /etc/apk/repositories
apk add pgdog
```

`gpg --show-keys pgdog.asc` prints the key's fingerprint, to compare with
the one in [repo/README.md](repo/README.md#keys).

Or download the package from the release and install the file:

```bash
sudo apt install ./pgdog_1.0.0-1_amd64.deb
sudo dnf install ./pgdog-1.0.0-1.x86_64.rpm
sudo apk add ./pgdog_1.0.0-r1_x86_64.apk    # with the key in /etc/apk/keys
```

Check the download against `SHA256SUMS` from the same release:
`sha256sum -c SHA256SUMS --ignore-missing`.

## What gets installed

| Path | |
|------|-|
| `/usr/bin/pgdog` | The binary. |
| `/etc/pgdog/pgdog.toml` | Configuration. Listens on `127.0.0.1:6432`, no databases. |
| `/etc/pgdog/users.toml` | Users and passwords: mode `0640`, group `pgdog`. |
| `/usr/lib/systemd/system/pgdog.service` | The service (deb, rpm). |
| `/usr/lib/systemd/system/pgdog@.service` | One service per `/etc/pgdog/<name>/` (deb, rpm). |
| `/etc/init.d/pgdog`, `/etc/conf.d/pgdog` | The service (apk). Logs in `/var/log/pgdog/`. |
| `/usr/share/doc/pgdog/` | README, result cache guide, example configs, Grafana dashboard, Prometheus alerts. |

The package creates the `pgdog` system user; the service runs as it. Edited
configuration files are kept on upgrade and reinstall.

## Run

The service isn't started on install: add a database to `pgdog.toml` and a
user to `users.toml` first.

```bash
sudo systemctl enable --now pgdog
sudo systemctl reload pgdog        # SIGHUP: reloads both files, keeps clients
journalctl -u pgdog -f
```

On Alpine: `rc-update add pgdog && rc-service pgdog start`,
`rc-service pgdog reload`.

Stopping sends SIGINT: PgDog stops accepting clients and waits up to
`general.shutdown_timeout` (60 s by default) for running transactions.

Several PgDogs on one host: put each configuration in `/etc/pgdog/<name>/`
(`pgdog.toml` and `users.toml`, different ports) and
`systemctl enable --now pgdog@<name>`. Environment overrides (`PGDOG_*`,
`RUST_LOG`) go in `/etc/default/pgdog` or `/etc/sysconfig/pgdog`
(`pgdog-<name>` for an instance).

## Versions

PgDog in this fork has its own version numbers, starting at 1.0.0. It forked
from upstream [pgdogdev/pgdog](https://github.com/pgdogdev/pgdog) at 0.1.38.
`pgdog --version` and `SHOW VERSION` print the package version.

## Building

Everything runs in containers (podman or docker):

```bash
# Binaries: glibc (Rocky 9) and musl (Alpine)
podman build -t pgdog-builder-glibc -f packaging/builder/glibc.Containerfile packaging/builder
podman run --rm -v "$PWD":/src -w /src pgdog-builder-glibc \
    sh -c 'git config --global --add safe.directory /src && packaging/scripts/compile.sh dist/glibc'
# ...same with musl.Containerfile into dist/musl

# Packages, with nfpm (https://nfpm.goreleaser.com)
packaging/package.sh amd64 dist/glibc/pgdog dist/musl/pgdog packages

# Install test on Debian, Ubuntu, Rocky, Alma and Alpine
packaging/test/run.sh packages amd64
```

Signing and the package repository: [repo/README.md](repo/README.md).

## Releasing

1. Bump `version` in `pgdog/Cargo.toml` and merge it.
2. Tag the merge commit: `git tag v1.0.1 && git push origin v1.0.1`.

The `release` workflow builds, signs and tests the packages, attaches them
and `SHA256SUMS` to the release, and publishes the repository with every
released version. It fails if the tag doesn't match `pgdog/Cargo.toml` or the
signing keys aren't set up ([repo/README.md](repo/README.md)).
