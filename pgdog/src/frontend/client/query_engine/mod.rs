use std::{mem, time::Instant};

use crate::{
    backend::pool::{Connection, Request},
    config::config,
    frontend::{
        client::query_engine::{hooks::QueryEngineHooks, route_query::ClusterCheck},
        client::TransactionType,
        router::{parser::Shard, Route},
        BufferedQuery, Client, ClientComms, Command, Error, Router, RouterContext, Stats,
    },
    net::{ErrorResponse, Message, Parameters},
    result_cache::{
        lock::{Lock, Remote, RemoteLock},
        read_after_write_window,
        redis::RedisResultCache,
        reply_for,
        singleflight::{Flight, Leader},
        CacheableRequest,
    },
    state::State,
};

use tracing::debug;

pub mod advisory_lock;
pub mod connect;
pub mod context;
pub mod deallocate;
pub mod discard;
pub mod end_transaction;
pub mod fake;
pub mod hooks;
pub mod incomplete_requests;
pub mod internal_values;
pub mod multi_step;
pub mod notify_buffer;
pub mod pub_sub;
pub mod query;
pub mod rewrite;
pub mod route_query;
pub mod set;
pub mod shard_key_rewrite;
pub mod start_transaction;
#[cfg(test)]
mod test;
#[cfg(test)]
mod testing;
mod transaction_cache;
pub mod two_pc;
pub mod unknown_command;

use self::query::ExplainResponseState;
pub(crate) use advisory_lock::AdvisoryLocks;
pub use context::QueryEngineContext;
use notify_buffer::NotifyBuffer;
use transaction_cache::TransactionCache;
pub use two_pc::phase::TwoPcPhase;
use two_pc::TwoPc;

#[derive(Debug)]
pub struct QueryEngine {
    begin_stmt: Option<BufferedQuery>,
    router: Router,
    comms: ClientComms,
    stats: Stats,
    backend: Connection,
    streaming: bool,
    two_pc: TwoPc,
    notify_buffer: NotifyBuffer,
    pending_explain: Option<ExplainResponseState>,
    hooks: QueryEngineHooks,
    advisory_locks: AdvisoryLocks,
    result_cache: Option<RedisResultCache>,
    result_cache_capture: Option<ResultCacheCapture>,
    pending_invalidations: Vec<crate::frontend::router::parser::OwnedTable>,
    /// Tables this session wrote, and when: its reads of them go to the
    /// primary for the read-after-write window.
    recent_writes: Vec<(crate::frontend::router::parser::OwnedTable, Instant)>,
    /// What the client's statements tell about its transaction, to know when
    /// the result cache can serve it.
    transaction_cache: TransactionCache,
}

#[derive(Debug)]
struct ResultCacheCapture {
    key: crate::result_cache::ResultCacheKey,
    db: String,
    tables: Vec<crate::frontend::router::parser::OwnedTable>,
    bytes: Vec<u8>,
    errored: bool,
    /// Set when this client runs the query for others waiting on the same key.
    /// Dropping it without a result releases them.
    leader: Option<Leader>,
    /// Lock making other PgDogs wait for this result. Dropping it without a
    /// result lets them run the query.
    remote_lock: Option<RemoteLock>,
    /// When the query started, to record how long it takes to compute.
    started: Instant,
}

impl QueryEngine {
    /// Create new query engine.
    pub fn new(params: &Parameters, comms: &ClientComms, admin: bool) -> Result<Self, Error> {
        let user = params.get_required("user")?;
        let database = params.get_default("database", user);

        let backend = Connection::new(user, database, admin)?;

        Ok(Self {
            backend,
            comms: comms.clone(),
            hooks: QueryEngineHooks::new(),
            stats: Stats::default(),
            streaming: bool::default(),
            two_pc: TwoPc::default(),
            notify_buffer: NotifyBuffer::default(),
            pending_explain: None,
            begin_stmt: None,
            router: Router::default(),
            advisory_locks: AdvisoryLocks::default(),
            result_cache: None,
            result_cache_capture: None,
            pending_invalidations: Vec::new(),
            recent_writes: Vec::new(),
            transaction_cache: TransactionCache::default(),
        })
    }

    pub fn from_client(client: &Client) -> Result<Self, Error> {
        Self::new(&client.params, &client.comms, client.admin)
    }

    /// Wait for an async message from the backend.
    pub async fn read_backend(&mut self) -> Result<Message, Error> {
        Ok(self.backend.read().await?)
    }

