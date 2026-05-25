//! ASP.NET Core minimal API endpoint extraction.
//!
//! Scans C# source files for `app.MapGet("/path", ...)`, `app.MapPost(...)`, etc.
//! and emits `ApiEndpoint` nodes with HTTP method and path metadata.
//!
//! **ADR compliance:** This runs as a framework-gated pass inside `EnrichmentFinalizer`,
//! triggered only when `detected_frameworks` contains `"aspnet"`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::ExtractionResult;

/// HTTP method mapping patterns in C# minimal APIs.
const MAP_METHODS: &[(&str, &str)] = &[
    ("MapGet", "GET"),
    ("MapPost", "POST"),
    ("MapPut", "PUT"),
    ("MapDelete", "DELETE"),
    ("MapPatch", "PATCH"),
    ("MapHead", "HEAD"),
    ("MapOptions", "OPTIONS"),
];

/// HTTP method attribute mapping patterns in ASP.NET MVC/Web API controllers.
const HTTP_ATTRIBUTE_METHODS: &[(&str, &str)] = &[
    ("HttpGet", "GET"),
    ("HttpPost", "POST"),
    ("HttpPut", "PUT"),
    ("HttpDelete", "DELETE"),
    ("HttpPatch", "PATCH"),
    ("HttpHead", "HEAD"),
    ("HttpOptions", "OPTIONS"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Attribute {
    name: String,
    route: Option<String>,
    line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ControllerContext {
    name: String,
    route_prefix: Option<String>,
}

/// Extract ASP.NET minimal API and controller endpoints from C# nodes.
///
/// Reads C# source files and emits authoritative server-route `ApiEndpoint` nodes for:
/// - minimal API registrations such as `app.MapGet("/path", ...)`;
/// - controller actions decorated with `[HttpGet]`, `[HttpPost]`, etc., optionally
///   combined with controller-level `[Route("...")]` prefixes.
pub fn aspnet_endpoint_pass(root_pairs: &[(String, PathBuf)], nodes: &[Node]) -> ExtractionResult {
    let mut result = ExtractionResult::default();

    let cs_files: Vec<(&str, &PathBuf)> = nodes
        .iter()
        .filter(|n| n.language == "csharp")
        .map(|n| (n.id.root.as_str(), &n.id.file))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    for (root_slug, file_path) in &cs_files {
        let abs_path = root_pairs
            .iter()
            .find(|(slug, _)| slug == root_slug)
            .map(|(_, root)| root.join(file_path))
            .unwrap_or_else(|| file_path.to_path_buf());

        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if !contains_aspnet_endpoint_marker(&content) {
            continue;
        }

        extract_endpoints_from_source(&content, file_path, root_slug, &mut result);
    }

    result
}

fn contains_aspnet_endpoint_marker(content: &str) -> bool {
    MAP_METHODS.iter().any(|(pat, _)| content.contains(pat))
        || HTTP_ATTRIBUTE_METHODS
            .iter()
            .any(|(attr, _)| content.contains(attr))
}

/// Parse a C# source file for ASP.NET endpoint declarations.
fn extract_endpoints_from_source(
    content: &str,
    file_path: &Path,
    root_slug: &str,
    result: &mut ExtractionResult,
) {
    let mut pending_attrs: Vec<Attribute> = Vec::new();
    let mut controller: Option<ControllerContext> = None;

    for (i, line) in content.lines().enumerate() {
        let line_number = i + 1;
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            continue;
        }

        let minimal_before = result.nodes.len();
        extract_minimal_api_endpoints(trimmed, line_number, file_path, root_slug, result);
        if result.nodes.len() != minimal_before {
            pending_attrs.clear();
            continue;
        }

        if trimmed.starts_with('[') {
            let attrs = parse_attributes(trimmed, line_number);
            if !attrs.is_empty() {
                pending_attrs.extend(attrs);
                continue;
            }
        }

        if let Some(class_name) = extract_class_name(trimmed) {
            let route_prefix = pending_attrs
                .iter()
                .find(|attr| attr.name == "Route")
                .and_then(|attr| attr.route.clone());
            let is_controller = class_name.ends_with("Controller")
                || trimmed.contains("ControllerBase")
                || pending_attrs
                    .iter()
                    .any(|attr| attr.name == "ApiController");
            if is_controller {
                controller = Some(ControllerContext {
                    name: class_name,
                    route_prefix,
                });
            }
            pending_attrs.clear();
            continue;
        }

        if is_method_declaration(trimmed) {
            if let Some((http_method, route_fragment, attribute_line)) =
                route_from_method_attrs(&pending_attrs)
            {
                let route = combine_routes(controller.as_ref(), route_fragment.as_deref());
                push_endpoint(
                    result,
                    root_slug,
                    file_path,
                    attribute_line,
                    http_method,
                    &route,
                    format!("[aspnet_controller] {} {}", http_method, route),
                );
            }
            pending_attrs.clear();
            continue;
        }

        if !trimmed.is_empty() {
            pending_attrs.clear();
        }
    }
}

fn extract_minimal_api_endpoints(
    trimmed: &str,
    line_number: usize,
    file_path: &Path,
    root_slug: &str,
    result: &mut ExtractionResult,
) {
    for &(map_method, http_method) in MAP_METHODS {
        let Some(map_pos) = trimmed.find(map_method) else {
            continue;
        };
        let after_method = trimmed[map_pos + map_method.len()..].trim_start();
        if !after_method.starts_with('(') {
            continue;
        }
        let after_paren = after_method[1..].trim_start();
        if !after_paren.starts_with('"') {
            continue;
        }
        let route_end = after_paren[1..].find('"');
        let Some(end) = route_end else { continue };
        let route = &after_paren[1..1 + end];

        push_endpoint(
            result,
            root_slug,
            file_path,
            line_number,
            http_method,
            route,
            format!("app.{}(\"{}\", ...)", map_method, route),
        );
    }
}

fn push_endpoint(
    result: &mut ExtractionResult,
    root_slug: &str,
    file_path: &Path,
    line_number: usize,
    http_method: &str,
    route: &str,
    signature: String,
) {
    let endpoint_name = format!("{} {}", http_method, route);
    let node_id = NodeId {
        root: root_slug.to_string(),
        file: file_path.to_path_buf(),
        name: endpoint_name,
        kind: NodeKind::ApiEndpoint,
    };

    let mut metadata = BTreeMap::new();
    metadata.insert("method".to_string(), http_method.to_string());
    metadata.insert("path".to_string(), route.to_string());
    metadata.insert("http_method".to_string(), http_method.to_string());
    metadata.insert("http_path".to_string(), route.to_string());
    metadata.insert("framework".to_string(), "aspnet".to_string());
    metadata.insert("endpoint_source".to_string(), "server_route".to_string());
    metadata.insert("synthetic".to_string(), "false".to_string());

    result.nodes.push(Node {
        id: node_id,
        language: "csharp".to_string(),
        line_start: line_number,
        line_end: line_number,
        signature,
        body: String::new(),
        metadata,
        source: ExtractionSource::TreeSitter,
    });
}

fn parse_attributes(trimmed: &str, line: usize) -> Vec<Attribute> {
    let mut attrs = Vec::new();
    let mut rest = trimmed;

    while let Some(start) = rest.find('[') {
        let after_start = &rest[start + 1..];
        let Some(end) = find_attribute_end(after_start) else {
            break;
        };
        let content = after_start[..end].trim();
        if let Some(attr) = parse_attribute(content, line) {
            attrs.push(attr);
        }
        rest = &after_start[end + 1..];
    }

    attrs
}

fn find_attribute_end(input: &str) -> Option<usize> {
    let mut in_string = false;
    let mut escaped = false;

    for (idx, ch) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            ']' if !in_string => return Some(idx),
            _ => {}
        }
    }

    None
}

