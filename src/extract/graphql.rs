//! GraphQL schema extractor using tree-sitter.
//!
//! Extracts from `.graphql` and `.gql` files:
//! - Type definitions (object, interface, input, scalar, enum, union) → `NodeKind::Other("graphql_type")`
//! - Operations (query, mutation, subscription) → `NodeKind::Function`
//! - Fields within type definitions → `NodeKind::Field` + `EdgeKind::HasField` edges
//! - Fragments → `NodeKind::Other("graphql_fragment")`
//! - Implements relationships → `EdgeKind::Implements` edges
//!
//! Bridges schema → resolver via name matching: resolver implementations that
//! share a name with a GraphQL type or operation can be connected by the
//! graph query layer.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{
    Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind,
};

use super::{ExtractionResult, Extractor};

/// GraphQL schema extractor.
pub struct GraphQlExtractor;

impl GraphQlExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for GraphQlExtractor {
    fn extensions(&self) -> &[&str] {
        &["graphql", "gql"]
    }

    fn name(&self) -> &str {
        "graphql"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_graphql::LANGUAGE.into())?;

        let tree = match parser.parse(content, None) {
            Some(t) => t,
            None => return Ok(ExtractionResult::default()),
        };

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        let root = tree.root_node();
        let source = content.as_bytes();

        walk_node(root, path, source, &mut nodes, &mut edges);

        Ok(ExtractionResult { nodes, edges })
    }
}

/// Walk tree-sitter nodes recursively, extracting GraphQL definitions.
fn walk_node(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    match node.kind() {
        // Type system definitions
        "object_type_definition"
        | "interface_type_definition"
        | "input_object_type_definition"
        | "scalar_type_definition"
        | "enum_type_definition"
        | "union_type_definition" => {
            extract_type_definition(node, path, source, nodes, edges);
            return; // children handled inside
        }

        // Operations (query, mutation, subscription)
        "operation_definition" => {
            extract_operation(node, path, source, nodes, edges);
            return;
        }

        // Fragment definitions
        "fragment_definition" => {
            extract_fragment(node, path, source, nodes);
            return;
        }

        _ => {}
    }

    // Walk children
    let child_count = node.child_count();
    for i in 0..child_count {
        if let Some(child) = node.child(i as u32) {
            walk_node(child, path, source, nodes, edges);
        }
    }
}

/// Extract a type definition node (object, interface, input, scalar, enum, union).
fn extract_type_definition(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let type_kind = node.kind();

    let name = match find_child_text(node, "name", source) {
        Some(n) => n,
        None => return,
    };

    let line_start = node.start_position().row + 1;
    let line_end = node.end_position().row + 1;
    let body = node.utf8_text(source).unwrap_or("").to_string();

    let kind_keyword = match type_kind {
        "object_type_definition" => "type",
        "interface_type_definition" => "interface",
        "input_object_type_definition" => "input",
        "scalar_type_definition" => "scalar",
        "enum_type_definition" => "enum",
        "union_type_definition" => "union",
        _ => "type",
    };

    let signature = format!("{} {}", kind_keyword, name);

    let mut metadata = BTreeMap::new();
    metadata.insert("graphql_kind".to_string(), kind_keyword.to_string());

    let type_node_id = NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name: name.clone(),
        kind: NodeKind::Other("graphql_type".to_string()),
    };

    nodes.push(Node {
        id: type_node_id.clone(),
        language: "graphql".to_string(),
        line_start,
        line_end,
        signature,
        body,
        metadata,
        source: ExtractionSource::Schema,
    });

    // Extract implements interfaces → Implements edges
    if type_kind == "object_type_definition" || type_kind == "interface_type_definition" {
        extract_implements(node, path, source, &type_node_id, edges);
    }

    // Extract fields or enum values
    match type_kind {
        "object_type_definition" | "interface_type_definition" | "input_object_type_definition" => {
            extract_fields(node, path, source, &type_node_id, nodes, edges);
        }
        "enum_type_definition" => {
            extract_enum_values(node, path, source, &type_node_id, nodes, edges);
        }
        _ => {}
    }
}

