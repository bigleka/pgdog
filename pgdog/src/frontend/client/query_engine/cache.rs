//! Result cache: serve reads from Redis, capture results to store them, and
//! invalidate the tables writes touch.

use std::{
    mem,
    time::{Instant, SystemTime},
};

use super::*;
use crate::{
    backend::Cluster,
    frontend::client::TransactionType,
    net::{Protocol, ToBytes},
    result_cache::{
        CacheableRequest,
        entry::Entry,
        lock::{Lock, Remote},
        read_after_write_window, reply_for,
        singleflight::Flight,
        statements::only_reads,
        tables::tables,
    },
    stats::ResultCache as CacheStats,
};

impl QueryEngine {
    /// Serve the request from the result cache if it can, or get ready to
    /// store its result. Returns `true` if the reply came from the cache.
    pub(super) async fn result_cache(
        &mut self,
        context: &mut QueryEngineContext<'_>,
        client_request: &mut ClientRequest,
    ) -> Result<bool, Error> {
        // Lazily initialize result cache (if enabled).
        if self.result_cache.is_none() && !context.admin {
            self.result_cache = RedisResultCache::global().await;
        }

        // What this statement does to the transaction, for the result cache.
        match client_request.ast.as_ref() {
            Some(ast) => self.transaction_cache.observe(ast),
            None if client_request.is_executable() => self.transaction_cache.unknown(),
            None => (),
        }

        // Table-based invalidation: remember tables touched by writes. Their
        // cache entries are dropped once the writes are visible to other
        // sessions, i.e. when the transaction (explicit or implicit) ends.
        if self.result_cache.is_some()
            && client_request.is_executable()
            && let Some(ast) = client_request.ast.as_ref()
            // Not the route: it comes from the first statement only.
            && !only_reads(ast)
        {
            for table in tables(ast) {
                if !self.pending_invalidations.contains(&table) {
                    self.pending_invalidations.push(table);
                }
            }
        }

        self.read_after_own_write(client_request);

        // A pipeline gets one ReadyForQuery for all its requests: a cached
        // reply, which ends with one, can't stand in for one of them.
        if !matches!(context.pipeline, Pipeline::None) || !client_request.is_executable() {
            return Ok(false);
        }

        let (Some(cache), Ok(cluster)) = (&self.result_cache, self.backend.cluster()) else {
            return Ok(false);
        };

        // Outside transactions, or in read-only READ COMMITTED ones.
        let in_transaction = context.in_transaction();
        if in_transaction && !self.cache_in_transaction(context) {
            return Ok(false);
        }
        let Some(req) = CacheableRequest::from_client_request(client_request) else {
            return Ok(false);
        };
        let user = context.params.get_required("user").unwrap_or("unknown");
        let Some(key) = cache
            .build_key(cluster.name(), user, context.params, &req)
            .await
            // A transaction may already run on a replica, which can miss the
            // write: don't cache what it reads.
            .filter(|key| !(in_transaction && key.read_primary && has_replicas(cluster)))
        else {
            return Ok(false);
        };

        // Read-after-write: a table it reads was just written, here or
        // through another PgDog.
        if key.read_primary && has_replicas(cluster) {
            read_primary(client_request);
        }

        let (leader, remote_lock) = match cache.get(&key).await {
            Some(cached) => {
                // XFetch: lead an early refresh unless someone, here or in
                // another PgDog, already is.
                let refresh = match cached
                    .refresh_early
                    .then(|| cache.singleflight().join(&key.redis_key))
                {
                    Some(Flight::Leader(leader)) => match cache.lock(&key).await {
                        Lock::Acquired(lock) => Some((leader, Some(lock))),
                        Lock::Unavailable => Some((leader, None)),
                        Lock::Held => None,
                    },
                    _ => None,
                };

                match refresh {
                    Some((leader, lock)) => {
                        CacheStats::early_refresh();
                        (Some(leader), lock)
                    }
                    None => {
                        let payload = &cached.entry.payload;
                        CacheStats::hit(payload.len());
                        self.send_cached(context, payload).await?;
                        return Ok(true);
                    }
                }
            }
            None => {
                CacheStats::miss();

                // Coalesce concurrent misses: one client runs the query, the
                // others wait for its result.
                match cache.singleflight().join(&key.redis_key) {
                    // Same between PgDogs sharing Redis.
                    Flight::Leader(leader) => match cache.lock_or_wait(&key).await {
                        Remote::Lead(lock) => (Some(leader), lock),
                        Remote::Served(entry) => {
                            CacheStats::remote_coalesced(entry.payload.len());
                            self.send_cached(context, &entry.payload).await?;
                            leader.finish(entry.payload);
                            return Ok(true);
                        }
                    },
                    Flight::Follower(follower) => match follower.wait().await {
                        Some(payload) => {
                            CacheStats::coalesced(payload.len());
                            self.send_cached(context, &payload).await?;
                            return Ok(true);
                        }
                        // Leader got no result to share: run the query ourselves.
                        None => (None, None),
                    },
                }
            }
        };

        self.result_cache_capture = Some(ResultCacheCapture {
            key,
            db: cluster.name().to_string(),
            tables: req.tables,
            bytes: Vec::new(),
            errored: false,
            leader,
            remote_lock,
            started: Instant::now(),
        });

        Ok(false)
    }