fn parse_attribute(content: &str, line: usize) -> Option<Attribute> {
    let name_end = content
        .find(|c: char| c == '(' || c == ',' || c.is_whitespace())
        .unwrap_or(content.len());
    let mut name = content[..name_end].trim().to_string();
    if let Some(stripped) = name.strip_suffix("Attribute") {
        name = stripped.to_string();
    }
    if name.is_empty() {
        return None;
    }
    let route = first_string_literal(content);
    Some(Attribute { name, route, line })
}

fn first_string_literal(content: &str) -> Option<String> {
    let start = content.find('"')?;
    let rest = &content[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn extract_class_name(trimmed: &str) -> Option<String> {
    let class_pos = trimmed.find("class ")?;
    let after_class = &trimmed[class_pos + "class ".len()..];
    let name = after_class
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .next()
        .unwrap_or("")
        .trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn is_method_declaration(trimmed: &str) -> bool {
    trimmed.contains('(')
        && trimmed.contains(')')
        && !trimmed.starts_with("if ")
        && !trimmed.starts_with("for ")
        && !trimmed.starts_with("foreach ")
        && !trimmed.starts_with("while ")
        && !trimmed.starts_with("switch ")
        && !trimmed.starts_with("catch ")
        && !trimmed.starts_with("using ")
        && !trimmed.starts_with("return ")
}

fn route_from_method_attrs(attrs: &[Attribute]) -> Option<(&'static str, Option<String>, usize)> {
    let http_attr = attrs.iter().find_map(|attr| {
        HTTP_ATTRIBUTE_METHODS
            .iter()
            .find(|(name, _)| attr.name == *name)
            .map(|(_, method)| (*method, attr.route.clone(), attr.line))
    })?;

    let route_attr = attrs
        .iter()
        .find(|attr| attr.name == "Route")
        .and_then(|attr| attr.route.clone());

    Some((http_attr.0, http_attr.1.or(route_attr), http_attr.2))
}

fn combine_routes(controller: Option<&ControllerContext>, route_fragment: Option<&str>) -> String {
    let fragment = route_fragment.unwrap_or("").trim();
    let fragment = replace_controller_tokens(fragment, controller);
    let prefix = controller
        .and_then(|ctx| ctx.route_prefix.as_deref())
        .map(|prefix| replace_controller_tokens(prefix, controller))
        .unwrap_or_default();

    if fragment.starts_with('/') {
        return normalize_route(&fragment);
    }
    if let Some(stripped) = fragment.strip_prefix("~/") {
        return normalize_route(stripped);
    }

    let mut parts = Vec::new();
    if !prefix.trim_matches('/').is_empty() {
        parts.push(prefix.trim_matches('/').to_string());
    }
    if !fragment.trim_matches('/').is_empty() {
        parts.push(fragment.trim_matches('/').to_string());
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("/{}", parts.join("/"))
    }
}

fn replace_controller_tokens(route: &str, controller: Option<&ControllerContext>) -> String {
    let Some(controller) = controller else {
        return route.to_string();
    };
    let controller_name = controller
        .name
        .strip_suffix("Controller")
        .unwrap_or(&controller.name);
    route
        .replace("[controller]", controller_name)
        .replace("[Controller]", controller_name)
}

fn normalize_route(route: &str) -> String {
    let trimmed = route.trim();
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
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

    #[test]
    fn test_commented_out_endpoints_skipped() {
        let content = r#"
// app.MapGet("/debug", handler);
/* app.MapPost("/old", handler); */
app.MapGet("/real", handler);
"#;
        let mut result = ExtractionResult::default();
        let path = PathBuf::from("src/Program.cs");
        extract_endpoints_from_source(content, &path, "test", &mut result);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].id.name, "GET /real");
    }

    #[test]
    fn test_line_numbers_are_1_based() {
        let content = "app.MapGet(\"/first\", handler);\napp.MapPost(\"/second\", handler);";
        let mut result = ExtractionResult::default();
        let path = PathBuf::from("src/Program.cs");
        extract_endpoints_from_source(content, &path, "test", &mut result);
        assert_eq!(result.nodes[0].line_start, 1);
        assert_eq!(result.nodes[1].line_start, 2);
    }

    #[test]
    fn test_whitespace_before_paren() {
        let content = "app.MapGet (\"/spaced\", handler);";
        let mut result = ExtractionResult::default();
        let path = PathBuf::from("src/Program.cs");
        extract_endpoints_from_source(content, &path, "test", &mut result);
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].id.name, "GET /spaced");
    }

    #[test]
    fn test_extracts_controller_http_attribute_routes() {
        let content = r#"
using Microsoft.AspNetCore.Mvc;

[ApiController]
public sealed class WeatherController : ControllerBase
{
    [HttpGet("weather/{city}")]
    public IActionResult GetWeather(string city) => Ok(new { city });

    [HttpPost]
    [Route("weather")]
    public IActionResult CreateWeather([FromBody] object request) => Ok(request);
}
"#;
        let mut result = ExtractionResult::default();
        let path = PathBuf::from("Controllers/WeatherController.cs");
        extract_endpoints_from_source(content, &path, "test", &mut result);

        let names: Vec<_> = result
            .nodes
            .iter()
            .map(|node| node.id.name.as_str())
            .collect();
        assert_eq!(names, vec!["GET /weather/{city}", "POST /weather"]);
        assert!(result.nodes.iter().all(|node| {
            node.metadata.get("endpoint_source").map(|s| s.as_str()) == Some("server_route")
        }));
    }

    #[test]
    fn test_combines_controller_and_method_route_attributes() {
        let content = r#"
using Microsoft.AspNetCore.Mvc;

[Route("api/[controller]")]
public class WeatherController : ControllerBase
{
    [HttpGet("{city}")]
    public IActionResult GetWeather(string city) => Ok(new { city });

    [HttpDelete("~/admin/weather/{city}")]
    public IActionResult DeleteWeather(string city) => Ok();
}
"#;
        let mut result = ExtractionResult::default();
        let path = PathBuf::from("Controllers/WeatherController.cs");
        extract_endpoints_from_source(content, &path, "test", &mut result);

        let names: Vec<_> = result
            .nodes
            .iter()
            .map(|node| node.id.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["GET /api/Weather/{city}", "DELETE /admin/weather/{city}"]
        );
        assert_eq!(result.nodes[0].metadata["http_path"], "/api/Weather/{city}");
    }
}
