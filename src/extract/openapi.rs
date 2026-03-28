//! OpenAPI / JSON Schema extractor.
//!
//! Parses `.yaml` and `.json` files that contain OpenAPI specs to extract:
//! - Endpoint paths -> `NodeKind::ApiEndpoint` nodes
//! - Schema definitions -> nodes with `EdgeKind::HasField` edges
//! - Request/response references -> edges between endpoints and schemas

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde_yaml::Value;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

/// OpenAPI / JSON Schema extractor.
pub struct OpenApiExtractor;

impl Default for OpenApiExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenApiExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for OpenApiExtractor {
    fn extensions(&self) -> &[&str] {
        &["yaml", "yml", "json"]
    }

    fn name(&self) -> &str {
        "openapi"
    }

    fn can_handle(&self, _path: &Path, content: &str) -> bool {
        // Only handle files that look like OpenAPI or JSON Schema
        let first_lines: String = content.lines().take(50).collect::<Vec<_>>().join("\n");
        first_lines.contains("\"openapi\":")
            || first_lines.contains("openapi:")
            || first_lines.contains("\"swagger\":")
            || first_lines.contains("swagger:")
            || first_lines.contains("\"$schema\":")
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let doc: Value = if path.extension().map(|e| e == "json").unwrap_or(false) {
            let json_val: serde_json::Value = serde_json::from_str(content)?;
            // Convert JSON to YAML Value for uniform handling
            serde_yaml::from_str(&serde_json::to_string(&json_val)?)?
        } else {
            serde_yaml::from_str(content)?
        };

        let mut nodes = Vec::new();
        let mut edges = Vec::new();

        // Extract paths (endpoints)
        if let Some(paths) = doc.get("paths") {
            extract_paths(paths, path, &mut nodes, &mut edges);
        }

        // Extract component schemas (OpenAPI 3.x)
        if let Some(components) = doc.get("components")
            && let Some(schemas) = components.get("schemas")
        {
            extract_schemas(schemas, path, &mut nodes, &mut edges);
        }

        // Extract definitions (Swagger 2.x / JSON Schema)
        if let Some(definitions) = doc.get("definitions") {
            extract_schemas(definitions, path, &mut nodes, &mut edges);
        }

        Ok(ExtractionResult { nodes, edges })
    }
}

/// Extract API endpoint paths.
fn extract_paths(paths: &Value, file_path: &Path, nodes: &mut Vec<Node>, edges: &mut Vec<Edge>) {
    let mapping = match paths.as_mapping() {
        Some(m) => m,
        None => return,
    };

    for (path_key, methods) in mapping {
        let endpoint_path = match path_key.as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        let methods_map = match methods.as_mapping() {
            Some(m) => m,
            None => continue,
        };

        for (method_key, operation) in methods_map {
            let method = match method_key.as_str() {
                Some(s) => s.to_uppercase(),
                None => continue,
            };

            // Skip non-HTTP method keys like "parameters", "summary"
            if !["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"]
                .contains(&method.as_str())
            {
                continue;
            }

            let endpoint_name = format!("{} {}", method, endpoint_path);

            let endpoint_node_id = NodeId {
                root: String::new(),
                file: file_path.to_path_buf(),
                name: endpoint_name.clone(),
                kind: NodeKind::ApiEndpoint,
            };

            let mut metadata = BTreeMap::new();
            metadata.insert("method".to_string(), method.clone());
            metadata.insert("path".to_string(), endpoint_path.clone());

            if let Some(op_id) = operation.get("operationId").and_then(|v| v.as_str()) {
                metadata.insert("operation_id".to_string(), op_id.to_string());
            }
            if let Some(summary) = operation.get("summary").and_then(|v| v.as_str()) {
                metadata.insert("summary".to_string(), summary.to_string());
            }

            nodes.push(Node {
                id: endpoint_node_id.clone(),
                language: "openapi".to_string(),
                line_start: 0,
                line_end: 0,
                signature: endpoint_name,
                body: serde_yaml::to_string(operation).unwrap_or_default(),
                metadata,
                source: ExtractionSource::Schema,
            });

            // Extract schema references from request body and responses
            extract_refs_from_operation(operation, file_path, &endpoint_node_id, edges);
        }
    }
}

