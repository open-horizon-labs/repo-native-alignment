//! Tree-sitter Query API helper for declarative pattern matching.
//!
//! Provides [`QueryExtractor`] — a helper that runs tree-sitter `Query` patterns
//! against a parsed syntax tree and returns matched captures as [`CaptureSet`]s.
//!
//! # Why
//!
//! Manual AST traversal requires hardcoded node-kind checks per framework.
//! The tree-sitter query API supports `#match?` and `#eq?` predicates that
//! let a single query cover multiple framework variants declaratively.
//!
//! # Usage
//!
//! ```rust,ignore
//! let extractor = QueryExtractor::new(language, query_source)?;
//! let captures = extractor.run(root_node, source_bytes);
//! for capture_set in captures {
//!     let path_text = capture_set.get("path");
//!     let name_text = capture_set.get("name");
//!     // emit node ...
//! }
//! ```
//!
//! # Route decorator detection
//!
//! The primary first use: detecting HTTP route decorators across frameworks.
//! See [`RouteQueryConfig`] and the Python/TypeScript configs in `extract/configs.rs`.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use tree_sitter::{Language, Node, Query, QueryCursor, StreamingIterator};

// ---------------------------------------------------------------------------
// CaptureSet — results of one query match
// ---------------------------------------------------------------------------

/// A set of named captures from one query match.
///
/// Each entry maps a capture name (e.g. `"path"`, `"name"`) to the matched
/// source text. Captures that had no match for this particular match instance
/// are absent from the map.
#[derive(Debug, Clone, Default)]
pub struct CaptureSet {
    /// Byte offset of the first capture in this match (for position tracking).
    pub start_byte: usize,
    /// Byte offset just past the last capture.
    pub end_byte: usize,
    /// Row (0-indexed) of the first capture.
    pub start_row: usize,
    /// Named capture text values.
    captures: BTreeMap<String, String>,
}

impl CaptureSet {
    /// Get the text for a named capture. Returns `None` if that capture had
    /// no match in this instance.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.captures.get(name).map(|s| s.as_str())
    }

    /// Returns `true` if the capture set has no named values.
    pub fn is_empty(&self) -> bool {
        self.captures.is_empty()
    }
}

// ---------------------------------------------------------------------------
// QueryExtractor
// ---------------------------------------------------------------------------

/// Wraps a compiled tree-sitter `Query` and provides a simple `run()` method
/// that returns [`CaptureSet`]s for each match.
///
/// Compile once (via [`QueryExtractor::new`]) and reuse across files.
pub struct QueryExtractor {
    query: Query,
}

impl QueryExtractor {
    /// Compile a query for the given language.
    ///
    /// `query_source` is a tree-sitter query string, e.g.:
    /// ```scheme
    /// (decorator (call function: (identifier) @name
    ///                  arguments: (argument_list (string) @path))
    ///   (#match? @name "^(route|get|post)$"))
    /// ```
    pub fn new(language: &Language, query_source: &str) -> Result<Self> {
        let query = Query::new(language, query_source)
            .with_context(|| format!("failed to compile tree-sitter query:\n{query_source}"))?;
        Ok(Self { query })
    }

    /// Run the query against `root` and return one [`CaptureSet`] per match.
    ///
    /// `source` must be the UTF-8 bytes of the file that was parsed into `root`.
    pub fn run<'tree>(&self, root: Node<'tree>, source: &[u8]) -> Vec<CaptureSet> {
        let mut cursor = QueryCursor::new();
        let mut results = Vec::new();

        let capture_names = self.query.capture_names();
        let mut matches = cursor.matches(&self.query, root, source);

        while let Some(m) = matches.next() {
            let mut set = CaptureSet::default();
            let mut first = true;

            for capture in m.captures {
                let node = capture.node;
                let name = capture_names[capture.index as usize];
                let text = node.utf8_text(source).unwrap_or("").trim().to_string();

                if first {
                    set.start_byte = node.start_byte();
                    set.end_byte = node.end_byte();
                    set.start_row = node.start_position().row;
                    first = false;
                } else {
                    if node.start_byte() < set.start_byte {
                        set.start_byte = node.start_byte();
                        set.start_row = node.start_position().row;
                    }
                    if node.end_byte() > set.end_byte {
                        set.end_byte = node.end_byte();
                    }
                }

                set.captures.insert(name.to_string(), text);
            }

            if !set.is_empty() {
                results.push(set);
            }
        }

        results
    }
}

// ---------------------------------------------------------------------------
// RouteQueryConfig — per-language route decorator query
// ---------------------------------------------------------------------------

