# PgDog Result Cache (Redis)

PgDog can cache **read query results** in a Redis-compatible store (**Redis**, **Valkey**, or **Dragonfly**) and serve them back as the original PostgreSQL wire messages. The goal is lower read latency without teaching clients about a cache.

The cache is **off by default**. Enable it with a `[result_cache]` section in `pgdog.toml`.

## What gets cached

On a cacheable **read**, PgDog stores the backend response payload (typically `RowDescription` + `DataRow*` + `CommandComplete` + `ReadyForQuery`) and, on a later hit, flushes those bytes to the client without contacting Postgres.

A request is cacheable when:

- Result cache is enabled and Redis is reachable.
- The request is **executable** (simple `Query`, or extended-protocol `Execute`).
- The router classified the statement as a **read**.
- Every table in the parsed AST passes the schema/table allow/deny lists (empty allow lists mean “allow all”).
- The captured response is non-empty, did not include an `ErrorResponse`, and is under `max_entry_bytes`.

When the cache is enabled, PgDog **forces the query parser** for `SELECT` / `INSERT` / `UPDATE` / `DELETE` so it can extract table names for caching and invalidation.

## Cache key

Keys look like:

```text
{key_prefix}:v1:{db}:{user}:{sessionSig}:{routeSig}:{fingerprint}
```

| Component | Meaning |
|-----------|---------|
| `key_prefix` | Configurable prefix (default `pgdog:result_cache`) |
| `db` | Logical database name from `pgdog.toml` |
| `user` | Client user |
| `sessionSig` | MD5 of `search_path` (conservative; extra session state is not hashed) |
| `routeSig` | `direct` or `cross` |
| `fingerprint` | MD5 of SQL text plus bind-parameter bytes |

Entries that fail the table/schema filter are not stored or looked up.

## Invalidation (table tags)

Each stored entry is also added to Redis **sets** keyed by table:

```text
{key_prefix}:tbl:{db}:{schema}.{table}
```

(`schema` is `_` when the AST has no schema.)

On a write (`INSERT` / `UPDATE` / `DELETE` / other write statements PgDog can classify), PgDog:

1. Extracts tables from the AST.
2. **Deletes the tagged cache keys immediately** (conservative: other sessions will not see stale rows after the write is parsed).
3. Records the tables as pending invalidation for the session.

On a successful **COMMIT** (or implicit commit when not in an explicit transaction), pending tables are invalidated again. On **ROLLBACK**, pending tags are dropped; keys already deleted at write time stay deleted.

Tag sets themselves expire with a TTL of `max(60s, 2 × expire_seconds)`.

Row-level invalidation (`granular_invalidation_enabled`) is **reserved** and has no effect yet.

## Read-after-write consistency

When `read_after_write_consistency_enabled` is true (default), a session that recently wrote to a table **bypasses the cache** for reads of that table for `read_after_write_window_ms` (default 2000 ms). This covers the case where invalidation has not yet been observed, or the same session must see its own writes.

## Stampede protection

### In-process singleflight

Concurrent misses for the **same key on the same PgDog process** share one backend query. Followers wait up to `singleflight_timeout_ms` (default 5000). On timeout they fall through to Postgres.

### Distributed singleflight

Across PgDog processes, a Redis lock (`{key_prefix}:lock:{redis_key}`) is taken with NX + PX. If the lock is not acquired, the waiter polls Redis until the key appears or `distributed_singleflight_timeout_ms` (default 3000) elapses.

Both features default to **on**.

## Adaptive TTL

On each hit, if `adaptive_ttl_enabled` is true (default), remaining TTL is extended by `expire_seconds`, capped at `max_expire_seconds` (default 300). Hot keys therefore live longer than cold ones.

When adaptive TTL is **off**, a hit still **refreshes** the key back to `expire_seconds` (sliding expiration).

Default `expire_seconds` is **30** if omitted.

## XFetch

`xfetch_enabled` / `xfetch_beta` evaluate a probabilistic “refresh soon” condition on hits (XFetch: `-beta * delta * ln(u) > remaining_ttl`).

**Current behavior:** when the condition fires, PgDog increments the `result_cache_xfetch_triggers` metric only. It does **not** yet recompute the query in the background. The client still receives the cached payload.

## Encryption

If `encryption_key` is set, values are encrypted with **AES-256-GCM** before `SET` and decrypted on `GET`. The configured string is hashed with **SHA-256** to a 256-bit key. Clients never see ciphertext.

