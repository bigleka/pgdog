//! Tables a statement reads or writes, for tagging and invalidating cached
//! results.

use std::fmt::{self, Display};

use pg_raw_parse::{Node, nodes::ObjectType, walk::{Recurse, walk_manual}};

use crate::frontend::router::parser::{Ast, Table};

/// A table named in a query. Cache tags ignore aliases: only the schema, if
/// named, and the name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct OwnedTable {
    /// Table name.
    pub name: String,
    /// Schema name, if named.
    pub schema: Option<String>,
}

impl From<Table<'_>> for OwnedTable {
    fn from(table: Table<'_>) -> Self {
        Self {
            name: table.name.to_owned(),
            schema: table.schema.map(str::to_owned),
        }
    }
}

impl Display for OwnedTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let table = Table {
            name: &self.name,
            schema: self.schema.as_deref(),
            alias: None,
        };
        table.fmt(f)
    }
}

/// Tables the statements in `ast` read or write, each once.
pub(crate) fn tables(ast: &Ast) -> Vec<OwnedTable> {
    let mut found = Vec::new();
    for statement in ast.ast.stmts() {
        statement_tables(statement, &mut found);
    }

    let mut tables: Vec<OwnedTable> = Vec::with_capacity(found.len());
    for table in found {
        let table = OwnedTable::from(table);
        if !tables.contains(&table) {
            tables.push(table);
        }
    }
    tables
}

fn statement_tables<'a>(statement: Node<'a>, tables: &mut Vec<Table<'a>>) {
    match statement {
        // The expression walker knows queries, not utility statements.
        Node::SelectStmt(_)
        | Node::InsertStmt(_)
        | Node::UpdateStmt(_)
        | Node::DeleteStmt(_)
        | Node::MergeStmt(_) => {
            walk_manual::<()>(statement, |node| {
                if let Node::RangeVar(range_var) = node {
                    tables.push(Table::from(range_var));
                }
                Recurse::yes()
            });
        }
        Node::ExplainStmt(explain) => statement_tables(explain.query(), tables),
        Node::CreateTableAsStmt(create) => {
            if let Some(range_var) = create.into().and_then(|into| into.rel()) {
                tables.push(Table::from(range_var));
            }
            statement_tables(create.query(), tables);
        }
        Node::CopyStmt(copy) => match copy.relation() {
            Some(range_var) => tables.push(Table::from(range_var)),
            None => statement_tables(copy.query(), tables),
        },
        Node::CreateStmt(create) => tables.extend(create.relation().map(Table::from)),
        Node::AlterTableStmt(alter) => tables.extend(alter.relation().map(Table::from)),
        Node::RenameStmt(rename) => tables.extend(rename.relation().map(Table::from)),
        Node::TruncateStmt(truncate) => tables.extend(
            truncate
                .relations()
                .into_iter()
                .filter_map(|relation| Table::try_from(relation).ok()),
        ),
        Node::DropStmt(drop) if drop.remove_type == ObjectType::OBJECT_TABLE => tables.extend(
            drop.objects()
                .into_iter()
                .filter_map(|object| match object {
                    Node::NodeList(name) => Table::try_from(name).ok(),
                    _ => None,
                }),
        ),
        _ => (),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn tables_of(query: &str) -> Vec<String> {
        let ast = Ast::parse(query).expect("query parses");
        let mut names: Vec<_> = tables(&ast).iter().map(ToString::to_string).collect();
        names.sort();
        names
    }

    #[test]
    fn test_select_tables() {
        assert_eq!(
            tables_of("SELECT * FROM a JOIN s.b x ON true WHERE id IN (SELECT id FROM c)"),
            ["\"a\"", "\"c\"", "\"s\".\"b\""]
        );
    }

    #[test]
    fn test_select_cte_and_union() {
        assert_eq!(
            tables_of("WITH w AS (SELECT * FROM a) SELECT * FROM w UNION SELECT * FROM b"),
            ["\"a\"", "\"b\"", "\"w\""]
        );
    }

    #[test]
    fn test_dml_tables() {
        assert_eq!(tables_of("INSERT INTO a SELECT * FROM b"), ["\"a\"", "\"b\""]);
        assert_eq!(tables_of("UPDATE s.a SET x = 1 FROM b"), ["\"b\"", "\"s\".\"a\""]);
        assert_eq!(tables_of("DELETE FROM a USING b"), ["\"a\"", "\"b\""]);
    }

    #[test]
    fn test_each_table_once() {
        assert_eq!(tables_of("SELECT * FROM a, a x, a y"), ["\"a\""]);
    }

    #[test]
    fn test_ddl_tables() {
        assert_eq!(tables_of("CREATE TABLE s.a (id int)"), ["\"s\".\"a\""]);
        assert_eq!(tables_of("DROP TABLE a, s.b"), ["\"a\"", "\"s\".\"b\""]);
        assert_eq!(tables_of("TRUNCATE a, s.b"), ["\"a\"", "\"s\".\"b\""]);
        assert_eq!(tables_of("ALTER TABLE a ADD COLUMN x int"), ["\"a\""]);
        assert_eq!(tables_of("ALTER TABLE a RENAME TO b"), ["\"a\""]);
        assert_eq!(tables_of("CREATE TABLE a AS SELECT * FROM b"), ["\"a\"", "\"b\""]);
        assert_eq!(tables_of("COPY a FROM STDIN"), ["\"a\""]);
    }

    #[test]
    fn test_drop_other_objects_names_no_table() {
        assert!(tables_of("DROP INDEX a").is_empty());
    }

    #[test]
    fn test_explain_tables() {
        assert_eq!(tables_of("EXPLAIN ANALYZE DELETE FROM a"), ["\"a\""]);
    }

    #[test]
    fn test_multiple_statements() {
        assert_eq!(tables_of("SELECT 1 FROM a; UPDATE b SET x = 1"), ["\"a\"", "\"b\""]);
    }

    #[test]
    fn test_statements_without_tables() {
        for query in ["BEGIN", "SET search_path TO s", "SHOW work_mem", "VACUUM", "SELECT 1"] {
            assert!(tables_of(query).is_empty(), "{query}");
        }
    }
}
