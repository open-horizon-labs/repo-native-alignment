//! SQL migration extractor.
//!
//! Parses `.sql` files using `sqlparser-rs` to extract:
//! - `CREATE TABLE` -> `NodeKind::SqlTable` nodes
//! - Column definitions -> `NodeKind::Other("sql_column")` nodes + `EdgeKind::HasField` edges
//! - `ALTER TABLE` -> `EdgeKind::Evolves` edges
//! - `FOREIGN KEY` references -> `EdgeKind::DependsOn` edges between tables

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use sqlparser::ast::{AlterTableOperation, ColumnOption, DataType, Statement};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

/// SQL migration extractor using sqlparser-rs.
pub struct SqlExtractor;

impl Default for SqlExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl SqlExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for SqlExtractor {
    fn extensions(&self) -> &[&str] {
        &["sql"]
    }

    fn name(&self) -> &str {
        "sql"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let dialect = GenericDialect {};
        let statements = match Parser::parse_sql(&dialect, content) {
            Ok(stmts) => stmts,
            Err(e) => {
                tracing::debug!("SQL parse error in {}: {}", path.display(), e);
                return Ok(ExtractionResult::default());
            }
        };

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        for stmt in &statements {
            match stmt {
                Statement::CreateTable(create_table) => {
                    let table_name = create_table.name.to_string();

                    let table_node_id = NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: table_name.clone(),
                        kind: NodeKind::SqlTable,
                    };

                    nodes.push(Node {
                        id: table_node_id.clone(),
                        language: "sql".to_string(),
                        line_start: 0,
                        line_end: 0,
                        signature: format!("CREATE TABLE {}", table_name),
                        body: stmt.to_string(),
                        metadata: BTreeMap::new(),
                        source: ExtractionSource::Schema,
                    });

                    // Extract columns
                    for column in &create_table.columns {
                        let col_name = column.name.to_string();
                        let col_type = format_data_type(&column.data_type);

                        let col_node_id = NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name: format!("{}.{}", table_name, col_name),
                            kind: NodeKind::Other("sql_column".to_string()),
                        };

                        let mut metadata = BTreeMap::new();
                        metadata.insert("column_type".to_string(), col_type.clone());
                        metadata.insert("table".to_string(), table_name.clone());

                        nodes.push(Node {
                            id: col_node_id.clone(),
                            language: "sql".to_string(),
                            line_start: 0,
                            line_end: 0,
                            signature: format!("{} {}", col_name, col_type),
                            body: format!("{} {}", col_name, col_type),
                            metadata,
                            source: ExtractionSource::Schema,
                        });

                        edges.push(Edge {
                            from: table_node_id.clone(),
                            to: col_node_id,
                            kind: EdgeKind::HasField,
                            source: ExtractionSource::Schema,
                            confidence: Confidence::Detected,
                        });

                        // Check for REFERENCES (foreign key on column)
                        for option in &column.options {
                            if let ColumnOption::ForeignKey(fk) = &option.option {
                                let ref_table = fk.foreign_table.to_string();
                                edges.push(Edge {
                                    from: table_node_id.clone(),
                                    to: NodeId {
                                        root: String::new(),
                                        file: path.to_path_buf(),
                                        name: ref_table,
                                        kind: NodeKind::SqlTable,
                                    },
                                    kind: EdgeKind::DependsOn,
                                    source: ExtractionSource::Schema,
                                    confidence: Confidence::Detected,
                                });
                            }
                        }
                    }

                    // Check table-level constraints for foreign keys
                    for constraint in &create_table.constraints {
                        if let sqlparser::ast::TableConstraint::ForeignKey(fk) = constraint {
                            let ref_table = fk.foreign_table.to_string();
                            edges.push(Edge {
                                from: table_node_id.clone(),
                                to: NodeId {
                                    root: String::new(),
                                    file: path.to_path_buf(),
                                    name: ref_table,
                                    kind: NodeKind::SqlTable,
                                },
                                kind: EdgeKind::DependsOn,
                                source: ExtractionSource::Schema,
                                confidence: Confidence::Detected,
                            });
                        }
                    }
                }

                Statement::AlterTable(alter) => {
                    let table_name = alter.name.to_string();
                    let alter_node_id = NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: format!("ALTER {}", table_name),
                        kind: NodeKind::Other("sql_alter".to_string()),
                    };

                    nodes.push(Node {
                        id: alter_node_id.clone(),
                        language: "sql".to_string(),
                        line_start: 0,
                        line_end: 0,
                        signature: format!("ALTER TABLE {}", table_name),
                        body: stmt.to_string(),
                        metadata: BTreeMap::new(),
                        source: ExtractionSource::Schema,
                    });

                    // Evolves edge to the original table
                    edges.push(Edge {
                        from: alter_node_id.clone(),
                        to: NodeId {
                            root: String::new(),
                            file: path.to_path_buf(),
                            name: table_name.clone(),
                            kind: NodeKind::SqlTable,
                        },
                        kind: EdgeKind::Evolves,
                        source: ExtractionSource::Schema,
                        confidence: Confidence::Detected,
                    });

                    // Extract added columns from ALTER TABLE operations
                    for op in &alter.operations {
                        if let AlterTableOperation::AddColumn { column_def, .. } = op {
                            let col_name = column_def.name.to_string();
                            let col_type = format_data_type(&column_def.data_type);

                            let col_node_id = NodeId {
                                root: String::new(),
                                file: path.to_path_buf(),
                                name: format!("{}.{}", table_name, col_name),
                                kind: NodeKind::Other("sql_column".to_string()),
                            };

                            let mut metadata = BTreeMap::new();
                            metadata.insert("column_type".to_string(), col_type.clone());
                            metadata.insert("table".to_string(), table_name.clone());
                            metadata.insert("via_alter".to_string(), "true".to_string());

                            nodes.push(Node {
                                id: col_node_id.clone(),
                                language: "sql".to_string(),
                                line_start: 0,
                                line_end: 0,
                                signature: format!("{} {}", col_name, col_type),
                                body: format!("ADD COLUMN {} {}", col_name, col_type),
                                metadata,
                                source: ExtractionSource::Schema,
                            });

                            edges.push(Edge {
                                from: alter_node_id.clone(),
                                to: col_node_id,
                                kind: EdgeKind::HasField,
                                source: ExtractionSource::Schema,
                                confidence: Confidence::Detected,
                            });
                        }
                    }
                }

