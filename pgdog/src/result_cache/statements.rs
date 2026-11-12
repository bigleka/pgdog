//! What a query's statements do, as far as the result cache cares.

use std::ops::ControlFlow;

use pg_raw_parse::{
    Node, nodes,
    walk::{Recurse, walk_manual},
};

use crate::frontend::router::parser::Ast;

/// A `SELECT` that only reads: no `FOR UPDATE`/`FOR SHARE`, `INTO`, or
/// data-modifying `WITH`, anywhere in it.
pub(crate) fn plain_select(select: &nodes::SelectStmt) -> bool {
    let writes = walk_manual(Node::SelectStmt(select), |node| match node {
        Node::LockingClause(_) | Node::IntoClause(_) => ControlFlow::Break(()),
        Node::SelectStmt(select) if select.into_clause().is_some() => ControlFlow::Break(()),
        Node::CommonTableExpr(cte) if !matches!(cte.ctequery(), Node::SelectStmt(_)) => {
            ControlFlow::Break(())
        }
        _ => Recurse::yes(),
    });
    writes.is_none()
}

/// Every statement in the query is a plain `SELECT`. A query is routed by
/// its first statement, which can be a read followed by writes.
pub(crate) fn only_reads(ast: &Ast) -> bool {
    let mut statements = ast.ast.stmts().peekable();
    statements.peek().is_some()
        && statements
            .all(|statement| matches!(statement, Node::SelectStmt(select) if plain_select(select)))
}

#[cfg(test)]
mod test {
    use super::*;

    fn reads(query: &str) -> bool {
        only_reads(&Ast::parse(query).unwrap())
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
            "SELECT * FROM a WHERE id IN (SELECT id FROM b FOR SHARE)",
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
