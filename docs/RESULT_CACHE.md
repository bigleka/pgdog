# PgDog Result Cache (Redis)

PgDog supports an **experimental** Redis/RESP-backed **query result cache** inspired by pgpool-II's memqcache, but designed for Redis-compatible servers such as **Redis**, **Valkey**, and **Dragonfly**.

## What it does (MVP)

- Caches **SELECT results** and returns the cached Postgres-protocol response on cache hit.
- Over the simple protocol (`Query`) and the extended protocol (`Parse`/`Bind`/`Execute`, used by most drivers for parameterized queries). See [Extended protocol](#extended-protocol).
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

## Redis topologies

| Topology | Config |
|---|---|
| One Redis / Valkey / Dragonfly server | `redis_url = "redis://host:6379"` |
| Proxy in front of several servers (twemproxy, Envoy) | `redis_url = "redis://proxy:22121"` |
| Redis Cluster | `redis_mode = "cluster"`, `redis_urls = ["redis://node1:6379", ...]` (or a single node in `redis_url`; the rest is discovered) |
| Sentinel | `redis_mode = "sentinel"`, `redis_urls = ["redis://sentinel1:26379", ...]`, `redis_sentinel_master = "mymaster"`. Put the primary's credentials in `redis_url` (e.g. `redis://:password@`) if it needs them |

With Sentinel, after a failover the former primary drops its connections or refuses writes (`READONLY`). PgDog then skips the cache for the backoff below and reconnects through Sentinel to the new primary.

PgDog only sends commands that work on all of them: no `MULTI`, no `CLIENT` commands (twemproxy closes the connection on them), and multi-key commands only where the client splits them per cluster node.

## When Redis is down or slow

The cache never holds a query up for long:

- Connecting and every Redis command time out after 500ms (`redis_connect_timeout_ms`, `redis_response_timeout_ms`).
- After a failed connection, a timeout, or a network error, PgDog skips the cache for 5 seconds (`redis_unavailable_backoff_ms`): queries go straight to Postgres without waiting on Redis. It then reconnects on the next query.
- Writes made while Redis is unreachable can't invalidate anything. Once Redis is back, entries of the tables they changed may be served until they expire (`expire_seconds`, or `adaptive_ttl_max_seconds` with adaptive TTL).

### Redis timeouts

The defaults suit a Redis in the same datacenter as PgDog. **Keep them unless you've measured a reason not to.** The cache only speeds reads up; Postgres always has the answer. A timeout too long makes queries wait on a sick Redis; one too short marks a healthy Redis as down, and the cache keeps switching off.

| Option | Default | What it bounds |
|---|---|---|
| `redis_connect_timeout_ms` | `500` | Opening a connection (TCP, TLS, AUTH). |
| `redis_response_timeout_ms` | `500` | Each Redis command. The most a slow Redis adds to a query before PgDog gives up on it. |
| `redis_connect_retries` | `1` | Extra connection attempts before Redis counts as down. Each one can cost a query another `redis_connect_timeout_ms`. |
| `redis_unavailable_backoff_ms` | `5000` | How long the cache is skipped once Redis counts as down. |

Before changing them:

1. **Measure Redis latency from the PgDog host**, under normal load: `redis-cli -h <host> --latency-history`, plus `SLOWLOG GET` and `INFO commandstats` on Redis. Behind twemproxy, Envoy or TLS, measure through it.
2. **Response timeout:** keep it at 5 to 10 times the p99 you measured, never below it. Keep it well below your applications' query timeouts and Postgres `statement_timeout`: when Redis hangs, a query can wait this long before going to Postgres.
3. **Connect timeout:** raise it only for a remote Redis (another region, TLS handshakes), to a few round trips. Worst case for the query that reconnects: `redis_connect_timeout_ms × (redis_connect_retries + 1)`.
4. **Backoff:** shorter brings the cache back sooner after an outage, but while Redis stays down, each attempt to reconnect costs the queries running at that moment one connect timeout. Keep it at a few seconds or more in production. `0` (reconnect on every query) is only for testing.
5. **Sentinel:** the time to move to a new primary is set in Sentinel, not here: `down-after-milliseconds` (Redis default 30000) plus the failover itself. Until then PgDog skips the cache and serves from Postgres. Lower `down-after-milliseconds` in `sentinel.conf` if that's too long, keeping it above your network's normal hiccups.
6. **Check after each change**, one option at a time: `result_cache_redis_errors` and "Redis unavailable" warnings in the logs should stay at zero while Redis is healthy, and the hit rate (`result_cache_hits` vs `result_cache_misses`) shouldn't drop. Changing these settings reconnects to Redis on the next query.

## Extended protocol

A request is cached when it runs one statement and ends with `Sync`: `[Parse] Bind [Describe] Execute Sync`, the way drivers send a parameterized query. Named and unnamed statements both work.

- The key covers the query, the parameter values and types, the parameter and result formats (text or binary), and which messages were sent. A hit replies with exactly the messages a miss would have (`ParseComplete`, `BindComplete`, `ParameterDescription`, `RowDescription`), so the same SQL over the simple protocol, or with or without `Describe`, gets its own entry.
- Statement names don't count: clients naming the same statement differently share entries.
- A statement whose `Parse` was answered from the cache never reached Postgres. PgDog prepares it on the server when a later `Bind` needs it, as it does for any prepared statement.
- Not cached: `Execute` with a row limit (partial results), pipelines of several statements before `Sync`, and requests ending in `Flush` instead of `Sync`.
- Writes over the extended protocol invalidate like any other.

## Read-after-write (replicas)

With replicas, a read right after a write can land on a replica that hasn't replayed the write yet. Worse, without protection, the cache would then store that stale result under the table's new generation and serve it to every client until it expires.

For `read_after_write_window_ms` (default `1000`) after a write to a table, reads of that table go to the primary:

- **Every client and every PgDog sharing Redis.** The table's generation in Redis is the time of its last write (Redis clock, in ms), so building the cache key tells whether it was just written, with no extra round trip. Results stored meanwhile come from the primary.
- **The session that wrote, for any read.** Reads PgDog doesn't cache (tables in the unsafe lists, pipelines) still go to the primary for that session's own writes.
- Explicit transactions (other than `BEGIN READ ONLY`) already run on the primary.
- It applies with `read_write_split = "exclude_primary"` too: these reads go to the primary anyway.
- After the window, reads go back to the replicas. The cache keeps serving what the primary returned.

Choosing the window: set it above your replicas' usual replication lag, which PgDog reports per replica. If it's too short, a lagging replica can still answer, and its result gets cached. If it's too long, more reads land on the primary after every write. `0` turns it off. Without replicas it changes nothing.

Limits:

- Tables are matched by name as written in the query: `public.users` and `users` are different tables here, as they are for invalidation.
- PgDog hosts and Redis need synchronized clocks (NTP): the window is measured against the time Redis recorded the write.
- In session pooling mode, a client keeps the server it got first. Use transaction mode with replicas.
- While Redis is unreachable, only the session's own writes are tracked.

## Stampede protection (singleflight)

When several clients of the same PgDog process miss on the same cache key at once (e.g. a hot entry just expired), only the first one runs the query. The others wait for its result and get it as a cache hit, so the database sees one query instead of one per client.

- The result is stored in Redis before waiting clients are released, so clients arriving afterwards hit the cache.
- If the first client gets no result to share (the query fails, the result can't be cached, or it disconnects), waiting clients run the query themselves.
- Waiting has no timeout of its own: it lasts as long as the first client's query.

All clients of a process share one result cache and one Redis connection.

### Between PgDogs sharing Redis

Several PgDogs (e.g. replicas of a Kubernetes deployment) pointing at the same Redis coalesce misses with each other too:

- Before running the query, the PgDog that missed takes a lock on the key in Redis (`SET {key}:lock <token> NX PX`). The others find it held and wait, checking the cache every 20 ms, then serve the result stored there.
- The lock is renewed while the query runs, so long queries keep it. `singleflight_lock_ms` (default `1000`) is how long the lock lasts once its holder stops renewing it: if a PgDog dies mid-query, the others run the query themselves about that long after.
- If the query fails or its result can't be cached, the lock is released right away and the next PgDog runs it.
- The lock is released only by its holder (compare-and-delete, sent with `EVAL` so it works through twemproxy).
- An XFetch early refresh is skipped while another PgDog is already refreshing the entry: the hit serves the cached result.
- Each miss costs one more Redis round trip (two for the PgDog that runs the query). `singleflight_lock_ms = 0` turns this off, leaving only coalescing within each PgDog.
- Works on every supported topology.

## Early refresh (XFetch)

Singleflight stops a stampede once an entry has expired. XFetch avoids most of them in the first place: a hit close to expiry may recompute the entry ahead of time, while it's still being served.

- Each entry stores how long its query took to compute (`delta`, in microseconds). A hit refreshes it when `-delta * xfetch_beta * ln(rand)` reaches the entry's remaining TTL, so slow queries and entries near expiry refresh earlier (Vattani, Chierichetti, Lowenstein, "Optimal Probabilistic Cache Stampede Prevention", VLDB 2015).
- The refreshing client runs the query and gets the fresh result; it pays the query latency.
- Only one client at a time refreshes a given entry, across all PgDogs sharing Redis. Hits on it during the refresh are served the current entry immediately.
- `xfetch_beta` tunes it: above `1.0` refreshes earlier, below later, `0` disables it. Default `1.0`.

## Adaptive TTL

Off by default. With `adaptive_ttl_max_seconds` set, every hit pushes its entry's expiry to `expire_seconds` from now, so entries that keep being read stay cached and cold ones expire as usual. No entry lives longer than `adaptive_ttl_max_seconds` after its query ran.

- Writes through PgDog still invalidate entries immediately.
- Changes made **outside** PgDog (another application, a migration, `psql` straight to Postgres) can stay invisible for up to `adaptive_ttl_max_seconds` instead of `expire_seconds`. Pick the cap with that in mind.
- The TTL update is sent to Redis in the background; the client doesn't wait for it.
- Near the cap the remaining TTL shrinks again, so XFetch refreshes hot entries before they expire.

## Metrics

Exposed in OpenMetrics format on the PgDog metrics endpoint:

| Metric | Meaning |
|---|---|
| `result_cache_hits` | Cached results served |
| `result_cache_misses` | Cacheable queries not found in the cache |
| `result_cache_coalesced` | Misses answered with another client's in-flight result (singleflight) |
| `result_cache_remote_waits` | Misses that waited for another PgDog running the same query |
| `result_cache_remote_coalesced` | Misses served from the result another PgDog computed |
| `result_cache_early_refreshes` | Entries recomputed before expiry (XFetch) |
| `result_cache_ttl_extensions` | Entries kept alive longer by a hit (adaptive TTL) |
| `result_cache_stores` | Entries written |
| `result_cache_bytes_served` / `result_cache_bytes_stored` | Bytes served from / written to the cache |
| `result_cache_read_after_write_primary_reads` | Reads sent to the primary because a table they read was just written |
| `result_cache_redis_errors` | Failed Redis calls |

## Configuration

Add this section to `pgdog.toml`:

```toml
[result_cache]
enabled = true
redis_url = "redis://127.0.0.1:6379"
# redis_mode = "cluster"
# redis_urls = ["redis://10.0.0.1:6379", "redis://10.0.0.2:6379"]
# redis_mode = "sentinel"
# redis_urls = ["redis://10.0.0.1:26379", "redis://10.0.0.2:26379"]
# redis_sentinel_master = "mymaster"
expire_seconds = 30
max_entry_bytes = 524288
key_prefix = "pgdog:result_cache"
xfetch_beta = 1.0
# adaptive_ttl_max_seconds = 300  # off by default
singleflight_lock_ms = 1000       # 0 = don't coalesce between PgDogs
read_after_write_window_ms = 1000 # with replicas; 0 = off

# Redis timeouts: read "Redis timeouts" above before changing them.
# redis_connect_timeout_ms = 500
# redis_response_timeout_ms = 500
# redis_connect_retries = 1
# redis_unavailable_backoff_ms = 5000

# Optional allow/deny lists (regex). Unsafe lists take precedence.
cache_safe_schema_list = []
cache_unsafe_schema_list = []
cache_safe_table_list = []
cache_unsafe_table_list = []
```

## Current limitations

- Over the extended protocol, only one statement per `Sync` (see [Extended protocol](#extended-protocol)).
- Skips caching inside explicit transactions.
- Session signature is conservative and may lead to fewer cache hits (by design).