/// Extract an operation definition (query, mutation, subscription, or anonymous shorthand).
fn extract_operation(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    // Operation type: query | mutation | subscription (absent for shorthand query)
    let op_type = find_child_kind_text(node, "operation_type", source)
        .unwrap_or_else(|| "query".to_string());

    // Name is optional for anonymous operations
    let name = find_child_text(node, "name", source)
        .unwrap_or_else(|| format!("<anonymous {}>", op_type));

    let line_start = node.start_position().row + 1;
    let line_end = node.end_position().row + 1;
    let body = node.utf8_text(source).unwrap_or("").to_string();
    let signature = format!("{} {}", op_type, name);

    let mut metadata = BTreeMap::new();
    metadata.insert("operation_type".to_string(), op_type.clone());

    let op_node_id = NodeId {
        root: String::new(),
        file: path.to_path_buf(),
        name,
        kind: NodeKind::Function,
    };

    nodes.push(Node {
        id: op_node_id.clone(),
        language: "graphql".to_string(),
        line_start,
        line_end,
        signature,
        body,
        metadata,
        source: ExtractionSource::Schema,
    });

    // Emit DependsOn edges to root selection fields
    extract_selection_type_refs(node, path, source, &op_node_id, edges);
}

/// Extract a fragment definition.
fn extract_fragment(
    node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    nodes: &mut Vec<Node>,
) {
    let name = match find_child_text(node, "fragment_name", source) {
        Some(n) => n,
        None => return,
    };

    let on_type = find_child_type_condition(node, source).unwrap_or_default();

    let line_start = node.start_position().row + 1;
    let line_end = node.end_position().row + 1;
    let body = node.utf8_text(source).unwrap_or("").to_string();
    let signature = if on_type.is_empty() {
        format!("fragment {}", name)
    } else {
        format!("fragment {} on {}", name, on_type)
    };

    let mut metadata = BTreeMap::new();
    if !on_type.is_empty() {
        metadata.insert("on_type".to_string(), on_type);
    }

    nodes.push(Node {
        id: NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name,
            kind: NodeKind::Other("graphql_fragment".to_string()),
        },
        language: "graphql".to_string(),
        line_start,
        line_end,
        signature,
        body,
        metadata,
        source: ExtractionSource::Schema,
    });
}

/// Extract field definitions from object/interface/input types.
fn extract_fields(
    type_node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    parent_id: &NodeId,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let fields_container = find_child_by_kind(type_node, "fields_definition")
        .or_else(|| find_child_by_kind(type_node, "input_fields_definition"));

    let container = match fields_container {
        Some(c) => c,
        None => return,
    };

    let child_count = container.child_count();
    for i in 0..child_count {
        let child = match container.child(i as u32) {
            Some(c) => c,
            None => continue,
        };

        if child.kind() != "field_definition" && child.kind() != "input_value_definition" {
            continue;
        }

        let field_name = match find_child_text(child, "name", source) {
            Some(n) => n,
            None => continue,
        };

        let field_type = extract_type_text(child, source);

        let line_start = child.start_position().row + 1;
        let line_end = child.end_position().row + 1;
        let body = child.utf8_text(source).unwrap_or("").to_string();

        let mut metadata = BTreeMap::new();
        metadata.insert("parent_type".to_string(), parent_id.name.clone());
        if !field_type.is_empty() {
            metadata.insert("field_type".to_string(), field_type.clone());
        }

        let field_node_id = NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: format!("{}.{}", parent_id.name, field_name),
            kind: NodeKind::Field,
        };

        nodes.push(Node {
            id: field_node_id.clone(),
            language: "graphql".to_string(),
            line_start,
            line_end,
            signature: body.lines().next().unwrap_or("").trim().to_string(),
            body,
            metadata,
            source: ExtractionSource::Schema,
        });

        edges.push(Edge {
            from: parent_id.clone(),
            to: field_node_id.clone(),
            kind: EdgeKind::HasField,
            source: ExtractionSource::Schema,
            confidence: Confidence::Detected,
        });

        // If field type references a custom (non-scalar) type, add DependsOn edge
        if !field_type.is_empty() {
            let bare_type = strip_type_modifiers(&field_type);
            if !is_builtin_scalar(bare_type) && !bare_type.is_empty() {
                edges.push(Edge {
                    from: field_node_id,
                    to: NodeId {
                        root: String::new(),
                        file: path.to_path_buf(),
                        name: bare_type.to_string(),
                        kind: NodeKind::Other("graphql_type".to_string()),
                    },
                    kind: EdgeKind::DependsOn,
                    source: ExtractionSource::Schema,
                    confidence: Confidence::Detected,
                });
            }
        }
    }
}

