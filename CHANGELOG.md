# Changelog

This fork has its own version numbers. It forked from upstream
[pgdogdev/pgdog](https://github.com/pgdogdev/pgdog) at 0.1.38.

## 1.1.0

Helm chart and release images, result cache in read-only transactions,
schema-aware invalidation, and a pgbench benchmark.

### Helm chart

- `charts/pgdog`: upstream's chart (pgdogdev/helm v0.97) with this fork's
  image, `resultCache` settings, the encryption key from a Secret and an
  optional Valkey. Settings this fork's PgDog doesn't support make the
  install fail by name. Published as `oci://ghcr.io/bigleka/charts/pgdog`
  on releases. See [charts/pgdog/README.md](charts/pgdog/README.md).

### Docs

- The result cache guide says which Redis eviction policies are safe.
- Result cache benchmark with pgbench (`bench/result_cache/run.sh`), with
  results in the guide.

### Container image

- `ghcr.io/bigleka/pgdog` is tagged `X.Y.Z`, `X.Y` and `latest` on
  releases, besides `main` and the commit SHA. Release images were never
  built: the release event is created with `GITHUB_TOKEN`, which doesn't
  trigger other workflows.

### Result cache (Redis)

- The encryption key can come from a file (`encryption_key_file`) or the
  `PGDOG_RESULT_CACHE_ENCRYPTION_KEY` environment variable instead of
  `pgdog.toml`. An empty key, or both `encryption_key` and
  `encryption_key_file`, is now a configuration error.
- A write to a table named without a schema (`UPDATE pedidos`) now drops
  cached reads of it under its schema (`SELECT ... FROM public.pedidos`),
  and the other way around. Each was invalidated only under the name it was
  written with, so the other could be served stale until it expired. The
  generation counters change; while PgDogs of both versions share a Redis,
  writes through one may not invalidate the other's entries.
- Read-after-write: a session that wrote `pedidos` sends its reads of
  `public.pedidos` to the primary too, and the other way around.
- Plain `SELECT`s in read-only `READ COMMITTED` transactions use the cache
  (`BEGIN READ ONLY ISOLATION LEVEL READ COMMITTED`). `BEGIN READ ONLY`
  without a level does with `cache_read_only_transactions = true`.
  `REPEATABLE READ` and `SERIALIZABLE` transactions never do.
- A simple query with several statements is cached only if all of them are
  plain `SELECT`s, and invalidates the tables of all of them. It went by its
  first statement: after `SELECT 1; UPDATE pedidos ...`, reads of `pedidos`
  could be served stale, and the same query run again could be served from
  the cache without running its write at all.

## 1.0.0

First release of the fork, with Linux packages.

### Result cache (Redis)

- Redis standalone, Cluster, Sentinel and twemproxy.
- Invalidation on writes, including inside transactions, through
  per-table generations shared by every PgDog using the same Redis.
- Single-flight: one query per miss in a PgDog, and across PgDogs with a
  Redis lock (`singleflight_lock_ms`).
- XFetch early refresh and adaptive TTL.
- Read-after-write: reads of a table written in the last
  `read_after_write_window_ms` go to the primary.
- Extended protocol (prepared statements) caching.
- Configurable Redis timeouts, retries and back-off.
- Metrics, Grafana dashboard, Prometheus alerts, Kubernetes PodMonitor and
  PrometheusRule (`examples/grafana`).

See [docs/RESULT_CACHE.md](docs/RESULT_CACHE.md).

### Packaging

- `.deb`, `.rpm` and `.apk` for amd64 and arm64, with systemd (or OpenRC)
  services, a `pgdog` system user and configuration in `/etc/pgdog`.
  See [packaging/README.md](packaging/README.md).
- `pgdog --version` and `SHOW VERSION` show the package version.
- Signed APT, YUM and APK repository with every released version, on
  GitHub Pages; `.rpm` and `.apk` packages are signed too. See
  [packaging/repo/README.md](packaging/repo/README.md).
