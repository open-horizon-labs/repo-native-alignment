//! ASP.NET Core minimal API endpoint extraction.
//!
//! Scans C# source files for `app.MapGet("/path", ...)`, `app.MapPost(...)`, etc.
//! and emits `ApiEndpoint` nodes with HTTP method and path metadata.
//!
//! **ADR compliance:** This runs as a framework-gated pass inside `EnrichmentFinalizer`,
//! triggered only when `detected_frameworks` contains `"aspnet"`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::ExtractionResult;

/// HTTP method mapping patterns in C# minimal APIs.
const MAP_METHODS: &[(&str, &str)] = &[
    ("MapGet", "GET"),
    ("MapPost", "POST"),
    ("MapPut", "PUT"),
    ("MapDelete", "DELETE"),
    ("MapPatch", "PATCH"),
];

/// Extract ASP.NET minimal API endpoints from C# nodes.
///
/// Reads the source files of C# function nodes, scanning for `app.MapXxx("/path", ...)`
/// patterns. Each match produces an `ApiEndpoint` node with `method` and `path` metadata.
pub fn aspnet_endpoint_pass(
    root_pairs: &[(String, PathBuf)],
    nodes: &[Node],
) -> ExtractionResult {
    let mut result = ExtractionResult::default();

    // Collect C# files that likely contain endpoint mappings.
    // Look at function nodes in files whose names suggest endpoint registration.
    let cs_files: Vec<(&str, &PathBuf)> = nodes
        .iter()
        .filter(|n| n.language == "csharp")
        .map(|n| (n.id.root.as_str(), &n.id.file))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    for (root_slug, file_path) in &cs_files {
        // Resolve to absolute path via root_pairs.
        let abs_path = root_pairs
            .iter()
            .find(|(slug, _)| slug == root_slug)
            .map(|(_, root)| root.join(file_path))
            .unwrap_or_else(|| file_path.to_path_buf());

        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Quick check: does this file contain any Map method?
        if !MAP_METHODS.iter().any(|(pat, _)| content.contains(pat)) {
            continue;
        }

        extract_endpoints_from_source(
            &content,
            file_path,
            root_slug,
            &mut result,
        );
    }

    result
}

/// Parse a C# source file for `MapGet("/path", ...)` etc. patterns.
fn extract_endpoints_from_source(
    content: &str,
    file_path: &PathBuf,
    root_slug: &str,
    result: &mut ExtractionResult,
) {
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        for &(map_method, http_method) in MAP_METHODS {
            // Match patterns like: app.MapGet("/path", ...) or .MapGet("/path", ...)
            let Some(map_pos) = trimmed.find(map_method) else {
                continue;
            };
            // Expect '(' after the method name
            let after_method = &trimmed[map_pos + map_method.len()..];
            if !after_method.starts_with('(') {
                continue;
            }
            let after_paren = after_method[1..].trim_start();
            // Extract the route string (first argument, must be a string literal)
            if !after_paren.starts_with('"') {
                continue;
            }
            let route_end = after_paren[1..].find('"');
            let Some(end) = route_end else { continue };
            let route = &after_paren[1..1 + end];

            let endpoint_name = format!("{} {}", http_method, route);
            let node_id = NodeId {
                root: root_slug.to_string(),
                file: file_path.clone(),
                name: endpoint_name.clone(),
                kind: NodeKind::ApiEndpoint,
            };

            let mut metadata = BTreeMap::new();
            metadata.insert("method".to_string(), http_method.to_string());
            metadata.insert("path".to_string(), route.to_string());
            metadata.insert("framework".to_string(), "aspnet".to_string());

            result.nodes.push(Node {
                id: node_id,
                language: "csharp".to_string(),
                line_start: i,
                line_end: i,
                signature: format!("{}.{}(\"{}\", ...)", "app", map_method, route),
                body: String::new(),
                metadata,
                source: ExtractionSource::TreeSitter,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_minimal_api_endpoints() {
        let content = r#"
var app = builder.Build();
app.MapGet("/reviews/{id:guid}", async (Guid id, HttpContext ctx) => { });
app.MapPost("/intakes", async (HttpContext ctx) => { });
app.MapPut("/templates/{id}", async (string id) => { });
app.MapDelete("/templates/{id}", async (string id) => { });
"#;
        let mut result = ExtractionResult::default();
        let path = PathBuf::from("src/Endpoints.cs");
        extract_endpoints_from_source(content, &path, "test", &mut result);

        assert_eq!(result.nodes.len(), 4);
        assert_eq!(result.nodes[0].id.name, "GET /reviews/{id:guid}");
        assert_eq!(result.nodes[1].id.name, "POST /intakes");
        assert_eq!(result.nodes[2].id.name, "PUT /templates/{id}");
        assert_eq!(result.nodes[3].id.name, "DELETE /templates/{id}");

        // Check metadata
        assert_eq!(result.nodes[0].metadata["method"], "GET");
        assert_eq!(result.nodes[0].metadata["path"], "/reviews/{id:guid}");
        assert_eq!(result.nodes[0].metadata["framework"], "aspnet");
    }

    #[test]
    fn test_ignores_non_map_lines() {
        let content = r#"
// MapGet is mentioned in a comment
var result = MapGetSomething();
app.UseRouting();
"#;
        let mut result = ExtractionResult::default();
        let path = PathBuf::from("src/Program.cs");
        extract_endpoints_from_source(content, &path, "test", &mut result);
        assert!(result.nodes.is_empty());
    }

    #[test]
    fn test_map_with_variable_route_skipped() {
        let content = r#"
app.MapGet(routeVar, handler);
"#;
        let mut result = ExtractionResult::default();
        let path = PathBuf::from("src/Program.cs");
        extract_endpoints_from_source(content, &path, "test", &mut result);
        // Variable routes are skipped (not string literal)
        assert!(result.nodes.is_empty());
    }
}