/// Configuration for a single route-decorator query pattern.
///
/// Each `RouteQueryConfig` describes one tree-sitter query that detects
/// HTTP route decorators in a specific language/framework. The query must
/// produce at least a `@path` capture containing the HTTP path string.
/// An optional `@method` capture carries the HTTP method verb.
///
/// Used in [`LangConfig::route_queries`](super::generic::LangConfig).
#[derive(Debug, Clone, Copy)]
pub struct RouteQueryConfig {
    /// Human-readable label for logging/debugging (e.g. `"python-flask"`).
    pub label: &'static str,
    /// The tree-sitter query source string.
    pub query: &'static str,
    /// Default HTTP method when `@method` capture is absent (e.g. `"GET"`).
    pub default_method: &'static str,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Python Flask/FastAPI route detection ──────────────────────────────

    /// Verify that `@app.route("/users")` is captured as a route decorator.
    #[test]
    fn test_python_flask_route_decorator() {
        let source = r#"
@app.route("/users")
def get_users():
    pass
"#;
        // Query: match decorator with call whose function name is a known route method
        // and first argument is a string (the path).
        let query_source = r#"
(decorator
  (call
    function: (_) @name
    arguments: (argument_list
      (string) @path))
  (#match? @name "route$|get$|post$|put$|delete$|patch$"))
"#;
        let language: Language = tree_sitter_python::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect Flask route decorator");

        let first = &captures[0];
        let path = first.get("path").unwrap_or("");
        assert!(
            path.contains("/users"),
            "path capture should contain '/users', got: {path:?}"
        );
    }

    /// Verify that `@router.post("/items")` is captured.
    #[test]
    fn test_python_fastapi_post_decorator() {
        let source = r#"
@router.post("/items")
async def create_item():
    pass
"#;
        let query_source = r#"
(decorator
  (call
    function: (_) @name
    arguments: (argument_list
      (string) @path))
  (#match? @name "route$|get$|post$|put$|delete$|patch$"))
"#;
        let language: Language = tree_sitter_python::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect FastAPI router.post decorator");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/items"), "expected '/items', got: {path:?}");
    }

    // ── TypeScript NestJS route detection ────────────────────────────────

    /// Verify that `@Get("/users")` on a method is captured.
    #[test]
    fn test_typescript_nestjs_get_decorator() {
        let source = r#"
class UserController {
  @Get("/users")
  findAll() {}
}
"#;
        // TypeScript: decorators are call_expression or identifier inside decorator node
        let query_source = r#"
(decorator
  (call_expression
    function: (identifier) @name
    arguments: (arguments
      (string) @path))
  (#match? @name "^(Get|Post|Put|Delete|Patch|Head|Options|Controller|Route)$"))
"#;
        let language: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect NestJS @Get decorator");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/users"), "expected '/users', got: {path:?}");
    }

    /// Verify that `@Post("/items")` is captured.
    #[test]
    fn test_typescript_nestjs_post_decorator() {
        let source = r#"
class ItemController {
  @Post("/items")
  create() {}
}
"#;
        let query_source = r#"
(decorator
  (call_expression
    function: (identifier) @name
    arguments: (arguments
      (string) @path))
  (#match? @name "^(Get|Post|Put|Delete|Patch|Head|Options|Controller|Route)$"))
"#;
        let language: Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect NestJS @Post decorator");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/items"), "expected '/items', got: {path:?}");
    }

    // ── Non-route decorators should not be captured ──────────────────────

    #[test]
    fn test_python_non_route_decorator_skipped() {
        let source = r#"
@login_required
def profile():
    pass
"#;
        let query_source = r#"
(decorator
  (call
    function: (_) @name
    arguments: (argument_list
      (string) @path))
  (#match? @name "route$|get$|post$|put$|delete$|patch$"))
"#;
        let language: Language = tree_sitter_python::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();

        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        // @login_required takes no path argument, so no (string) capture → no match
        assert!(
            captures.is_empty(),
            "non-route decorator should not be captured"
        );
    }

    // ── QueryExtractor compilation error ─────────────────────────────────

    #[test]
    fn test_bad_query_returns_error() {
        let language: Language = tree_sitter_python::LANGUAGE.into();
        let result = QueryExtractor::new(&language, "(this_node_does_not_exist)");
        assert!(result.is_err(), "invalid query should return Err");
    }

    // ── CaptureSet helper ─────────────────────────────────────────────────

    #[test]
    fn test_capture_set_empty() {
        let set = CaptureSet::default();
        assert!(set.is_empty());
        assert_eq!(set.get("anything"), None);
    }
}
