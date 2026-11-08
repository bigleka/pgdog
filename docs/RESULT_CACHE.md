# PgDog Result Cache (Redis)

PgDog supports an **experimental** Redis/RESP-backed **query result cache** inspired by pgpool-II's memqcache, but designed for Redis-compatible servers such as **Redis**, **Valkey**, and **Dragonfly**.

## What it does (MVP)

- Caches **SELECT results** and returns the cached Postgres-protocol response on cache hit.
- Stores a binary payload containing the response messages (e.g. `RowDescription` + `DataRow*` + `CommandComplete` + `ReadyForQuery`).

## Conservative invalidation (MVP)

To avoid stale results while still being precise, PgDog uses **table-based invalidation**:

- On `DML/DDL`, PgDog extracts touched tables from the parsed AST.
- Each cached entry is tagged into Redis sets keyed by table name.
- When a table changes, PgDog deletes only the keys referenced by that table tag set.
- Invalidation happens when the write's transaction ends (right after an autocommit
  statement, or at `COMMIT`/`ROLLBACK` of an explicit transaction), before the client
  receives `ReadyForQuery`.
- Each table also has a generation counter (`<prefix>:gen:<db>:<schema>.<table>`)
  that invalidation increments. Cache keys include the current generation of every
  table the query reads, so a slow read that started before a write and finishes after
  it stores its result under a key no one looks up again. These counters never expire.
- Enabling the result cache turns on the query parser when `query_parser = "auto"`,
  since both caching and invalidation need the parsed query.
- Writes that don't go through PgDog are not seen; their results stay cached until
  `expire_seconds` passes.

## Stampede protection (singleflight)

When several clients of the same PgDog process miss on the same cache key at once (e.g. a hot entry just expired), only the first one runs the query. The others wait for its result and get it as a cache hit, so the database sees one query instead of one per client.

- The result is stored in Redis before waiting clients are released, so clients arriving afterwards hit the cache.
- If the first client gets no result to share (the query fails, the result can't be cached, or it disconnects), waiting clients run the query themselves.
- Waiting has no timeout of its own: it lasts as long as the first client's query.
- Coalescing is per PgDog process. Separate PgDog instances each run the query once.

All clients of a process share one result cache and one Redis connection.

## Early refresh (XFetch)

Singleflight stops a stampede once an entry has expired. XFetch avoids most of them in the first place: a hit close to expiry may recompute the entry ahead of time, while it's still being served.

- Each entry stores how long its query took to compute (`delta`). A hit refreshes it when `-delta * xfetch_beta * ln(rand)` reaches the entry's remaining TTL, so slow queries and entries near expiry refresh earlier (Vattani, Chierichetti, Lowenstein, "Optimal Probabilistic Cache Stampede Prevention", VLDB 2015).
- The refreshing client runs the query and gets the fresh result; it pays the query latency.
- Only one client per PgDog process refreshes a given entry at a time. Hits on it during the refresh are served the current entry immediately.
- `xfetch_beta` tunes it: above `1.0` refreshes earlier, below later, `0` disables it. Default `1.0`.

## Metrics

Exposed in OpenMetrics format on the PgDog metrics endpoint:

| Metric | Meaning |
|---|---|
| `result_cache_hits` | Cached results served |
| `result_cache_misses` | Cacheable queries not found in the cache |
| `result_cache_coalesced` | Misses answered with another client's in-flight result (singleflight) |
| `result_cache_early_refreshes` | Entries recomputed before expiry (XFetch) |
| `result_cache_stores` | Entries written |
| `result_cache_bytes_served` / `result_cache_bytes_stored` | Bytes served from / written to the cache |
| `result_cache_redis_errors` | Failed Redis calls |

## Configuration

Add this section to `pgdog.toml`:

```toml
[result_cache]
enabled = true
redis_url = "redis://127.0.0.1:6379"
expire_seconds = 30
max_entry_bytes = 524288
key_prefix = "pgdog:result_cache"
xfetch_beta = 1.0

# Optional allow/deny lists (regex). Unsafe lists take precedence.
cache_safe_schema_list = []
cache_unsafe_schema_list = []
cache_safe_table_list = []
cache_unsafe_table_list = []
```

## Current limitations

- Only caches **simple query protocol** (`Query` message) requests.
- Skips caching inside explicit transactions.
- Session signature is conservative and may lead to fewer cache hits (by design).

