//! What a query's statements do, as far as the result cache cares.

use pg_query::{protobuf::SelectStmt, NodeEnum, ParseResult};

/// A `SELECT` that only reads: no `FOR UPDATE`/`FOR SHARE`, `INTO`, or
/// data-modifying `WITH`, in it or in its set operations.
pub fn plain_select(select: &SelectStmt) -> bool {
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

/// Every statement in the query is a plain `SELECT`. A query is routed by
/// its first statement, which can be a read followed by writes.
pub fn only_reads(statements: &ParseResult) -> bool {
    let statements = &statements.protobuf.stmts;
    !statements.is_empty()
        && statements.iter().all(|statement| {
            matches!(
                statement.stmt.as_ref().and_then(|stmt| stmt.node.as_ref()),
                Some(NodeEnum::SelectStmt(select)) if plain_select(select)
            )
        })
}

#[cfg(test)]
mod test {
    use super::*;

    fn reads(query: &str) -> bool {
        only_reads(&pg_query::parse(query).unwrap())
    }

    #[test]
    fn test_only_reads() {
        for query in [
            "SELECT 1",
            "SELECT * FROM a; SELECT * FROM b",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "SELECT 1 UNION SELECT 2",
        ] {
            assert!(reads(query), "{query}");
        }
    }

    #[test]
    fn test_writes() {
        for query in [
            "SELECT 1; UPDATE a SET v = 1",
            "UPDATE a SET v = 1; SELECT 1",
            "SELECT * FROM a FOR UPDATE",
            "SELECT * INTO b FROM a",
            "WITH x AS (DELETE FROM a RETURNING *) SELECT * FROM x",
            "SELECT 1 UNION (SELECT 1 FROM a FOR UPDATE)",
            "SHOW work_mem",
            "SET search_path TO s",
        ] {
            assert!(!reads(query), "{query}");
        }
    }
}
