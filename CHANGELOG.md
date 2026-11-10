# Changelog

This fork has its own version numbers. It forked from upstream
[pgdogdev/pgdog](https://github.com/pgdogdev/pgdog) at 0.1.38.

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
