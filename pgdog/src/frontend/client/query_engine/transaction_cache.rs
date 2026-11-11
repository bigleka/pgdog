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

use pg_raw_parse::{Node, nodes::TransactionStmtKind::*};

use crate::{
    frontend::router::parser::Ast,
    parameters::{ParameterStore, ParameterValue},
    result_cache::statements::plain_select,
};

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
    fn from_options<'a>(options: impl IntoIterator<Item = Node<'a>>) -> Option<Self> {
        options.into_iter().find_map(|option| match option {
            Node::DefElem(def) if def.defname() == Some("transaction_isolation") => {
                match def.arg() {
                    Node::A_Const(value) => value.val()?.string_value().map(Self::from_name),
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
    pub(super) fn observe(&mut self, statements: &Ast) {
        for statement in statements.ast.stmts() {
            match statement {
                Node::TransactionStmt(stmt)
                    if matches!(stmt.kind, TRANS_STMT_BEGIN | TRANS_STMT_START) =>
                {
                    self.isolation = Isolation::from_options(stmt.options());
                    self.changed = false;
                }
                Node::TransactionStmt(_) | Node::VariableShowStmt(_) => (),
                Node::VariableSetStmt(stmt) if stmt.name() == Some("TRANSACTION") => {
                    if let Some(isolation) = Isolation::from_options(stmt.args()) {
                        self.isolation = Some(isolation);
                    }
                }
                Node::VariableSetStmt(stmt) if stmt.name() == Some("SESSION CHARACTERISTICS") => {
                    if let Some(isolation) = Isolation::from_options(stmt.args()) {
                        self.session_snapshot = isolation == Isolation::Snapshot;
                    }
                }
                Node::SelectStmt(select) if plain_select(select) => (),
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
    pub(super) fn cacheable(&self, params: &ParameterStore, assume_default: bool) -> bool {
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

#[cfg(test)]
mod test {
    use super::*;

    fn after(statements: &[&str]) -> TransactionCache {
        let mut transaction = TransactionCache::default();
        for statement in statements {
            transaction.observe(&Ast::parse(statement).unwrap());
        }
        transaction
    }

    fn cacheable(statements: &[&str], assume_default: bool) -> bool {
        after(statements).cacheable(&ParameterStore::default(), assume_default)
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
        let mut params = ParameterStore::default();
        params.set("default_transaction_isolation", "serializable");
        assert!(!transaction.cacheable(&params, true));
        params.set("default_transaction_isolation", "read committed");
        assert!(transaction.cacheable(&params, true));
    }

    #[test]
    fn test_unparsed_statement_stops_cache() {
        let begin = Ast::parse("BEGIN ISOLATION LEVEL READ COMMITTED").unwrap();
        let mut transaction = TransactionCache::default();
        transaction.observe(&begin);
        transaction.unknown();
        assert!(!transaction.cacheable(&ParameterStore::default(), true));
        transaction.observe(&begin);
        assert!(transaction.cacheable(&ParameterStore::default(), false));
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
