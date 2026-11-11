//! Whether the result cache can serve the client's transaction.
//!
//! Every `READ COMMITTED` statement takes a new snapshot, so a cached result
//! is as good as one from an autocommit read. `REPEATABLE READ` and
//! `SERIALIZABLE` statements must all see the transaction's snapshot: the
//! cache can't serve them.
//!
//! Inside a read-only transaction the router sends every statement as a
//! read, writes to temporary tables included, so its route can't tell what
//! the statement does: only plain `SELECT`s go through the cache.

use pg_query::{
    protobuf::{a_const::Val, SelectStmt, TransactionStmtKind},
    Node, NodeEnum, ParseResult,
};

use crate::net::{parameter::ParameterValue, Parameters};

/// Isolation level named by a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Isolation {
    /// `READ COMMITTED`, or `READ UNCOMMITTED`, which Postgres runs as it.
    ReadCommitted,
    /// `REPEATABLE READ` or `SERIALIZABLE`.
    Snapshot,
}

impl Isolation {
    fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "read committed" | "read uncommitted" => Self::ReadCommitted,
            // Unknown names count as the strictest.
            _ => Self::Snapshot,
        }
    }

    /// `ISOLATION LEVEL ...` among BEGIN or SET TRANSACTION options.
    fn from_options(options: &[Node]) -> Option<Self> {
        options
            .iter()
            .find_map(|option| match option.node.as_ref()? {
                NodeEnum::DefElem(def) if def.defname == "transaction_isolation" => {
                    match def.arg.as_ref()?.node.as_ref()? {
                        NodeEnum::AConst(value) => match value.val.as_ref()? {
                            Val::Sval(name) => Some(Self::from_name(&name.sval)),
                            _ => None,
                        },
                        _ => None,
                    }
                }
                _ => None,
            })
    }
}

/// What the client's statements tell about its transaction.
#[derive(Debug, Default)]
pub(super) struct TransactionCache {
    /// Named by the current transaction's BEGIN or a SET TRANSACTION in it.
    isolation: Option<Isolation>,
    /// `SET SESSION CHARACTERISTICS` made `REPEATABLE READ` or
    /// `SERIALIZABLE` the session's default.
    session_snapshot: bool,
    /// The current transaction ran something besides plain reads: a write
    /// (to a temporary table, in a read-only one), or a SET, which can
    /// change what the same query returns.
    changed: bool,
}

impl TransactionCache {
    /// Note what the statements do to the transaction.
    pub(super) fn observe(&mut self, statements: &ParseResult) {
        for statement in &statements.protobuf.stmts {
            let Some(node) = statement.stmt.as_ref().and_then(|stmt| stmt.node.as_ref()) else {
                self.changed = true;
                continue;
            };
            match node {
                NodeEnum::TransactionStmt(stmt)
                    if matches!(
                        stmt.kind(),
                        TransactionStmtKind::TransStmtBegin | TransactionStmtKind::TransStmtStart
                    ) =>
                {
                    self.isolation = Isolation::from_options(&stmt.options);
                    self.changed = false;
                }
                NodeEnum::TransactionStmt(_) | NodeEnum::VariableShowStmt(_) => (),
                NodeEnum::VariableSetStmt(stmt) if stmt.name == "TRANSACTION" => {
                    if let Some(isolation) = Isolation::from_options(&stmt.args) {
                        self.isolation = Some(isolation);
                    }
                }
                NodeEnum::VariableSetStmt(stmt) if stmt.name == "SESSION CHARACTERISTICS" => {
                    if let Some(isolation) = Isolation::from_options(&stmt.args) {
                        self.session_snapshot = isolation == Isolation::Snapshot;
                    }
                }
                NodeEnum::SelectStmt(select) if plain_select(select) => (),
                _ => self.changed = true,
            }
        }
    }

    /// A statement that wasn't parsed: it could do anything.
    pub(super) fn unknown(&mut self) {
        self.changed = true;
    }

    /// The cache can serve the current transaction's reads: it ran only
    /// plain reads so far, and is known to be `READ COMMITTED`, or doesn't
    /// name a level, nothing the client set makes its default stricter, and
    /// `assume_default` says the default is `READ COMMITTED`.
    pub(super) fn cacheable(&self, params: &Parameters, assume_default: bool) -> bool {
        if self.changed {
            return false;
        }
        match self.isolation {
            Some(isolation) => isolation == Isolation::ReadCommitted,
            None => {
                let session_default = match params.get("default_transaction_isolation") {
                    Some(ParameterValue::String(name)) => Some(Isolation::from_name(name)),
                    _ => None,
                };
                assume_default
                    && !self.session_snapshot
                    && session_default != Some(Isolation::Snapshot)
            }
        }
    }
}

/// A `SELECT` that only reads: no `FOR UPDATE`/`FOR SHARE`, `INTO`, or
/// data-modifying `WITH`, in it or in its set operations.
fn plain_select(select: &SelectStmt) -> bool {
    let ctes_read = select.with_clause.as_ref().is_none_or(|with| {
        with.ctes.iter().all(|cte| match cte.node.as_ref() {
            Some(NodeEnum::CommonTableExpr(cte)) => matches!(
                cte.ctequery.as_ref().and_then(|query| query.node.as_ref()),
                Some(NodeEnum::SelectStmt(query)) if plain_select(query)
            ),
            _ => false,
        })
    });
    let side_reads = |side: &Option<Box<SelectStmt>>| side.as_deref().is_none_or(plain_select);

    ctes_read
        && select.locking_clause.is_empty()
        && select.into_clause.is_none()
        && side_reads(&select.larg)
        && side_reads(&select.rarg)
}