                Statement::CreateType {
                    name,
                    representation,
                } => {
                    // PostgreSQL: CREATE TYPE status AS ENUM ('active', 'inactive');
                    let type_name = name.to_string();
                    if let Some(sqlparser::ast::UserDefinedTypeRepresentation::Enum { labels }) =
                        representation
                    {
                        for label in labels {
                            let label_str = label.value.clone();
                            if !label_str.is_empty() {
                                let mut metadata = BTreeMap::new();
                                metadata.insert("value".to_string(), label_str.clone());
                                metadata.insert("synthetic".to_string(), "false".to_string());
                                nodes.push(Node {
                                    id: NodeId {
                                        root: String::new(),
                                        file: path.to_path_buf(),
                                        name: format!("{}.{}", type_name, label_str),
                                        kind: NodeKind::Const,
                                    },
                                    language: "sql".to_string(),
                                    line_start: 0,
                                    line_end: 0,
                                    signature: format!("{} ENUM value", type_name),
                                    body: label_str,
                                    metadata,
                                    source: ExtractionSource::Schema,
                                });
                            }
                        }
                    }
                }

                _ => {} // Skip other statement types
            }
        }

        Ok(ExtractionResult { nodes, edges })
    }
}

/// Format a sqlparser DataType to a readable string.
fn format_data_type(dt: &DataType) -> String {
    dt.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_create_table() {
        let extractor = SqlExtractor::new();
        let content = r#"
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    email VARCHAR(255) UNIQUE
);
"#;
        let result = extractor
            .extract(Path::new("migrations/001_create_users.sql"), content)
            .unwrap();

        // Table node
        let tables: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::SqlTable)
            .collect();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].id.name, "users");

        // Column nodes
        let columns: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("sql_column".to_string()))
            .collect();
        assert_eq!(columns.len(), 3, "Should find 3 columns");

        // HasField edges
        let field_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::HasField)
            .collect();
        assert_eq!(field_edges.len(), 3);
    }

    #[test]
    fn test_extract_alter_table() {
        let extractor = SqlExtractor::new();
        let content = r#"
ALTER TABLE users ADD COLUMN age INTEGER;
"#;
        let result = extractor
            .extract(Path::new("migrations/002_add_age.sql"), content)
            .unwrap();

        // ALTER node
        let alters: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("sql_alter".to_string()))
            .collect();
        assert_eq!(alters.len(), 1);

        // Evolves edge
        let evolves_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Evolves)
            .collect();
        assert_eq!(evolves_edges.len(), 1);
        assert_eq!(evolves_edges[0].to.name, "users");

        // Column added via ALTER
        let columns: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("sql_column".to_string()))
            .collect();
        assert_eq!(columns.len(), 1);
        assert_eq!(columns[0].id.name, "users.age");
    }

    #[test]
    fn test_extract_foreign_key() {
        let extractor = SqlExtractor::new();
        let content = r#"
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    user_id INTEGER,
    FOREIGN KEY (user_id) REFERENCES users(id)
);
"#;
        let result = extractor
            .extract(Path::new("migrations/003_create_orders.sql"), content)
            .unwrap();

        // DependsOn edge from orders to users
        let dep_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert_eq!(dep_edges.len(), 1);
        assert_eq!(dep_edges[0].from.name, "orders");
        assert_eq!(dep_edges[0].to.name, "users");
    }

    #[test]
    fn test_extract_multiple_tables() {
        let extractor = SqlExtractor::new();
        let content = r#"
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name VARCHAR(255)
);