/// Extract `$ref` references from an operation's request/response bodies.
fn extract_refs_from_operation(
    operation: &Value,
    file_path: &Path,
    endpoint_id: &NodeId,
    edges: &mut Vec<Edge>,
) {
    // Collect all $ref values from the operation
    let mut refs = Vec::new();
    collect_refs(operation, &mut refs);

    for ref_path in refs {
        // Extract schema name from $ref like "#/components/schemas/User"
        let schema_name = ref_path.rsplit('/').next().unwrap_or(&ref_path).to_string();

        if !schema_name.is_empty() {
            edges.push(Edge {
                from: endpoint_id.clone(),
                to: NodeId {
                    root: String::new(),
                    file: file_path.to_path_buf(),
                    name: schema_name,
                    kind: NodeKind::Struct,
                },
                kind: EdgeKind::DependsOn,
                source: ExtractionSource::Schema,
                confidence: Confidence::Detected,
            });
        }
    }
}

/// Recursively collect all `$ref` string values from a YAML value.
fn collect_refs(value: &Value, refs: &mut Vec<String>) {
    match value {
        Value::Mapping(map) => {
            for (k, v) in map {
                if k.as_str() == Some("$ref") {
                    if let Some(r) = v.as_str() {
                        refs.push(r.to_string());
                    }
                } else {
                    collect_refs(v, refs);
                }
            }
        }
        Value::Sequence(seq) => {
            for item in seq {
                collect_refs(item, refs);
            }
        }
        _ => {}
    }
}