Omit `encryption_key` to store plaintext wire payloads in Redis. Treat the key as a secret; do not commit it.

If decryption fails (wrong key, corrupt blob), the lookup is treated as a **miss**.

## Schema and table filters

Each list entry is a **regular expression**.

| Setting | Role |
|---------|------|
| `cache_unsafe_schema_list` | Deny schema (wins over allow) |
| `cache_unsafe_table_list` | Deny table; matched against `table` and `schema.table` |
| `cache_safe_schema_list` | If non-empty, schema must match |
| `cache_safe_table_list` | If non-empty, table must match |

Unsafe always wins. Empty allow lists allow everything that was not denied.

## Configuration

```toml
[result_cache]
enabled = true
redis_url = "redis://127.0.0.1:6379"
expire_seconds = 30
max_entry_bytes = 524288
key_prefix = "pgdog:result_cache"
# encryption_key = "a-long-random-secret"

singleflight_enabled = true
singleflight_timeout_ms = 5000
distributed_singleflight_enabled = true
distributed_singleflight_timeout_ms = 3000

adaptive_ttl_enabled = true
max_expire_seconds = 300

xfetch_enabled = true
xfetch_beta = 1.0

read_after_write_consistency_enabled = true
read_after_write_window_ms = 2000

# granular_invalidation_enabled = false  # reserved, unused

cache_safe_schema_list = []
cache_unsafe_schema_list = []
cache_safe_table_list = []
cache_unsafe_table_list = []
```

| Option | Default when omitted | Notes |
|--------|----------------------|--------|
| `enabled` | `false` | Cache is unused unless true and Redis connects |
| `redis_url` | empty | e.g. `redis://127.0.0.1:6379` |
| `expire_seconds` | `30` | Entry TTL |
| `max_entry_bytes` | `524288` (512 KiB) | Larger responses are not stored |
| `key_prefix` | `pgdog:result_cache` | Prefix for values, tags, and locks |
| `encryption_key` | unset | AES-256-GCM when set |
| `singleflight_enabled` | `true` | In-process coalescing |
| `singleflight_timeout_ms` | `5000` | Follower wait |
| `distributed_singleflight_enabled` | `true` | Redis NX lock |
| `distributed_singleflight_timeout_ms` | `3000` | Cross-node wait / lock PX |
| `adaptive_ttl_enabled` | `true` | Extend TTL on hit |
| `max_expire_seconds` | `300` | Cap for adaptive TTL |
| `xfetch_enabled` | `true` | Metric-only today |
| `xfetch_beta` | `1.0` | XFetch aggressiveness |
| `read_after_write_consistency_enabled` | `true` | Session bypass after writes |
| `read_after_write_window_ms` | `2000` | Bypass window |
| `granular_invalidation_enabled` | unset | No effect |
| `cache_*_list` | `[]` | Regex allow/deny |

See also [`example.pgdog.toml`](../example.pgdog.toml).

## Metrics

When OpenMetrics is enabled, PgDog exports counters including:

| Metric | Meaning |
|--------|---------|
| `result_cache_hits` | Cached payloads served (includes some coalesced waits) |
| `result_cache_misses` | Cacheable read not found |
| `result_cache_stores` | Entries written |
| `result_cache_bytes_served` / `result_cache_bytes_stored` | Payload sizes |
| `result_cache_redis_errors` | Redis failures |
| `result_cache_singleflight_joined` | In-process followers that received a leader result |
| `result_cache_singleflight_timeouts` | Follower timeouts |
| `result_cache_distributed_singleflight_joined` | Waiters that got a key from another node |
| `result_cache_ttl_extended` | Adaptive TTL extensions |
| `result_cache_xfetch_triggers` | XFetch condition true (no background refresh yet) |
| `result_cache_read_after_write_bypasses` | Session consistency skip |

## Limitations

- Session signature only includes `search_path`. Other GUC / role settings that change results can produce **stale or wrong hits** unless you disable the cache or use deny lists.
- Writes invalidate **immediately** (not only after commit). A rolled-back transaction can still drop cache entries for the tables it touched.
- Cross-shard reads can be cached (`routeSig=cross`); invalidation still depends on tables parsed from the AST.
- Admin connections do not use the result cache.
- Redis must speak the Redis protocol; clustering/Sentinel URLs depend on the `redis` crate connection string you pass in `redis_url`.
