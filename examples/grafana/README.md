# Prometheus and Grafana

PgDog serves its metrics in the OpenMetrics format. Enable the endpoint in
`pgdog.toml`:

```toml
[general]
openmetrics_port = 9090
```

This directory has:

| File | What it is |
|---|---|
| [`dashboard.json`](dashboard.json) | Grafana dashboard: clients, pools, queries, errors, and the result cache. |
| [`prometheus/prometheus.yml`](prometheus/prometheus.yml) | Prometheus scrape config. |
| [`prometheus/alerts.yml`](prometheus/alerts.yml) | Alerting rules. |
| [`kubernetes/podmonitor.yaml`](kubernetes/podmonitor.yaml) | Prometheus Operator: scrape every PgDog pod. |
| [`kubernetes/prometheusrule.yaml`](kubernetes/prometheusrule.yaml) | Prometheus Operator: the same alerting rules. |
| [`docker-compose.yml`](docker-compose.yml) | Prometheus and Grafana to try it locally. |

## Try it locally

With PgDog running on this machine with `openmetrics_port = 9090`:

```bash
docker compose up
```

Open Grafana at http://localhost:3000 (`admin` / `admin`): the **PgDog** dashboard is already there. Prometheus is at http://localhost:9091.

## Import the dashboard

In Grafana, **Dashboards → New → Import**, upload `dashboard.json` and pick your Prometheus data source. Its variables:

- **PgDog**: one or more PgDog instances (the `instance` label).
- **Database**: one or more databases, for the pool panels.

The result cache panels show the whole PgDog (the cache metrics have no database label). Sum them across PgDogs sharing one Redis to see the cache as a whole.

If you set `openmetrics_namespace`, metric names get that prefix: replace them in the dashboard and the alerts (e.g. `clients` → `pgdog_clients`).

## Kubernetes

With the [Prometheus Operator](https://prometheus-operator.dev/):

1. Name the PgDog container's metrics port `metrics`:
   ```yaml
   ports:
     - name: metrics
       containerPort: 9090
   ```
2. Check the `selector` of `kubernetes/podmonitor.yaml` matches your PgDog pods' labels, and the `release` label matches what your Prometheus selects.
3. Apply it, and the rules:
   ```bash
   kubectl apply -f kubernetes/podmonitor.yaml -f kubernetes/prometheusrule.yaml
   ```
4. Import `dashboard.json` into Grafana. Each pod shows up as an instance.

## Alerts

| Alert | When | What to do |
|---|---|---|
| `PgDogDown` | Prometheus can't scrape a PgDog for 1 minute. | Check the PgDog process or pod. |
| `PgDogClientsWaiting` | Clients wait for a connection for 2 minutes. | Pools too small for the load (`default_pool_size`), or queries holding connections too long. |
| `PgDogLongWait` | A client waited over 1 second. | Same; it's already slowing the applications down. |
| `PgDogServerErrors` | Postgres returns errors through PgDog for 5 minutes. | Check the Postgres logs. |
| `PgDogResultCacheRedisDown` | The result cache is enabled but skipped for 2 minutes: Redis is unreachable or too slow. | Queries still work, from Postgres. Check Redis. Once it's back, entries of tables written meanwhile may be stale until they expire. |
| `PgDogResultCacheRedisErrors` | Over 1 Redis error per second for 5 minutes. | If Redis is healthy, `redis_response_timeout_ms` may be below its latency: see [Redis timeouts](../../docs/RESULT_CACHE.md#redis-timeouts). |

Thresholds are starting points: tune them to your workload.

## Keeping it in sync

`pgdog/src/stats/grafana.rs` tests that the dashboard and alerts only use metrics PgDog exports, that the dashboard shows every result cache metric, and that `kubernetes/prometheusrule.yaml` has the same alerts as `prometheus/alerts.yml`. Keep each alert `expr` on one line.