/// Extract enum values from an enum type definition.
fn extract_enum_values(
    type_node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    parent_id: &NodeId,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let values_container = match find_child_by_kind(type_node, "enum_values_definition") {
        Some(c) => c,
        None => return,
    };

    let child_count = values_container.child_count();
    for i in 0..child_count {
        let child = match values_container.child(i as u32) {
            Some(c) => c,
            None => continue,
        };

        if child.kind() != "enum_value_definition" {
            continue;
        }

        let value_name = match find_child_text(child, "enum_value", source) {
            Some(n) => n,
            None => continue,
        };

        let line_start = child.start_position().row + 1;
        let line_end = child.end_position().row + 1;
        let body = child.utf8_text(source).unwrap_or("").to_string();

        let mut metadata = BTreeMap::new();
        metadata.insert("parent_enum".to_string(), parent_id.name.clone());
        metadata.insert("synthetic".to_string(), "false".to_string());

        let value_node_id = NodeId {
            root: String::new(),
            file: path.to_path_buf(),
            name: format!("{}.{}", parent_id.name, value_name),
            kind: NodeKind::EnumVariant,
        };

        nodes.push(Node {
            id: value_node_id.clone(),
            language: "graphql".to_string(),
            line_start,
            line_end,
            signature: value_name,
            body,
            metadata,
            source: ExtractionSource::Schema,
        });

        edges.push(Edge {
            from: parent_id.clone(),
            to: value_node_id,
            kind: EdgeKind::HasField,
            source: ExtractionSource::Schema,
            confidence: Confidence::Detected,
        });
    }
}

/// Extract implements_interfaces and emit Implements edges.
fn extract_implements(
    type_node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    type_node_id: &NodeId,
    edges: &mut Vec<Edge>,
) {
    let impl_node = match find_child_by_kind(type_node, "implements_interfaces") {
        Some(n) => n,
        None => return,
    };

    let child_count = impl_node.child_count();
    for i in 0..child_count {
        let child = match impl_node.child(i as u32) {
            Some(c) => c,
            None => continue,
        };

        if child.kind() != "named_type" {
            continue;
        }

        let iface_name = match child.utf8_text(source).ok() {
            Some(t) => t.trim().to_string(),
            None => continue,
        };

        if iface_name.is_empty() {
            continue;
        }

        edges.push(Edge {
            from: type_node_id.clone(),
            to: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: iface_name,
                kind: NodeKind::Other("graphql_type".to_string()),
            },
            kind: EdgeKind::Implements,
            source: ExtractionSource::Schema,
            confidence: Confidence::Detected,
        });
    }
}