    /// Client can safely disconnect (no active backend connection or pending transaction).
    pub fn can_disconnect(&self) -> bool {
        self.begin_stmt.is_none() && self.backend.done()
    }

    /// Current state.
    pub fn client_state(&self) -> State {
        self.stats.state
    }

    /// Handle client request.
    pub async fn handle(&mut self, context: &mut QueryEngineContext<'_>) -> Result<(), Error> {
        self.stats
            .received(context.client_request.total_message_len());
        self.set_state(State::Active); // Client is active.

        // Rewrite prepared statements.
        self.rewrite_extended(context)?;

        if let ClusterCheck::Offline = self.cluster_check(context).await? {
            return Ok(());
        }

        // Rewrite statement if necessary.
        if !self.parse_and_rewrite(context).await? {
            return Ok(());
        }

        // Intercept commands we don't have to forward to a server.
        if self.intercept_incomplete(context).await? {
            self.update_stats(context);
            return Ok(());
        }

        // Route transaction to the right servers.
        if !self.route_query(context).await? {
            self.update_stats(context);
            debug!("query has nowhere to go");
            return Ok(());
        }

        self.hooks.before_execution(context)?;

        // Lazily initialize result cache (if enabled).
        if self.result_cache.is_none() && !context.admin {
            self.result_cache = RedisResultCache::global().await;
        }

        // What this statement does to the transaction, for the result cache.
        match context.client_request.ast.as_ref() {
            Some(ast) => self.transaction_cache.observe(ast.parse_result()),
            None if context.client_request.is_executable() => self.transaction_cache.unknown(),
            None => (),
        }

        // Table-based invalidation: remember tables touched by writes. Their cache
        // entries are dropped once the writes are visible to other sessions, i.e.
        // when the transaction (explicit or implicit) ends.
        if self.result_cache.is_some()
            && context.client_request.route().is_write()
            && context.client_request.is_executable()
        {
            if let Some(ast) = context.client_request.ast.as_ref() {
                for table in ast.tables() {
                    // Cache tags ignore aliases, so dedup on schema + name only.
                    let seen = self.pending_invalidations.iter().any(|pending| {
                        pending.name == table.name && pending.schema.as_deref() == table.schema
                    });
                    if !seen {
                        self.pending_invalidations.push(table.to_owned());
                    }
                }
            }
        }

        self.read_after_own_write(context);

        // Cache lookup: outside transactions, or in read-only READ COMMITTED ones.
        if let (Some(ref cache), Ok(cluster)) = (&self.result_cache, self.backend.cluster()) {
            let in_transaction = context.in_transaction();
            if (!in_transaction || self.cache_in_transaction(context))
                && context.client_request.is_executable()
            {
                if let Some(req) = CacheableRequest::from_client_request(context.client_request) {
                    let user = context.params.get_required("user").unwrap_or("unknown");
                    if let Some(key) = cache
                        .build_key(cluster.name(), user, context.params, &req)
                        .await
                        // A transaction may already run on a replica, which
                        // can miss the write: don't cache what it reads.
                        .filter(|key| {
                            !(in_transaction && key.read_primary && has_replicas(cluster))
                        })
                    {
                        // Read-after-write: a table it reads was just written,
                        // here or through another PgDog.
                        if key.read_primary && has_replicas(cluster) {
                            read_primary(context);
                        }

                        let (leader, remote_lock) = match cache.get(&key).await {
                            Some(cached) => {
                                // XFetch: lead an early refresh unless someone,
                                // here or in another PgDog, already is.
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
                                        crate::stats::ResultCache::early_refresh();
                                        (Some(leader), lock)
                                    }
                                    None => {
                                        let payload = &cached.entry.payload;
                                        crate::stats::ResultCache::hit(payload.len());
                                        let bytes_sent = context
                                            .stream
                                            .send_raw_flush(&reply_for(payload, in_transaction))
                                            .await?;
                                        self.stats.sent(bytes_sent);
                                        self.set_state(idle(in_transaction));
                                        return Ok(());
                                    }
                                }
                            }
                            None => {
                                crate::stats::ResultCache::miss();

                                // Coalesce concurrent misses: one client runs the query,
                                // the others wait for its result.
                                match cache.singleflight().join(&key.redis_key) {
                                    // Same between PgDogs sharing Redis.
                                    Flight::Leader(leader) => {
                                        match cache.lock_or_wait(&key).await {
                                            Remote::Lead(lock) => (Some(leader), lock),
                                            Remote::Served(entry) => {
                                                crate::stats::ResultCache::remote_coalesced(
                                                    entry.payload.len(),
                                                );
                                                let bytes_sent = context
                                                    .stream
                                                    .send_raw_flush(&reply_for(
                                                        &entry.payload,
                                                        in_transaction,
                                                    ))
                                                    .await?;
                                                self.stats.sent(bytes_sent);
                                                self.set_state(idle(in_transaction));
                                                leader.finish(entry.payload);
                                                return Ok(());
                                            }
                                        }
                                    }
                                    Flight::Follower(follower) => {
                                        if let Some(payload) = follower.wait().await {
                                            crate::stats::ResultCache::coalesced(payload.len());
                                            let bytes_sent = context
                                                .stream
                                                .send_raw_flush(&reply_for(
                                                    &payload,
                                                    in_transaction,
                                                ))
                                                .await?;
                                            self.stats.sent(bytes_sent);
                                            self.set_state(idle(in_transaction));
                                            return Ok(());
                                        }
                                        // Leader got no result to share: run the query ourselves.
                                        (None, None)
                                    }
                                }
                            }
                        };

                        self.result_cache_capture = Some(ResultCacheCapture {
                            key,
                            db: cluster.name().to_string(),
                            tables: req.tables.clone(),
                            bytes: Vec::new(),
                            errored: false,
                            leader,
                            remote_lock,
                            started: Instant::now(),
                        });
                    }
                }
            }
        }

        // Queue up request to mirrors, if any.
        // Do this before sending query to actual server
        // to have accurate timings between queries.
        self.backend.mirror(context.client_request);

        self.pending_explain = None;

        let command = self.router.command();

        if let Some(trace) = context
            .client_request
            .route // Admin commands don't have a route.
            .as_mut()
            .and_then(|route| route.take_explain())
        {
            if config().config.general.expanded_explain {
                self.pending_explain = Some(ExplainResponseState::new(trace));
            }
        }

        match command {
            Command::InternalField { name, value } => {
                self.show_internal_value(context, name.clone(), value.clone())
                    .await?
            }
            Command::UniqueId => self.unique_id(context).await?,
            Command::StartTransaction {
                query,
                transaction_type,
                extended,
                ..
            } => {
                self.start_transaction(context, query.clone(), *transaction_type, *extended)
                    .await?
            }
            Command::CommitTransaction { extended } => {
                self.backend.lock(self.advisory_locks.locked());
                self.stats.locked(self.advisory_locks.locked());

                if self.backend.connected() || *extended {
                    let extended = *extended;
                    let transaction_route =
                        self.transaction_route(context.client_request.route())?;
                    context.client_request.route = Some(transaction_route.clone());
                    context.cross_shard_disabled = Some(false);
                    self.end_connected(context, false, extended).await?;
                } else {
                    self.end_not_connected(context, false, *extended).await?
                }

                if context.params.commit() {
                    self.comms.update_params(context.params);
                }
            }
            Command::RollbackTransaction { extended } => {
                self.backend.lock(self.advisory_locks.locked());
                self.stats.locked(self.advisory_locks.locked());

                if self.backend.connected() || *extended {
                    let extended = *extended;
                    let transaction_route =
                        self.transaction_route(context.client_request.route())?;
                    context.client_request.route = Some(transaction_route.clone());
                    context.cross_shard_disabled = Some(false);
                    self.end_connected(context, true, extended).await?;
                } else {
                    self.end_not_connected(context, true, *extended).await?
                }

                context.params.rollback();
            }
            Command::Query(_) => self.execute(context).await?,
            Command::Listen { .. } | Command::Notify { .. } | Command::Unlisten(_)
                if self.backend.session_mode() =>
            {
                self.execute(context).await?
            }
            Command::Listen { channel, shard } => {
                self.listen(context, &channel.clone(), shard.clone())
                    .await?
            }
            Command::Notify {
                channel,
                payload,
                shard,
            } => {
                self.notify(context, &channel.clone(), &payload.clone(), &shard.clone())
                    .await?
            }
            Command::Unlisten(channel) => self.unlisten(context, &channel.clone()).await?,
            Command::Set { params, .. } => {
                let params = params.clone();
                self.set(context, &params).await?;
            }
            Command::ResetAll => {
                self.reset_all(context).await?;
            }
            Command::Copy(_) => self.execute(context).await?,
            Command::Deallocate => self.deallocate(context).await?,
            Command::Discard { extended } => self.discard(context, *extended).await?,
            command => self.unknown_command(context, command.clone()).await?,
        }

        self.hooks.after_execution(context)?;

        if context.in_error() {
            self.backend.mirror_clear();
            self.notify_buffer.clear();
        } else if !context.in_transaction() {
            self.backend.mirror_flush();
            self.flush_notify().await?;
        }

        self.update_stats(context);

        Ok(())
    }

    /// Drop result cache entries for tables written by this client.
    ///
    /// Call only once the client is outside a transaction, so the writes are
    /// visible to everyone reading from the database.
    async fn invalidate_pending(&mut self) {
        if self.pending_invalidations.is_empty() {
            return;
        }

        let tables = mem::take(&mut self.pending_invalidations);
        if read_after_write_window().is_some() {
            let now = Instant::now();
            for table in &tables {
                self.recent_writes
                    .retain(|(written, _)| !same_table(written, table));
                self.recent_writes.push((table.clone(), now));
            }
        }
        if let (Some(cache), Ok(cluster)) = (&self.result_cache, self.backend.cluster()) {
            cache.invalidate_tables(cluster.name(), &tables).await;
        }
    }

    /// Send reads of tables this session wrote within the read-after-write
    /// window to the primary, cached or not.
    fn read_after_own_write(&mut self, context: &mut QueryEngineContext<'_>) {
        if self.recent_writes.is_empty() || !context.client_request.route().use_replica() {
            return;
        }
        let Some(window) = read_after_write_window() else {
            self.recent_writes.clear();
            return;
        };
        self.recent_writes
            .retain(|(_, written_at)| written_at.elapsed() < window);

        let reads_recent_write = context.client_request.ast.as_ref().is_some_and(|ast| {
            ast.tables().iter().any(|table| {
                self.recent_writes.iter().any(|(written, _)| {
                    written.name == table.name
                        && may_be_same_schema(written.schema.as_deref(), table.schema)
                })
            })
        });
        if reads_recent_write && self.backend.cluster().is_ok_and(has_replicas) {
            read_primary(context);
        }
    }

    fn update_stats(&mut self, context: &mut QueryEngineContext<'_>) {
        let state = if self.backend.has_more_messages() {
            State::Active
        } else {
            match context.in_transaction() {
                true => State::IdleInTransaction,
                false => State::Idle,
            }
        };

        self.stats.state = state;

        self.stats
            .prepared_statements(context.prepared_statements.len_local());
        self.stats.memory_used(context.memory_stats);

        self.comms.update_stats(self.stats);
    }

    pub fn set_state(&mut self, state: State) {
        self.stats.state = state;
        self.comms.update_stats(self.stats);
    }

    pub fn get_state(&self) -> State {
        self.stats.state
    }

    /// Check if the backend protocol is out of sync due to an error in extended protocol.
    pub fn out_of_sync(&self) -> bool {
        self.backend.out_of_sync()
    }
}