    /// Send a cached reply, with the client's transaction status.
    async fn send_cached(
        &mut self,
        context: &mut QueryEngineContext<'_>,
        payload: &[u8],
    ) -> Result<(), Error> {
        let in_transaction = context.in_transaction();
        let sent = context
            .stream
            .send_raw_flush(&reply_for(payload, in_transaction))
            .await?;
        self.stats.sent(sent);
        if !in_transaction {
            self.backend.mirror_flush();
        }
        Ok(())
    }

    /// Before a server message goes to the client: record it for the cache,
    /// and drop the entries of tables written once the writes are visible.
    pub(super) async fn result_cache_message(
        &mut self,
        context: &QueryEngineContext<'_>,
        message: &Message,
    ) {
        let code = message.code();
        if let Some(capture) = self.result_cache_capture.as_mut() {
            if code == 'E' {
                capture.errored = true;
            }
            if !capture.errored {
                capture.bytes.extend_from_slice(&message.to_bytes());
            }
        }

        // Invalidate before the client sees ReadyForQuery, so any session
        // reading after the write completed gets fresh results.
        if code == 'Z' && !context.in_transaction() {
            self.invalidate_pending().await;
        }
    }

    /// After ReadyForQuery: store the captured result.
    pub(super) async fn result_cache_store(&mut self, context: &QueryEngineContext<'_>) {
        let Some(capture) = self.result_cache_capture.take() else {
            return;
        };
        if capture.errored || (context.in_transaction() && !self.cache_in_transaction(context)) {
            return;
        }

        let delta = capture.started.elapsed();
        let entry = Entry {
            delta,
            // When the query started: the data is as of then.
            computed_at: SystemTime::now()
                .checked_sub(delta)
                .unwrap_or_else(SystemTime::now),
            payload: capture.bytes,
        };
        if let Some(cache) = self.result_cache.as_ref() {
            cache
                .set_with_table_tags(&capture.db, &capture.key, &entry, &capture.tables)
                .await;
            CacheStats::store(entry.payload.len());
        }

        // Stored first, so clients arriving after the flight ends hit the cache.
        if let Some(leader) = capture.leader {
            leader.finish(entry.payload);
        }
        if let Some(lock) = capture.remote_lock {
            lock.release().await;
        }
    }

    /// Drop result cache entries for tables written by this client.
    ///
    /// Call only once the client is outside a transaction, so the writes are
    /// visible to everyone reading from the database.
    pub(super) async fn invalidate_pending(&mut self) {
        if self.pending_invalidations.is_empty() {
            return;
        }

        let tables = mem::take(&mut self.pending_invalidations);
        if read_after_write_window().is_some() {
            let now = Instant::now();
            for table in &tables {
                self.recent_writes.retain(|(written, _)| written != table);
                self.recent_writes.push((table.clone(), now));
            }
        }
        if let (Some(cache), Ok(cluster)) = (&self.result_cache, self.backend.cluster()) {
            cache.invalidate_tables(cluster.name(), &tables).await;
        }
    }

    /// Send reads of tables this session wrote within the read-after-write
    /// window to the primary, cached or not.
    fn read_after_own_write(&mut self, client_request: &mut ClientRequest) {
        if self.recent_writes.is_empty() || !client_request.route().use_replica() {
            return;
        }
        let Some(window) = read_after_write_window() else {
            self.recent_writes.clear();
            return;
        };
        self.recent_writes
            .retain(|(_, written_at)| written_at.elapsed() < window);

        let reads_recent_write = client_request.ast.as_ref().is_some_and(|ast| {
            tables(ast).iter().any(|table| {
                self.recent_writes.iter().any(|(written, _)| {
                    written.name == table.name
                        && may_be_same_schema(written.schema.as_deref(), table.schema.as_deref())
                })
            })
        });
        if reads_recent_write && self.backend.cluster().is_ok_and(has_replicas) {
            read_primary(client_request);
        }
    }

    /// The result cache can serve this transaction: read-only, only plain
    /// reads in it so far, and `READ COMMITTED`, so each statement sees the
    /// latest data anyway, as an autocommit read would.
    fn cache_in_transaction(&self, context: &QueryEngineContext<'_>) -> bool {
        let assume_read_committed = config()
            .config
            .result_cache
            .as_ref()
            .is_some_and(|result_cache| result_cache.cache_read_only_transactions);
        context.transaction().map(|t| t.transaction_type()) == Some(TransactionType::ReadOnly)
            && self
                .transaction_cache
                .cacheable(context.params, assume_read_committed)
    }
}

/// A table named without a schema is whichever one the session's search_path
/// finds, so it can be the table of that name in any schema.
fn may_be_same_schema(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    }
}

/// Without replicas every read already runs on the primary.
fn has_replicas(cluster: &Cluster) -> bool {
    cluster.shards().iter().any(|shard| shard.has_replicas())
}

/// Run this read on the primary.
fn read_primary(client_request: &mut ClientRequest) {
    if let Some(route) = client_request.route.as_mut()
        && route.use_replica()
    {
        route.set_read_primary();
        CacheStats::read_primary();
    }
}