/// Emit DependsOn edges from an operation to its top-level selected fields.
fn extract_selection_type_refs(
    op_node: tree_sitter::Node<'_>,
    path: &Path,
    source: &[u8],
    op_node_id: &NodeId,
    edges: &mut Vec<Edge>,
) {
    let selection_set = match find_child_by_kind(op_node, "selection_set") {
        Some(s) => s,
        None => return,
    };

    let child_count = selection_set.child_count();
    for i in 0..child_count {
        let child = match selection_set.child(i as u32) {
            Some(c) => c,
            None => continue,
        };

        if child.kind() != "field" {
            continue;
        }

        let field_name = match find_child_text(child, "name", source) {
            Some(n) => n,
            None => continue,
        };

        edges.push(Edge {
            from: op_node_id.clone(),
            to: NodeId {
                root: String::new(),
                file: path.to_path_buf(),
                name: field_name,
                kind: NodeKind::Field,
            },
            kind: EdgeKind::DependsOn,
            source: ExtractionSource::Schema,
            confidence: Confidence::Detected,
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the first child node with the given kind.
fn find_child_by_kind<'tree>(
    node: tree_sitter::Node<'tree>,
    kind: &str,
) -> Option<tree_sitter::Node<'tree>> {
    let child_count = node.child_count();
    for i in 0..child_count {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == kind {
                return Some(child);
            }
        }
    }
    None
}

/// Get the UTF-8 text of the first child with the given kind.
fn find_child_text(node: tree_sitter::Node<'_>, kind: &str, source: &[u8]) -> Option<String> {
    find_child_by_kind(node, kind)
        .and_then(|c| c.utf8_text(source).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Alias for find_child_text (for non-named node kinds like operation_type).
fn find_child_kind_text(
    node: tree_sitter::Node<'_>,
    kind: &str,
    source: &[u8],
) -> Option<String> {
    find_child_text(node, kind, source)
}

/// Extract the type_condition's named type from a fragment definition.
fn find_child_type_condition(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let tc = find_child_by_kind(node, "type_condition")?;
    let named = find_child_by_kind(tc, "named_type")?;
    named.utf8_text(source).ok().map(|s| s.trim().to_string())
}

/// Extract the type text from a field_definition or input_value_definition.
/// Checks for non_null_type, list_type, named_type, or "type" children.
fn extract_type_text(node: tree_sitter::Node<'_>, source: &[u8]) -> String {
    for kind in &["non_null_type", "list_type", "named_type", "type"] {
        if let Some(t) = find_child_by_kind(node, kind) {
            if let Ok(text) = t.utf8_text(source) {
                return text.trim().to_string();
            }
        }
    }
    String::new()
}

/// Strip GraphQL type modifiers (!, []) to get the bare type name.
///
/// Handles arbitrary nesting: `[[User!]!]!` → `User`.
fn strip_type_modifiers(type_str: &str) -> &str {
    let mut s = type_str.trim();
    loop {
        let prev = s;
        s = s.trim_end_matches('!').trim();
        if s.starts_with('[') && s.ends_with(']') {
            s = s[1..s.len() - 1].trim();
        }
        if s == prev {
            break;
        }
    }
    s
}

/// Returns true if the name is a built-in GraphQL scalar type.
fn is_builtin_scalar(name: &str) -> bool {
    matches!(name, "String" | "Int" | "Float" | "Boolean" | "ID")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_object_types() {
        let extractor = GraphQlExtractor::new();
        let content = r#"
type Query {
  user(id: ID!): User
  users: [User!]!
}

type User {
  id: ID!
  name: String!
  email: String
}
"#;
        let result = extractor
            .extract(Path::new("schema.graphql"), content)
            .unwrap();

        let types: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("graphql_type".to_string()))
            .collect();

        assert_eq!(types.len(), 2, "Should find 2 object types");
        let names: Vec<&str> = types.iter().map(|n| n.id.name.as_str()).collect();
        assert!(names.contains(&"Query"));
        assert!(names.contains(&"User"));
    }

    #[test]
    fn test_extract_fields_and_has_field_edges() {
        let extractor = GraphQlExtractor::new();
        let content = r#"
type User {
  id: ID!
  name: String!
  email: String
}
"#;
        let result = extractor
            .extract(Path::new("schema.graphql"), content)
            .unwrap();

        let fields: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Field)
            .collect();

        assert_eq!(fields.len(), 3, "Should find 3 fields");

        let field_names: Vec<&str> = fields.iter().map(|n| n.id.name.as_str()).collect();
        assert!(field_names.contains(&"User.id"));
        assert!(field_names.contains(&"User.name"));
        assert!(field_names.contains(&"User.email"));

        let has_field_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::HasField)
            .collect();
        assert_eq!(has_field_edges.len(), 3);
    }

    #[test]
    fn test_extract_interface_and_implements() {
        let extractor = GraphQlExtractor::new();
        let content = r#"
interface Node {
  id: ID!
}

type User implements Node {
  id: ID!
  name: String!
}
"#;
        let result = extractor
            .extract(Path::new("schema.graphql"), content)
            .unwrap();

        let types: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("graphql_type".to_string()))
            .collect();
        assert_eq!(types.len(), 2);

        let implements_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Implements)
            .collect();
        assert_eq!(implements_edges.len(), 1, "User implements Node");
        assert_eq!(implements_edges[0].from.name, "User");
        assert_eq!(implements_edges[0].to.name, "Node");
    }

    #[test]
    fn test_extract_enum_type() {
        let extractor = GraphQlExtractor::new();
        let content = r#"
enum Status {
  ACTIVE
  INACTIVE
  PENDING
}
"#;
        let result = extractor
            .extract(Path::new("schema.graphql"), content)
            .unwrap();

        let enums: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("graphql_type".to_string()))
            .collect();
        assert_eq!(enums.len(), 1);
        assert_eq!(enums[0].id.name, "Status");
        assert_eq!(
            enums[0].metadata.get("graphql_kind").map(|s| s.as_str()),
            Some("enum")
        );

        let variants: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::EnumVariant)
            .collect();
        assert_eq!(variants.len(), 3);
        let vnames: Vec<&str> = variants.iter().map(|n| n.id.name.as_str()).collect();
        assert!(vnames.contains(&"Status.ACTIVE"));
        assert!(vnames.contains(&"Status.INACTIVE"));
        assert!(vnames.contains(&"Status.PENDING"));
    }

    #[test]
    fn test_extract_mutation_and_subscription_types() {
        let extractor = GraphQlExtractor::new();
        let content = r#"
type Mutation {
  createUser(name: String!): User
  deleteUser(id: ID!): Boolean
}

type Subscription {
  userCreated: User
}

type User {
  id: ID!
  name: String!
}
"#;
        let result = extractor
            .extract(Path::new("schema.graphql"), content)
            .unwrap();

        let types: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("graphql_type".to_string()))
            .collect();
        let type_names: Vec<&str> = types.iter().map(|n| n.id.name.as_str()).collect();
        assert!(type_names.contains(&"Mutation"));
        assert!(type_names.contains(&"Subscription"));
        assert!(type_names.contains(&"User"));
    }

    #[test]
    fn test_extract_operation_definitions() {
        let extractor = GraphQlExtractor::new();
        let content = r#"
query GetUser($id: ID!) {
  user(id: $id) {
    id
    name
  }
}

mutation CreateUser($name: String!) {
  createUser(name: $name) {
    id
  }
}
"#;
        let result = extractor
            .extract(Path::new("operations.graphql"), content)
            .unwrap();

        let ops: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Function)
            .collect();
        assert_eq!(ops.len(), 2, "Should find 2 operations");

        let op_names: Vec<&str> = ops.iter().map(|n| n.id.name.as_str()).collect();
        assert!(op_names.contains(&"GetUser"));
        assert!(op_names.contains(&"CreateUser"));

        let get_user = ops.iter().find(|n| n.id.name == "GetUser").unwrap();
        assert_eq!(
            get_user.metadata.get("operation_type").map(|s| s.as_str()),
            Some("query")
        );

        let create_user = ops.iter().find(|n| n.id.name == "CreateUser").unwrap();
        assert_eq!(
            create_user.metadata.get("operation_type").map(|s| s.as_str()),
            Some("mutation")
        );
    }

    #[test]
    fn test_extract_scalar_and_input_types() {
        let extractor = GraphQlExtractor::new();
        let content = r#"
scalar DateTime
scalar UUID

input CreateUserInput {
  name: String!
  email: String!
}
"#;
        let result = extractor
            .extract(Path::new("schema.graphql"), content)
            .unwrap();

        let types: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("graphql_type".to_string()))
            .collect();

        let type_names: Vec<&str> = types.iter().map(|n| n.id.name.as_str()).collect();
        assert!(type_names.contains(&"DateTime"));
        assert!(type_names.contains(&"UUID"));
        assert!(type_names.contains(&"CreateUserInput"));

        let scalar_dt = types.iter().find(|n| n.id.name == "DateTime").unwrap();
        assert_eq!(
            scalar_dt.metadata.get("graphql_kind").map(|s| s.as_str()),
            Some("scalar")
        );

        let input = types.iter().find(|n| n.id.name == "CreateUserInput").unwrap();
        assert_eq!(
            input.metadata.get("graphql_kind").map(|s| s.as_str()),
            Some("input")
        );

        let fields: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Field && n.id.name.starts_with("CreateUserInput."))
            .collect();
        assert_eq!(fields.len(), 2);
    }

    #[test]
    fn test_extractor_name_and_extensions() {
        let extractor = GraphQlExtractor::new();
        assert_eq!(extractor.name(), "graphql");
        assert_eq!(extractor.extensions(), &["graphql", "gql"]);
    }

    #[test]
    fn test_language_is_graphql() {
        let extractor = GraphQlExtractor::new();
        let content = "type Foo { id: ID! }\n";
        let result = extractor
            .extract(Path::new("test.graphql"), content)
            .unwrap();
        assert!(result.nodes.iter().all(|n| n.language == "graphql"));
    }

    #[test]
    fn test_depends_on_edges_for_custom_types() {
        let extractor = GraphQlExtractor::new();
        let content = r#"
type Post {
  author: User!
  tags: [Tag!]!
  title: String!
}

type User {
  id: ID!
}

type Tag {
  name: String!
}
"#;
        let result = extractor
            .extract(Path::new("schema.graphql"), content)
            .unwrap();

        let depends: Vec<_> = result
            .edges
            .iter()
            .filter(|e| {
                e.kind == EdgeKind::DependsOn
                    && e.to.kind == NodeKind::Other("graphql_type".to_string())
            })
            .collect();

        let dep_targets: Vec<&str> = depends.iter().map(|e| e.to.name.as_str()).collect();
        assert!(dep_targets.contains(&"User"), "Post.author -> User");
        assert!(dep_targets.contains(&"Tag"), "Post.tags -> Tag");
    }

    #[test]
    fn test_strip_type_modifiers() {
        assert_eq!(strip_type_modifiers("String!"), "String");
        assert_eq!(strip_type_modifiers("[User!]!"), "User");
        assert_eq!(strip_type_modifiers("[User]"), "User");
        assert_eq!(strip_type_modifiers("User"), "User");
        // Nested list types (rare but valid GraphQL)
        assert_eq!(strip_type_modifiers("[[User!]!]!"), "User");
        assert_eq!(strip_type_modifiers("[[String]]"), "String");
    }

    #[test]
    fn test_fragment_extraction() {
        let extractor = GraphQlExtractor::new();
        let content = r#"
fragment UserFields on User {
  id
  name
  email
}
"#;
        let result = extractor
            .extract(Path::new("fragments.graphql"), content)
            .unwrap();

        let fragments: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("graphql_fragment".to_string()))
            .collect();

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].id.name, "UserFields");
        assert_eq!(
            fragments[0].metadata.get("on_type").map(|s| s.as_str()),
            Some("User")
        );
    }
}