#[cfg(test)]
mod test {
    use super::*;

    fn after(statements: &[&str]) -> TransactionCache {
        let mut transaction = TransactionCache::default();
        for statement in statements {
            transaction.observe(&pg_query::parse(statement).unwrap());
        }
        transaction
    }

    fn cacheable(statements: &[&str], assume_default: bool) -> bool {
        after(statements).cacheable(&Parameters::default(), assume_default)
    }

    #[test]
    fn test_begin_names_level() {
        for begin in [
            "BEGIN ISOLATION LEVEL READ COMMITTED",
            "BEGIN READ ONLY ISOLATION LEVEL READ COMMITTED",
            "START TRANSACTION ISOLATION LEVEL READ COMMITTED, READ ONLY",
            "BEGIN ISOLATION LEVEL READ UNCOMMITTED",
        ] {
            assert!(cacheable(&[begin], false), "{begin}");
        }
        for begin in [
            "BEGIN ISOLATION LEVEL REPEATABLE READ",
            "BEGIN READ ONLY ISOLATION LEVEL SERIALIZABLE",
            "START TRANSACTION ISOLATION LEVEL SERIALIZABLE",
        ] {
            assert!(!cacheable(&[begin], true), "{begin}");
        }
    }

    #[test]
    fn test_default_level_needs_assumption() {
        assert!(!cacheable(&["BEGIN READ ONLY"], false));
        assert!(cacheable(&["BEGIN READ ONLY"], true));
    }

    #[test]
    fn test_set_transaction_overrides_begin() {
        assert!(!cacheable(
            &[
                "BEGIN ISOLATION LEVEL READ COMMITTED",
                "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE"
            ],
            true
        ));
        assert!(cacheable(
            &["BEGIN", "SET TRANSACTION ISOLATION LEVEL READ COMMITTED"],
            false
        ));
        assert!(cacheable(
            &["BEGIN READ ONLY", "SET TRANSACTION READ ONLY"],
            true
        ));
    }

    /// Each BEGIN starts over.
    #[test]
    fn test_next_transaction_starts_over() {
        assert!(cacheable(
            &[
                "BEGIN ISOLATION LEVEL SERIALIZABLE",
                "INSERT INTO t VALUES (1)",
                "COMMIT",
                "BEGIN READ ONLY"
            ],
            true
        ));
    }

    #[test]
    fn test_session_characteristics() {
        let serializable =
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE";
        let read_committed_default =
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL READ COMMITTED";
        assert!(!cacheable(&[serializable, "BEGIN READ ONLY"], true));
        assert!(cacheable(
            &[serializable, read_committed_default, "BEGIN READ ONLY"],
            true
        ));
        // Named in the transaction: the session default doesn't matter.
        assert!(cacheable(
            &[serializable, "BEGIN ISOLATION LEVEL READ COMMITTED"],
            false
        ));
    }

    #[test]
    fn test_session_default_parameter() {
        let transaction = after(&["BEGIN READ ONLY"]);
        let mut params = Parameters::default();
        params.insert("default_transaction_isolation", "serializable");
        assert!(!transaction.cacheable(&params, true));
        params.insert("default_transaction_isolation", "read committed");
        assert!(transaction.cacheable(&params, true));
    }

    #[test]
    fn test_unparsed_statement_stops_cache() {
        let begin = pg_query::parse("BEGIN ISOLATION LEVEL READ COMMITTED").unwrap();
        let mut transaction = TransactionCache::default();
        transaction.observe(&begin);
        transaction.unknown();
        assert!(!transaction.cacheable(&Parameters::default(), true));
        transaction.observe(&begin);
        assert!(transaction.cacheable(&Parameters::default(), false));
    }

    #[test]
    fn test_plain_reads_keep_cache() {
        assert!(cacheable(
            &[
                "BEGIN ISOLATION LEVEL READ COMMITTED",
                "SELECT 1",
                "SELECT * FROM a JOIN b USING (id)",
                "WITH x AS (SELECT 1) SELECT * FROM x",
                "SELECT 1 UNION SELECT 2",
                "SHOW search_path",
                "SAVEPOINT s",
                "RELEASE SAVEPOINT s",
            ],
            false
        ));
    }

    #[test]
    fn test_anything_else_stops_cache_until_next_transaction() {
        for statement in [
            "INSERT INTO t VALUES (1)",
            "CREATE TEMP TABLE t (id INT)",
            "UPDATE t SET v = 1",
            "SELECT * FROM t FOR UPDATE",
            "SELECT * FROM t FOR SHARE",
            "SELECT * INTO u FROM t",
            "WITH x AS (INSERT INTO t VALUES (1) RETURNING *) SELECT * FROM x",
            "SELECT 1 UNION (SELECT * FROM t FOR UPDATE)",
            "SET LOCAL search_path TO other",
            "SET search_path TO other",
            "COPY t TO STDOUT",
        ] {
            let begin = "BEGIN ISOLATION LEVEL READ COMMITTED";
            assert!(!cacheable(&[begin, statement], false), "{statement}");
            assert!(
                !cacheable(&[begin, statement, "SELECT 1"], false),
                "{statement}, then a read"
            );
        }
    }
}