CREATE TABLE posts (
    id INTEGER PRIMARY KEY,
    title VARCHAR(255),
    author_id INTEGER
);
"#;
        let result = extractor
            .extract(Path::new("migrations/001_init.sql"), content)
            .unwrap();

        let tables: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::SqlTable)
            .collect();
        assert_eq!(tables.len(), 2);
    }

    #[test]
    fn test_sql_extractor_extensions() {
        let extractor = SqlExtractor::new();
        assert_eq!(extractor.extensions(), &["sql"]);
        assert_eq!(extractor.name(), "sql");
    }

    #[test]
    fn test_sql_handles_parse_errors_gracefully() {
        let extractor = SqlExtractor::new();
        let content = "THIS IS NOT VALID SQL AT ALL ;;;;";
        let result = extractor.extract(Path::new("bad.sql"), content).unwrap();
        // Should return empty result, not error
        assert!(result.nodes.is_empty());
    }

    // -----------------------------------------------------------------------
    // Regression / iceberg suite for SQL extractor.
    //
    // Sibling exercise to the proto extractor's iceberg suite (#647 / #648).
    // sql.rs delegates to sqlparser-rs (a real AST parser), so we don't expect
    // proto-style panics. These tests verify edge-case behavior on weird-but-valid
    // (and weird-and-invalid) inputs.
    // -----------------------------------------------------------------------

    #[test]
    fn regression_empty_file() {
        let extractor = SqlExtractor::new();
        let result = extractor.extract(Path::new("empty.sql"), "").unwrap();
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
    }

    #[test]
    fn regression_comment_only_line() {
        let extractor = SqlExtractor::new();
        let content = "-- just a comment\n";
        let result = extractor.extract(Path::new("c.sql"), content).unwrap();
        assert!(result.nodes.is_empty());
    }

    #[test]
    fn regression_block_comment_only() {
        let extractor = SqlExtractor::new();
        let content = "/* block\n   comment */\n";
        let result = extractor.extract(Path::new("c.sql"), content).unwrap();
        assert!(result.nodes.is_empty());
    }

    #[test]
    fn regression_single_line_empty_table() {
        let extractor = SqlExtractor::new();
        let content = "CREATE TABLE Foo();\n";
        let result = extractor.extract(Path::new("e.sql"), content).unwrap();
        let tables: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::SqlTable)
            .collect();
        assert_eq!(tables.len(), 1, "single-line empty table should be extracted");
        assert_eq!(tables[0].id.name, "Foo");
        let cols: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("sql_column".to_string()))
            .collect();
        assert!(cols.is_empty(), "empty table has zero columns");
    }

    #[test]
    fn regression_single_line_table_with_column() {
        let extractor = SqlExtractor::new();
        let content = "CREATE TABLE Foo (id INT);\n";
        let result = extractor.extract(Path::new("f.sql"), content).unwrap();
        let tables: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::SqlTable)
            .collect();
        let cols: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("sql_column".to_string()))
            .collect();
        assert_eq!(tables.len(), 1);
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].id.name, "Foo.id");
    }

    #[test]
    fn regression_create_if_not_exists() {
        let extractor = SqlExtractor::new();
        let content = "CREATE TABLE IF NOT EXISTS Foo (id INT);\n";
        let result = extractor.extract(Path::new("f.sql"), content).unwrap();
        let tables: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::SqlTable)
            .collect();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].id.name, "Foo");
    }

    #[test]
    fn regression_drop_table_unhandled() {
        // sqlparser-rs parses DROP fine; the extractor doesn't model drops.
        // Test pins the behavior so a future feature is intentional, not accidental.
        let extractor = SqlExtractor::new();
        let content = "DROP TABLE Foo;\n";
        let result = extractor.extract(Path::new("d.sql"), content).unwrap();
        assert!(
            result.nodes.is_empty(),
            "DROP currently produces no nodes (documented; revisit if schema-history tracking is added)"
        );
    }

    #[test]
    fn regression_inline_foreign_key() {
        let extractor = SqlExtractor::new();
        let content = r#"
CREATE TABLE users (id INT PRIMARY KEY);
CREATE TABLE orders (
    id INT PRIMARY KEY,
    user_id INT REFERENCES users(id)
);
"#;
        let result = extractor.extract(Path::new("fk.sql"), content).unwrap();
        let deps: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert_eq!(deps.len(), 1, "inline REFERENCES should produce one DependsOn edge");
        assert_eq!(deps[0].from.name, "orders");
        assert_eq!(deps[0].to.name, "users");
    }

    #[test]
    fn regression_self_referential_foreign_key() {
        let extractor = SqlExtractor::new();
        let content = r#"
CREATE TABLE nodes (
    id INT PRIMARY KEY,
    parent_id INT,
    FOREIGN KEY (parent_id) REFERENCES nodes(id)
);
"#;
        let result = extractor.extract(Path::new("self.sql"), content).unwrap();
        let deps: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].from.name, "nodes");
        assert_eq!(deps[0].to.name, "nodes", "self-FK target should be the same table");
    }

    #[test]
    fn regression_create_alter_create_in_one_file() {
        let extractor = SqlExtractor::new();
        let content = r#"
CREATE TABLE users (id INT);
ALTER TABLE users ADD COLUMN name VARCHAR(255);
CREATE TABLE posts (id INT);
"#;
        let result = extractor.extract(Path::new("multi.sql"), content).unwrap();
        let tables: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::SqlTable)
            .collect();
        let alters: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("sql_alter".to_string()))
            .collect();
        assert_eq!(tables.len(), 2, "both CREATEs produce table nodes");
        assert_eq!(alters.len(), 1, "the ALTER produces an alter node");
    }

    #[test]
    fn regression_postgres_serial_under_generic_dialect_is_graceful() {
        // Postgres `SERIAL` may or may not parse under GenericDialect across
        // sqlparser versions. Either way the extractor must not panic; if the
        // statement parses, we get a table node; if it fails, we get empty.
        let extractor = SqlExtractor::new();
        let content = "CREATE TABLE Foo (id SERIAL PRIMARY KEY);\n";
        let result = extractor.extract(Path::new("pg.sql"), content).unwrap();
        // No assertion on count — just verifying graceful behavior.
        let _ = result.nodes.len();
    }
}