/// Extract schema definitions (from components/schemas or definitions).
fn extract_schemas(
    schemas: &Value,
    file_path: &Path,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
) {
    let mapping = match schemas.as_mapping() {
        Some(m) => m,
        None => return,
    };

    for (name_key, schema_def) in mapping {
        let schema_name = match name_key.as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        let schema_node_id = NodeId {
            root: String::new(),
            file: file_path.to_path_buf(),
            name: schema_name.clone(),
            kind: NodeKind::Struct,
        };

        let mut metadata = BTreeMap::new();
        if let Some(schema_type) = schema_def.get("type").and_then(|v| v.as_str()) {
            metadata.insert("schema_type".to_string(), schema_type.to_string());
        }

        nodes.push(Node {
            id: schema_node_id.clone(),
            language: "openapi".to_string(),
            line_start: 0,
            line_end: 0,
            signature: format!("schema {}", schema_name),
            body: serde_yaml::to_string(schema_def).unwrap_or_default(),
            metadata,
            source: ExtractionSource::Schema,
        });

        // Extract enum values from the schema definition
        if let Some(enum_values) = schema_def.get("enum")
            && let Some(enum_seq) = enum_values.as_sequence()
        {
            for enum_val in enum_seq {
                let val_str = match enum_val {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => continue,
                };
                if !val_str.is_empty() {
                    let mut const_metadata = BTreeMap::new();
                    const_metadata.insert("value".to_string(), val_str.clone());
                    const_metadata.insert("synthetic".to_string(), "true".to_string());
                    nodes.push(Node {
                        id: NodeId {
                            root: String::new(),
                            file: file_path.to_path_buf(),
                            name: format!("{}.{}", schema_name, val_str),
                            kind: NodeKind::Const,
                        },
                        language: "openapi".to_string(),
                        line_start: 0,
                        line_end: 0,
                        signature: format!("{} enum: {}", schema_name, val_str),
                        body: val_str,
                        metadata: const_metadata,
                        source: ExtractionSource::Schema,
                    });
                }
            }
        }

        // Extract properties as fields
        if let Some(properties) = schema_def.get("properties")
            && let Some(props_map) = properties.as_mapping()
        {
            for (prop_key, prop_def) in props_map {
                let prop_name = match prop_key.as_str() {
                    Some(s) => s.to_string(),
                    None => continue,
                };

                let prop_type = prop_def
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                let field_node_id = NodeId {
                    root: String::new(),
                    file: file_path.to_path_buf(),
                    name: format!("{}.{}", schema_name, prop_name),
                    kind: NodeKind::Other("schema_field".to_string()),
                };

                let mut field_metadata = BTreeMap::new();
                field_metadata.insert("field_type".to_string(), prop_type);
                field_metadata.insert("schema".to_string(), schema_name.clone());

                nodes.push(Node {
                    id: field_node_id.clone(),
                    language: "openapi".to_string(),
                    line_start: 0,
                    line_end: 0,
                    signature: format!("{}.{}", schema_name, prop_name),
                    body: serde_yaml::to_string(prop_def).unwrap_or_default(),
                    metadata: field_metadata,
                    source: ExtractionSource::Schema,
                });

                edges.push(Edge {
                    from: schema_node_id.clone(),
                    to: field_node_id,
                    kind: EdgeKind::HasField,
                    source: ExtractionSource::Schema,
                    confidence: Confidence::Detected,
                });

                // If property has a $ref, create a DependsOn edge
                if let Some(ref_val) = prop_def.get("$ref").and_then(|v| v.as_str()) {
                    let ref_schema = ref_val.rsplit('/').next().unwrap_or(ref_val).to_string();
                    edges.push(Edge {
                        from: schema_node_id.clone(),
                        to: NodeId {
                            root: String::new(),
                            file: file_path.to_path_buf(),
                            name: ref_schema,
                            kind: NodeKind::Struct,
                        },
                        kind: EdgeKind::DependsOn,
                        source: ExtractionSource::Schema,
                        confidence: Confidence::Detected,
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_can_handle_openapi() {
        let extractor = OpenApiExtractor::new();
        assert!(extractor.can_handle(
            Path::new("api.yaml"),
            "openapi: 3.0.0\ninfo:\n  title: Test\n"
        ));
        assert!(extractor.can_handle(Path::new("api.json"), "{\"openapi\": \"3.0.0\"}"));
        assert!(!extractor.can_handle(Path::new("config.yaml"), "database:\n  host: localhost\n"));
    }

    #[test]
    fn test_extract_openapi_paths() {
        let extractor = OpenApiExtractor::new();
        let content = r#"
openapi: 3.0.0
info:
  title: Test API
  version: 1.0.0
paths:
  /users:
    get:
      summary: List users
      operationId: listUsers
      responses:
        '200':
          description: OK
    post:
      summary: Create user
      operationId: createUser
      requestBody:
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/CreateUserRequest'
      responses:
        '201':
          description: Created
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/User'
  /users/{id}:
    get:
      summary: Get user
      operationId: getUser
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/User'
components:
  schemas:
    User:
      type: object
      properties:
        id:
          type: integer
        name:
          type: string
        email:
          type: string
    CreateUserRequest:
      type: object
      properties:
        name:
          type: string
        email:
          type: string
"#;
        let result = extractor.extract(Path::new("api.yaml"), content).unwrap();

        // Endpoint nodes
        let endpoints: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert_eq!(endpoints.len(), 3, "Should find 3 endpoints");

        let endpoint_names: Vec<&str> = endpoints.iter().map(|n| n.id.name.as_str()).collect();
        assert!(endpoint_names.contains(&"GET /users"));
        assert!(endpoint_names.contains(&"POST /users"));
        assert!(endpoint_names.contains(&"GET /users/{id}"));

        // Schema nodes
        let schemas: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Struct)
            .collect();
        assert_eq!(schemas.len(), 2, "Should find 2 schemas");

        // Schema fields
        let fields: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Other("schema_field".to_string()))
            .collect();
        assert_eq!(fields.len(), 5, "Should find 5 schema fields (3 + 2)");

        // HasField edges
        let has_field_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::HasField)
            .collect();
        assert_eq!(has_field_edges.len(), 5);

        // DependsOn edges from endpoints to schemas (via $ref)
        let dep_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::DependsOn)
            .collect();
        assert!(
            dep_edges.len() >= 2,
            "Should have DependsOn edges from $ref"
        );
    }

    #[test]
    fn test_extract_swagger_2() {
        let extractor = OpenApiExtractor::new();
        let content = r#"
swagger: "2.0"
info:
  title: Legacy API
  version: 1.0.0
paths:
  /items:
    get:
      summary: List items
definitions:
  Item:
    type: object
    properties:
      id:
        type: integer
      name:
        type: string
"#;
        let result = extractor.extract(Path::new("api.yaml"), content).unwrap();

        let endpoints: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert_eq!(endpoints.len(), 1);

        let schemas: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::Struct)
            .collect();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].id.name, "Item");
    }

    #[test]
    fn test_extract_openapi_json() {
        let extractor = OpenApiExtractor::new();
        let content = r#"{
  "openapi": "3.0.0",
  "info": { "title": "JSON API", "version": "1.0.0" },
  "paths": {
    "/health": {
      "get": {
        "summary": "Health check"
      }
    }
  }
}"#;
        let result = extractor.extract(Path::new("api.json"), content).unwrap();

        let endpoints: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].id.name, "GET /health");
    }

    #[test]
    fn test_openapi_extractor_extensions() {
        let extractor = OpenApiExtractor::new();
        assert_eq!(extractor.extensions(), &["yaml", "yml", "json"]);
        assert_eq!(extractor.name(), "openapi");
    }

    #[test]
    fn test_openapi_language_tag() {
        let extractor = OpenApiExtractor::new();
        let content = "openapi: 3.0.0\ninfo:\n  title: T\n  version: 1\npaths:\n  /x:\n    get:\n      summary: x\n";
        let result = extractor.extract(Path::new("api.yaml"), content).unwrap();
        let endpoints: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.id.kind == NodeKind::ApiEndpoint)
            .collect();
        assert!(!endpoints.is_empty());
        assert_eq!(endpoints[0].language, "openapi");
    }
}
