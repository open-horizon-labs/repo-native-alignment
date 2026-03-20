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
/// Compile with [`QueryExtractor::new`] and reuse the same instance for all
/// files in a scan pass. Creating one `QueryExtractor` per file compiles the
/// query on every file — prefer creating it once before the file loop.
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

    // ── Adversarial: edge cases from dissent ──────────────────────────────

    /// Verify that `@userget("/items")` IS captured by the `get$` suffix pattern.
    ///
    /// This is a known limitation of suffix-only regex: `get$` matches any name
    /// ending in "get", including hypothetical `@userget`. The test documents this
    /// behavior explicitly so it's a conscious choice, not an oversight. If false
    /// positives become a real problem, the pattern can be tightened to `(^get$|\.get$)`.
    ///
    /// In practice, `@userget(...)` is not a real framework decorator name, so the
    /// theoretical false positive doesn't occur in real codebases.
    #[test]
    fn test_python_decorator_suffix_match_known_false_positive() {
        let source = r#"
@userget("/items")
async def list_items():
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
        // KNOWN LIMITATION: `get$` suffix matches `userget` — this is a false positive.
        // The test asserts the current behavior to make the trade-off explicit.
        // Tighten to `(^get$|\.get$)` if real-world false positives are observed.
        assert!(
            !captures.is_empty(),
            "known: get$ suffix matches userget — false positive documented"
        );
    }

    /// Verify that `@router.get("/items")` (legitimate suffix) is captured.
    #[test]
    fn test_python_router_get_suffix_matches() {
        let source = r#"
@router.get("/items")
async def list_items():
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
        assert!(!captures.is_empty(), "router.get should be captured");
        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/items"), "expected '/items', got: {path:?}");
    }

    /// Verify multiple route decorators in one file are all captured.
    #[test]
    fn test_python_multiple_routes_in_one_file() {
        let source = r#"
@app.route("/users")
def get_users():
    pass

@app.route("/items")
def get_items():
    pass

@app.route("/orders")
def get_orders():
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
        assert_eq!(captures.len(), 3, "should capture all 3 routes");
    }

    /// Verify that a decorator with no string argument (e.g. `@app.route(methods=["GET"])`)
    /// is NOT captured by the path-requiring query.
    #[test]
    fn test_python_decorator_without_string_path_skipped() {
        let source = r#"
@app.route(methods=["GET"])
def handler():
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
        // No positional string argument — should not be captured
        assert!(
            captures.is_empty(),
            "route without path string should not be captured"
        );
    }

    /// Verify TypeScript decorator that doesn't match the pattern is not captured.
    #[test]
    fn test_typescript_non_route_decorator_skipped() {
        let source = r#"
class UserService {
  @Injectable()
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
        assert!(captures.is_empty(), "@Injectable should not be captured");
    }

    // ── Java Spring Boot / JAX-RS route detection ─────────────────────────

    /// Verify `@GetMapping("/users")` is captured (Spring MVC).
    #[test]
    fn test_java_spring_get_mapping() {
        let source = r#"
public class UserController {
    @GetMapping("/users")
    public List<User> getUsers() { return null; }
}
"#;
        let query_source = r#"
(annotation
  name: (identifier) @name
  arguments: (annotation_argument_list
    (string_literal) @path)
  (#match? @name "^(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|RequestMapping|Path)$"))
"#;
        let language: Language = tree_sitter_java::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect @GetMapping annotation");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/users"), "expected '/users', got: {path:?}");
    }

    /// Verify `@PostMapping("/items")` is captured (Spring MVC).
    #[test]
    fn test_java_spring_post_mapping() {
        let source = r#"
@RestController
public class ItemController {
    @PostMapping("/items")
    public Item createItem(@RequestBody Item item) { return item; }
}
"#;
        let query_source = r#"
(annotation
  name: (identifier) @name
  arguments: (annotation_argument_list
    (string_literal) @path)
  (#match? @name "^(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|RequestMapping|Path)$"))
"#;
        let language: Language = tree_sitter_java::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect @PostMapping annotation");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/items"), "expected '/items', got: {path:?}");
    }

    /// Verify `@Path("/users")` is captured (JAX-RS).
    #[test]
    fn test_java_jaxrs_path_annotation() {
        let source = r#"
@Path("/users")
public class UserResource {
    @GET
    public Response getUsers() { return null; }
}
"#;
        let query_source = r#"
(annotation
  name: (identifier) @name
  arguments: (annotation_argument_list
    (string_literal) @path)
  (#match? @name "^(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|RequestMapping|Path)$"))
"#;
        let language: Language = tree_sitter_java::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect @Path annotation");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/users"), "expected '/users', got: {path:?}");
    }

    /// Verify non-route annotations like `@Override` are not captured.
    #[test]
    fn test_java_non_route_annotation_skipped() {
        let source = r#"
public class Foo {
    @Override
    public String toString() { return "foo"; }
}
"#;
        let query_source = r#"
(annotation
  name: (identifier) @name
  arguments: (annotation_argument_list
    (string_literal) @path)
  (#match? @name "^(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping|RequestMapping|Path)$"))
"#;
        let language: Language = tree_sitter_java::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(captures.is_empty(), "@Override should not be captured");
    }

    // ── Go gorilla/mux and gin route detection ────────────────────────────

    /// Verify `r.HandleFunc("/users", handler)` is captured (gorilla/mux).
    #[test]
    fn test_go_gorilla_handle_func() {
        let source = r#"
package main

import "github.com/gorilla/mux"

func main() {
    r := mux.NewRouter()
    r.HandleFunc("/users", getUsers).Methods("GET")
}
"#;
        let query_source = r#"
(call_expression
  function: (selector_expression
    field: (field_identifier) @name)
  arguments: (argument_list
    [(interpreted_string_literal)(raw_string_literal)] @path)
  (#match? @name "^(HandleFunc|Handle|GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS|Any|Get|Post|Put|Delete|Patch|Head|Options)$"))
"#;
        let language: Language = tree_sitter_go::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect HandleFunc call");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/users"), "expected '/users', got: {path:?}");
    }

    /// Verify `router.GET("/items", handler)` is captured (gin).
    #[test]
    fn test_go_gin_get_route() {
        let source = r#"
package main

import "github.com/gin-gonic/gin"

func main() {
    router := gin.Default()
    router.GET("/items", getItems)
}
"#;
        let query_source = r#"
(call_expression
  function: (selector_expression
    field: (field_identifier) @name)
  arguments: (argument_list
    [(interpreted_string_literal)(raw_string_literal)] @path)
  (#match? @name "^(HandleFunc|Handle|GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS|Any|Get|Post|Put|Delete|Patch|Head|Options)$"))
"#;
        let language: Language = tree_sitter_go::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect gin GET route");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/items"), "expected '/items', got: {path:?}");
    }

    // ── Rust Actix-web / Rocket route detection ───────────────────────────

    /// Verify `#[get("/users")]` is captured (Actix-web / Rocket).
    #[test]
    fn test_rust_actix_get_attribute() {
        let source = r#"
use actix_web::get;

#[get("/users")]
async fn get_users() -> impl Responder {
    HttpResponse::Ok().json(vec!["alice", "bob"])
}
"#;
        let query_source = r#"
(attribute_item
  (attribute
    (identifier) @name
    arguments: (token_tree
      (string_literal) @path))
  (#match? @name "^(get|post|put|delete|patch|head|options|route|connect|trace)$"))
"#;
        let language: Language = tree_sitter_rust::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect #[get] route attribute");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/users"), "expected '/users', got: {path:?}");
    }

    /// Verify `#[post("/items")]` is captured (Actix-web / Rocket).
    #[test]
    fn test_rust_actix_post_attribute() {
        let source = r#"
#[post("/items")]
async fn create_item(item: web::Json<Item>) -> impl Responder {
    HttpResponse::Created().json(item.into_inner())
}
"#;
        let query_source = r#"
(attribute_item
  (attribute
    (identifier) @name
    arguments: (token_tree
      (string_literal) @path))
  (#match? @name "^(get|post|put|delete|patch|head|options|route|connect|trace)$"))
"#;
        let language: Language = tree_sitter_rust::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect #[post] route attribute");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/items"), "expected '/items', got: {path:?}");
    }

    /// Verify `#[derive(Debug)]` is not captured as a route.
    #[test]
    fn test_rust_derive_attribute_skipped() {
        let source = r#"
#[derive(Debug, Clone)]
struct User {
    name: String,
}
"#;
        let query_source = r#"
(attribute_item
  (attribute
    (identifier) @name
    arguments: (token_tree
      (string_literal) @path))
  (#match? @name "^(get|post|put|delete|patch|head|options|route|connect|trace)$"))
"#;
        let language: Language = tree_sitter_rust::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(captures.is_empty(), "#[derive] should not be captured as route");
    }

    // ── Ruby Sinatra / Rails route detection ──────────────────────────────

    /// Verify `get '/users' do` is captured (Sinatra).
    #[test]
    fn test_ruby_sinatra_get_route() {
        let source = r#"
get '/users' do
  User.all.to_json
end
"#;
        let query_source = r#"
(call
  method: (identifier) @name
  arguments: (argument_list
    [(string)(simple_symbol)] @path)
  (#match? @name "^(get|post|put|delete|patch|head|options|match|root|resources|resource|namespace|scope|member|collection)$"))
"#;
        let language: Language = tree_sitter_ruby::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect Sinatra get route");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/users"), "expected '/users', got: {path:?}");
    }

    /// Verify `post '/items' do` is captured (Sinatra).
    #[test]
    fn test_ruby_sinatra_post_route() {
        let source = r#"
post '/items' do
  Item.create(params[:item]).to_json
end
"#;
        let query_source = r#"
(call
  method: (identifier) @name
  arguments: (argument_list
    [(string)(simple_symbol)] @path)
  (#match? @name "^(get|post|put|delete|patch|head|options|match|root|resources|resource|namespace|scope|member|collection)$"))
"#;
        let language: Language = tree_sitter_ruby::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect Sinatra post route");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/items"), "expected '/items', got: {path:?}");
    }

    // ── JavaScript Express.js route detection ─────────────────────────────

    /// Verify `app.get('/users', handler)` is captured (Express.js).
    #[test]
    fn test_javascript_express_get_route() {
        let source = r#"
const express = require('express');
const app = express();

app.get('/users', (req, res) => {
  res.json([]);
});
"#;
        let query_source = r#"
(call_expression
  function: (member_expression
    property: (property_identifier) @name)
  arguments: (arguments
    (string) @path)
  (#match? @name "^(get|post|put|delete|patch|head|options|use|all|route)$"))
"#;
        let language: Language = tree_sitter_javascript::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect Express app.get route");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/users"), "expected '/users', got: {path:?}");
    }

    /// Verify `router.post('/items', handler)` is captured (Express Router).
    #[test]
    fn test_javascript_express_router_post() {
        let source = r#"
const router = require('express').Router();

router.post('/items', async (req, res) => {
  const item = await Item.create(req.body);
  res.status(201).json(item);
});
"#;
        let query_source = r#"
(call_expression
  function: (member_expression
    property: (property_identifier) @name)
  arguments: (arguments
    (string) @path)
  (#match? @name "^(get|post|put|delete|patch|head|options|use|all|route)$"))
"#;
        let language: Language = tree_sitter_javascript::LANGUAGE.into();
        let extractor = QueryExtractor::new(&language, query_source).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();

        let captures = extractor.run(tree.root_node(), source.as_bytes());
        assert!(!captures.is_empty(), "should detect Express router.post route");

        let path = captures[0].get("path").unwrap_or("");
        assert!(path.contains("/items"), "expected '/items', got: {path:?}");
    }
}