impl QueryEngine {
    /// The result cache can serve this transaction: read-only, only plain
    /// reads in it so far, and `READ COMMITTED`, so each statement sees the
    /// latest data anyway, as an autocommit read would.
    fn cache_in_transaction(&self, context: &QueryEngineContext<'_>) -> bool {
        let assume_read_committed = config()
            .config
            .result_cache
            .as_ref()
            .is_some_and(|result_cache| result_cache.cache_read_only_transactions);
        context.transaction() == Some(TransactionType::ReadOnly)
            && self
                .transaction_cache
                .cacheable(context.params, assume_read_committed)
    }
}

/// State after a reply served from the cache.
fn idle(in_transaction: bool) -> State {
    if in_transaction {
        State::IdleInTransaction
    } else {
        State::Idle
    }
}

/// Cache tags ignore aliases: compare schema and name only.
fn same_table(
    a: &crate::frontend::router::parser::OwnedTable,
    b: &crate::frontend::router::parser::OwnedTable,
) -> bool {
    a.name == b.name && a.schema == b.schema
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
fn has_replicas(cluster: &crate::backend::Cluster) -> bool {
    cluster.shards().iter().any(|shard| shard.has_replicas())
}

/// Run this read on the primary.
fn read_primary(context: &mut QueryEngineContext<'_>) {
    if let Some(route) = context.client_request.route.as_mut() {
        if route.use_replica() {
            route.set_read_primary();
            crate::stats::ResultCache::read_primary();
        }
    }
}
