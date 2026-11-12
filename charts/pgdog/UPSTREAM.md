# Upstream

This chart is [pgdogdev/helm](https://github.com/pgdogdev/helm) at
commit `2fa5e87` (chart v0.97, for PgDog 0.1.63), MIT licensed
([LICENSE](LICENSE)). Changes for the fork:

- `Chart.yaml`: name kept, version follows the fork's releases.
- `values.yaml`: `image.repository` is `ghcr.io/bigleka/pgdog`;
  `openMetricsNamespace` is empty (upstream: `pgdog_`), so metric names
  match the fork's dashboard and alerts; new `resultCache` section.
- `templates/_fork.tpl` (new) and its call at the top of
  `templates/deployment.yaml`: refuse settings the fork's PgDog lacks.
- `templates/config.yaml`: `[result_cache]`.
- `templates/deployment.yaml`: `PGDOG_RESULT_CACHE_ENCRYPTION_KEY` from
  `resultCache.encryptionKeySecret`.
- `templates/valkey.yaml` (new): the optional Valkey.
- `test/`: upstream's tests for unsupported settings moved, unchanged, to
  `test/unsupported/` (and their checks out of `test.sh`); new
  `values-result-cache*.yaml`, `configcheck.sh` and `e2e/`.
- `README.md`: a section about the fork on top.
- Removed `dev/` (upstream's deploy script) and `.github/` (its CI and
  publishing, replaced by `.github/workflows/helm.yml` at the root).

To bring in upstream changes, diff `templates/` and `values.yaml` against
the new upstream commit and update the commit above. When the fork ports a
feature, drop it from `_fork.tpl` and move its test back from
`test/unsupported/`.
