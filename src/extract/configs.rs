//! Static `LangConfig` definitions for all tree-sitter code extractors.
//!
//! Each config drives the `GenericExtractor`. Adding a new `NodeKind` to all
//! languages is a one-column change in these tables — not 22 file edits.
//!
//! Languages with special cases (Python ALL_CAPS, Go multi-name const, C++
//! complex function detection) keep a thin per-language wrapper that calls
//! `GenericExtractor::new(&LANG_CONFIG).run()` and appends custom nodes.

use crate::graph::NodeKind;
use super::generic::LangConfig;
use super::query::RouteQueryConfig;

// ---------------------------------------------------------------------------
// Route query patterns
// ---------------------------------------------------------------------------

/// Python Flask/FastAPI/Starlette/Django-ninja route decorator query.
///
/// Matches decorators of the form:
/// - `@app.route("/path")`
/// - `@router.get("/path")`
/// - `@app.post("/path")`
/// etc.
///
/// The `@name` capture is the full function (attribute access or identifier),
/// and `@path` is the first string argument.
static PYTHON_ROUTE_QUERY: RouteQueryConfig = RouteQueryConfig {
    label: "python-route-decorators",
    query: r#"
(decorator
  (call
    function: (_) @name
    arguments: (argument_list
      (string) @path))
  (#match? @name "route$|get$|post$|put$|delete$|patch$|head$|options$"))
"#,
    default_method: "GET",
};

/// TypeScript NestJS / routing-controllers / tsoa decorator query.
///
/// Matches HTTP-method decorators on controller methods:
/// - `@Get("/users")`
/// - `@Post("/items")`
/// - `@HttpGet("/path")`
///
/// NOTE: `@Controller("/prefix")` and `@Route("/prefix")` are intentionally
/// excluded. These are class-level prefix annotations, not endpoint declarations.
/// Emitting them as `ApiEndpoint` nodes would create incorrect `GET /prefix`
/// nodes — the actual endpoints are the combination of prefix + method path.
/// Path combination (#390) will address this when linking decorators to functions.
static TYPESCRIPT_ROUTE_QUERY: RouteQueryConfig = RouteQueryConfig {
    label: "typescript-route-decorators",
    query: r#"
(decorator
  (call_expression
    function: (identifier) @name
    arguments: (arguments
      (string) @path))
  (#match? @name "^(Get|Post|Put|Delete|Patch|Head|Options|HttpGet|HttpPost|HttpPut|HttpDelete|HttpPatch)$"))
"#,
    default_method: "GET",
};

/// Java Spring MVC method-level route annotation query.
///
/// Matches annotations where the HTTP method is deterministic:
/// - `@GetMapping("/users")`     → GET
/// - `@PostMapping("/items")`    → POST
/// - `@PutMapping("/items/{id}")` → PUT
/// - `@DeleteMapping("/items/{id}")` → DELETE
/// - `@PatchMapping("/items/{id}")` → PATCH
///
/// NOT included (deferred to #390 or later):
/// - `@RequestMapping` — can specify any method or no method; class or method level
/// - `@Path` (JAX-RS) — URL-only; HTTP method comes from separate `@GET/@POST/...`
///
/// These omitted patterns collapse to GET in `infer_method_from_name()`, producing
/// incorrect metadata before multi-method and prefix expansion are implemented.
static JAVA_ROUTE_QUERY: RouteQueryConfig = RouteQueryConfig {
    label: "java-route-annotations",
    query: r#"
(annotation
  name: (identifier) @name
  arguments: (annotation_argument_list
    (string_literal) @path)
  (#match? @name "^(GetMapping|PostMapping|PutMapping|DeleteMapping|PatchMapping)$"))
"#,
    default_method: "GET",
};

/// Go gin, echo, fiber, chi route registration query.
///
/// Matches method calls where the HTTP method is deterministic:
/// - `router.GET("/path", handler)`    (gin, echo — uppercase)
/// - `app.Get("/path", handler)`       (fiber — uppercase first letter)
/// - `r.Get("/path", handler)`         (chi — lowercase first letter)
///
/// NOT included (deferred to #390 or later):
/// - `HandleFunc` — method unconstrained unless `Methods("GET", ...)` is chained
/// - `Handle`     — path matcher only, method unconstrained
/// - `Any`        — matches all HTTP methods, not a single-method endpoint
///
/// These omitted patterns collapse to GET in `infer_method_from_name()`, producing
/// incorrect metadata before multi-method and prefix expansion are implemented.
static GO_ROUTE_QUERY: RouteQueryConfig = RouteQueryConfig {
    label: "go-route-registration",
    query: r#"
(call_expression
  function: (selector_expression
    field: (field_identifier) @name)
  arguments: (argument_list
    [(interpreted_string_literal)(raw_string_literal)] @path)
  (#match? @name "^(GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS|Get|Post|Put|Delete|Patch|Head|Options)$"))
"#,
    default_method: "GET",
};

/// Rust Actix-web / Rocket / Poem route attribute query.
///
/// Matches attribute macros where the HTTP method is deterministic:
/// - `#[get("/users")]`    (Actix-web, Rocket)   → GET
/// - `#[post("/items")]`   (Actix-web, Rocket)   → POST
/// - `#[put("/items/{id}")]`  (Actix-web, Rocket) → PUT
/// - `#[delete("/items/{id}")]` (Actix-web, Rocket) → DELETE
/// - `#[patch("/items/{id}")]`  (Actix-web, Rocket) → PATCH
/// - `#[head("/path")]`    (Actix-web)            → HEAD
/// - `#[options("/path")]` (Actix-web)            → OPTIONS
///
/// NOT included (deferred to #390 or later):
/// - `route(...)` — Actix-web multi-method: `#[route("/path", method="GET", method="POST")]`
///   requires parsing the nested `method=` argument, not just the path
/// - `connect`/`trace` — uncommon; include if demand emerges
///
/// Tree-sitter represents `#[get("/path")]` as:
///   `(attribute_item (attribute (identifier) @name arguments: (token_tree (string_literal) @path)))`
///
/// Note: Axum uses `Router::route("/path", ...)` (function call style), not attribute macros.
static RUST_ROUTE_QUERY: RouteQueryConfig = RouteQueryConfig {
    label: "rust-route-attributes",
    query: r#"
(attribute_item
  (attribute
    (identifier) @name
    arguments: (token_tree
      (string_literal) @path))
  (#match? @name "^(get|post|put|delete|patch|head|options)$"))
"#,
    default_method: "GET",
};

/// JavaScript / Node.js Express route registration query.
///
/// Matches method calls where the HTTP method is deterministic:
/// - `app.get('/users', handler)`     (Express)    → GET
/// - `router.post('/items', handler)` (Express)    → POST
/// - `app.put('/items/:id', handler)` (Express)    → PUT
/// - `app.delete('/items/:id', handler)` (Express) → DELETE
/// - `app.patch('/items/:id', handler)` (Express)  → PATCH
///
/// NOT included (deferred to #390 or later):
/// - `use`   — middleware mount; not a route endpoint
/// - `all`   — matches all HTTP methods; not a single-method endpoint
/// - `route` — creates a route group for chaining `.get().post()`; not a direct endpoint
///
/// These omitted patterns collapse to GET in `infer_method_from_name()`, producing
/// incorrect metadata before multi-method and prefix expansion are implemented.
///
/// Tree-sitter JavaScript uses `member_expression` (not `selector_expression`
/// like Go), with `property: (property_identifier)` for the method name.
static JAVASCRIPT_ROUTE_QUERY: RouteQueryConfig = RouteQueryConfig {
    label: "javascript-express-routes",
    query: r#"
(call_expression
  function: (member_expression
    property: (property_identifier) @name)
  arguments: (arguments
    (string) @path)
  (#match? @name "^(get|post|put|delete|patch|head|options)$"))
"#,
    default_method: "GET",
};

/// Ruby Sinatra / Rails route method call query.
///
/// Matches route calls where the HTTP method is deterministic:
/// - `get '/users' do ... end`    (Sinatra)  → GET
/// - `post '/items' do ... end`   (Sinatra)  → POST
/// - `put '/items/:id' do ...`    (Sinatra)  → PUT
/// - `delete '/items/:id' do ...` (Sinatra)  → DELETE
/// - `patch '/items/:id' do ...`  (Sinatra)  → PATCH
///
/// NOT included (deferred to #390 or later):
/// - `match`      — matches multiple HTTP methods via `via:` option
/// - `root`       — shorthand for `get '/'`; would need special handling
/// - `resources`  — expands to 7 REST routes; requires expansion logic
/// - `resource`   — singular resource expansion (similar to resources)
/// - `namespace`/`scope`/`member`/`collection` — prefix/grouping; not endpoints
///
/// These omitted patterns expand into multiple routes or apply prefixes that
/// cannot be collapsed to a single `{method, path}` pair without expansion logic.
///
/// Ruby's tree-sitter grammar represents bare method calls as `(call method: ...)`.
static RUBY_ROUTE_QUERY: RouteQueryConfig = RouteQueryConfig {
    label: "ruby-sinatra-rails-routes",
    query: r#"
(call
  method: (identifier) @name
  arguments: (argument_list
    (string) @path)
  (#match? @name "^(get|post|put|delete|patch|head|options)$"))
"#,
    default_method: "GET",
};

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

pub static PYTHON_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_python::LANGUAGE.into(),
    language_name: "python",
    extensions: &["py"],
    node_kinds: &[
        ("function_definition",      NodeKind::Function),
        ("class_definition",         NodeKind::Struct),
        ("import_statement",         NodeKind::Import),
        ("import_from_statement",    NodeKind::Import),
        // Python has no keyword for fields; ALL_CAPS consts handled in python.rs
    ],
    scope_parent_kinds: &["class_definition"],
    const_value_field: None,
    full_text_name_kinds: &["import_statement", "import_from_statement"],
    string_literal_kinds: &[("string", None)],
    param_container_field: Some("parameters"),
    param_type_field: Some("type"),
    return_type_field: Some("return_type"),
    type_requires_uppercase: false,
    branch_node_types: &[
        "if_statement", "elif_clause", "else_clause",
        "for_statement", "while_statement",
        "boolean_operator",  // and, or
        "try_statement", "except_clause",
        "conditional_expression",  // ternary
    ],
    decorator_node_kinds: &["decorator"],
    type_param_node_kind: None,  // Python uses runtime generics (typing.Generic), not tree-sitter type_parameters
    docstring_in_body: true,     // Python uses triple-quoted strings as docstrings inside the function body
    route_queries: &[PYTHON_ROUTE_QUERY],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: Some(("call", "function")),
    pub_visibility_modifier: None,
    has_all_export: true,
    test_name_prefix: true,
};

// ---------------------------------------------------------------------------
// TypeScript
// ---------------------------------------------------------------------------

pub static TYPESCRIPT_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_typescript::LANGUAGE_TSX.into(),
    language_name: "typescript",
    extensions: &["ts", "tsx"],
    node_kinds: &[
        ("function_declaration",       NodeKind::Function),
        ("method_definition",          NodeKind::Function),
        ("class_declaration",          NodeKind::Struct),
        ("interface_declaration",      NodeKind::Trait),
        ("enum_declaration",           NodeKind::Enum),
        ("public_field_definition",    NodeKind::Field),
        ("method_signature",           NodeKind::Function),
        // enum variants handled as special case in typescript.rs (TS uses
        // property_identifier / enum_assignment, not a dedicated enum_member node type)
        // module-level const handled as special case in typescript.rs
    ],
    scope_parent_kinds: &["class_declaration", "enum_declaration", "interface_declaration"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string", Some("string_fragment"))],
    // TS: formal_parameters accessed via field "parameters",
    // each required_parameter has field "type" -> type_annotation node
    param_container_field: Some("parameters"),
    param_type_field: Some("type"),
    return_type_field: Some("return_type"),
    type_requires_uppercase: true,
    branch_node_types: &[
        "if_statement", "else_clause",
        "switch_statement", "switch_case",
        "for_statement", "for_in_statement", "while_statement", "do_statement",
        "binary_expression",  // covers && and ||
        "ternary_expression",
        "try_statement", "catch_clause",
        "optional_chain_expression",  // ?.
    ],
    decorator_node_kinds: &["decorator"],
    type_param_node_kind: Some("type_parameters"),
    docstring_in_body: false,
    route_queries: &[TYPESCRIPT_ROUTE_QUERY],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: Some(("call_expression", "function")),
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};

// ---------------------------------------------------------------------------
// JavaScript
// ---------------------------------------------------------------------------

pub static JAVASCRIPT_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_javascript::LANGUAGE.into(),
    language_name: "javascript",
    extensions: &["js", "jsx", "mjs"],
    node_kinds: &[
        ("function_declaration",           NodeKind::Function),
        ("generator_function_declaration", NodeKind::Function),
        ("method_definition",              NodeKind::Function),
        ("class_declaration",              NodeKind::Struct),
        ("class",                          NodeKind::Struct),
        // module-level const handled in javascript.rs
    ],
    scope_parent_kinds: &["class_declaration", "class"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string", Some("string_fragment"))],
    param_container_field: None,
    param_type_field: None,
    return_type_field: None,
    type_requires_uppercase: true,
    branch_node_types: &[
        "if_statement", "else_clause",
        "switch_statement", "switch_case",
        "for_statement", "for_in_statement", "while_statement", "do_statement",
        "binary_expression",  // covers && and ||
        "ternary_expression",
        "try_statement", "catch_clause",
    ],
    decorator_node_kinds: &["decorator"],
    type_param_node_kind: None,  // JavaScript has no generics
    docstring_in_body: false,
    route_queries: &[JAVASCRIPT_ROUTE_QUERY],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: Some(("call_expression", "function")),
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};

// ---------------------------------------------------------------------------
// Go -- thin config; multi-name const and receiver handled in go.rs
// ---------------------------------------------------------------------------

pub static GO_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_go::LANGUAGE.into(),
    language_name: "go",
    extensions: &["go"],
    node_kinds: &[
        ("function_declaration", NodeKind::Function),
        ("method_declaration",   NodeKind::Function),
        // type_declaration / const_declaration handled specially in go.rs
    ],
    scope_parent_kinds: &[],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[
        ("interpreted_string_literal", None),
        ("raw_string_literal",         None),
    ],
    param_container_field: Some("parameters"),
    param_type_field: Some("type"),
    return_type_field: Some("result"),
    type_requires_uppercase: false,
    branch_node_types: &[
        "if_statement", "else_clause",
        "expression_switch_statement", "expression_case",
        "type_switch_statement", "type_case",
        "for_statement",  // Go's only loop
        "select_statement", "communication_case",
        "binary_expression",  // && and ||
    ],
    decorator_node_kinds: &[],  // Go has no decorators/attributes
    type_param_node_kind: Some("type_parameter_list"),
    docstring_in_body: false,
    route_queries: &[GO_ROUTE_QUERY],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: Some(("call_expression", "function")),
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};

// ---------------------------------------------------------------------------
// Java
// ---------------------------------------------------------------------------

pub static JAVA_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_java::LANGUAGE.into(),
    language_name: "java",
    extensions: &["java"],
    node_kinds: &[
        ("class_declaration",       NodeKind::Struct),
        ("record_declaration",      NodeKind::Struct),
        ("interface_declaration",   NodeKind::Trait),
        ("enum_declaration",        NodeKind::Enum),
        ("method_declaration",      NodeKind::Function),
        ("constructor_declaration", NodeKind::Function),
        ("field_declaration",       NodeKind::Field),
        ("enum_constant",           NodeKind::Field),
        // static final consts handled in java.rs (text inspection)
    ],
    scope_parent_kinds: &["class_declaration", "record_declaration", "enum_declaration", "interface_declaration"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string_literal", None)],
    // Java: formal_parameters node accessed via field "parameters" on
    // method_declaration; each formal_parameter has field "type".
    param_container_field: Some("parameters"),
    param_type_field: Some("type"),
    return_type_field: Some("type"),
    type_requires_uppercase: true,
    branch_node_types: &[
        "if_statement", "else_clause",
        "switch_expression", "switch_block_statement_group",
        "for_statement", "enhanced_for_statement", "while_statement", "do_statement",
        "binary_expression",  // covers && and ||
        "ternary_expression",
        "try_statement", "catch_clause",
    ],
    // Java annotations are children of `modifiers` on the declaration node.
    // The collect_decorators function handles this via Strategy 3 (child container).
    decorator_node_kinds: &["annotation", "marker_annotation"],
    type_param_node_kind: Some("type_parameters"),
    docstring_in_body: false,
    route_queries: &[JAVA_ROUTE_QUERY],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: Some(("method_invocation", "name")),
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};

// ---------------------------------------------------------------------------
// Kotlin
// ---------------------------------------------------------------------------

pub static KOTLIN_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_kotlin_ng::LANGUAGE.into(),
    language_name: "kotlin",
    extensions: &["kt", "kts"],
    node_kinds: &[
        ("function_declaration",    NodeKind::Function),
        ("class_declaration",       NodeKind::Struct),
        ("object_declaration",      NodeKind::Struct),
        ("enum_class_body",         NodeKind::Enum),
        ("property_declaration",    NodeKind::Field),
        ("enum_entry",              NodeKind::Field),
        // const val / companion object consts handled in kotlin.rs
    ],
    scope_parent_kinds: &["class_declaration", "object_declaration"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string_literal", Some("string_content"))],
    // Kotlin tree-sitter-kotlin-ng: function_value_parameters and parameter
    // types are not accessible via field names -- DependsOn skipped for now.
    // TODO: add per-language extractor logic for Kotlin DependsOn edges.
    param_container_field: None,
    param_type_field: None,
    return_type_field: None,
    type_requires_uppercase: true,
    branch_node_types: &[
        "if_expression", "else_clause",
        "when_expression", "when_entry",
        "for_statement", "while_statement", "do_while_statement",
        "conjunction_expression", "disjunction_expression",  // && and ||
        "try_expression", "catch_block",
    ],
    decorator_node_kinds: &["annotation"],
    type_param_node_kind: Some("type_parameters"),
    docstring_in_body: false,
    route_queries: &[],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: Some(("call_expression", "calleeExpression")),
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};

// ---------------------------------------------------------------------------
// C#
// ---------------------------------------------------------------------------

pub static CSHARP_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_c_sharp::LANGUAGE.into(),
    language_name: "csharp",
    extensions: &["cs"],
    node_kinds: &[
        ("class_declaration",       NodeKind::Struct),
        ("struct_declaration",      NodeKind::Struct),
        ("record_declaration",      NodeKind::Struct),
        ("interface_declaration",   NodeKind::Trait),
        ("enum_declaration",        NodeKind::Enum),
        ("method_declaration",      NodeKind::Function),
        ("constructor_declaration", NodeKind::Function),
        ("field_declaration",       NodeKind::Field),
        ("enum_member_declaration", NodeKind::Field),
        // const fields handled in csharp.rs (text inspection)
    ],
    scope_parent_kinds: &["class_declaration", "struct_declaration", "record_declaration", "enum_declaration", "interface_declaration"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string_literal", None)],
    // C#: parameter_list via field "parameters", param type via field "type",
    // return type via field "returns" (NOT "type" on method_declaration).
    param_container_field: Some("parameters"),
    param_type_field: Some("type"),
    return_type_field: Some("returns"),
    type_requires_uppercase: true,
    branch_node_types: &[
        "if_statement", "else_clause",
        "switch_statement", "switch_section",
        "for_statement", "for_each_statement", "while_statement", "do_statement",
        "binary_expression",  // covers && and ||
        "conditional_expression",  // ternary
        "try_statement", "catch_clause",
    ],
    decorator_node_kinds: &["attribute_list"],
    type_param_node_kind: Some("type_parameter_list"),
    docstring_in_body: false,
    route_queries: &[],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: Some(("invocation_expression", "function")),
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};

// ---------------------------------------------------------------------------
// Swift
// ---------------------------------------------------------------------------

pub static SWIFT_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_swift::LANGUAGE.into(),
    language_name: "swift",
    extensions: &["swift"],
    node_kinds: &[
        ("function_declaration",    NodeKind::Function),
        ("class_declaration",       NodeKind::Struct),
        ("struct_declaration",      NodeKind::Struct),
        ("enum_declaration",        NodeKind::Enum),
        ("protocol_declaration",    NodeKind::Trait),
        ("property_declaration",    NodeKind::Field),
        ("enum_case_element",       NodeKind::Field),
        ("import_declaration",      NodeKind::Import),
    ],
    scope_parent_kinds: &["class_declaration", "struct_declaration", "enum_declaration", "protocol_declaration"],
    const_value_field: None,
    full_text_name_kinds: &["import_declaration"],
    string_literal_kinds: &[("string_literal", Some("string_literal_segment"))],
    // Swift tree-sitter: parameters are direct children (no container field),
    // and type/return_type use the overloaded "name" field.
    // DependsOn skipped for now -- needs per-language extractor logic.
    // TODO: add per-language extractor logic for Swift DependsOn edges.
    param_container_field: None,
    param_type_field: None,
    return_type_field: None,
    type_requires_uppercase: true,
    branch_node_types: &[
        "if_statement", "else_clause",
        "switch_statement", "case_statement",
        "for_statement", "while_statement",
        "guard_statement",
        "ternary_expression",
    ],
    decorator_node_kinds: &[],  // Swift attributes handled via @attribute syntax but tree-sitter-swift uses attribute nodes as children, not siblings
    type_param_node_kind: Some("type_parameters"),
    docstring_in_body: false,
    route_queries: &[],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: Some(("call_expression", "function")),
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};

// ---------------------------------------------------------------------------
// Zig
// ---------------------------------------------------------------------------

pub static ZIG_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_zig::LANGUAGE.into(),
    language_name: "zig",
    extensions: &["zig"],
    node_kinds: &[
        ("function_declaration",  NodeKind::Function),
        ("struct_declaration",    NodeKind::Struct),
        ("enum_declaration",      NodeKind::Enum),
        // const handled in zig.rs (text inspection)
    ],
    scope_parent_kinds: &["struct_declaration", "enum_declaration"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string_literal", None)],
    // Zig: "parameters" is NOT a field name on function_declaration (it's a
    // child node kind). But param type is field "type" and return type is
    // field "type" on the function_declaration node.
    // TODO: add per-language extractor logic for Zig DependsOn param edges.
    param_container_field: None,
    param_type_field: None,
    return_type_field: None,
    type_requires_uppercase: true,
    branch_node_types: &[
        "if_expression", "else_clause",
        "switch_expression",
        "for_expression", "while_expression",
        "binary_expression",  // and, or
        "try_expression",
    ],
    decorator_node_kinds: &[],  // Zig has no decorators/attributes
    type_param_node_kind: None,  // Zig uses comptime generics, not tree-sitter type_parameters
    docstring_in_body: false,
    route_queries: &[],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: None,
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};

// ---------------------------------------------------------------------------
// C / C++
// ---------------------------------------------------------------------------

pub static CPP_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_cpp::LANGUAGE.into(),
    language_name: "cpp",
    extensions: &["cpp", "cc", "cxx", "c", "h", "hpp"],
    node_kinds: &[
        ("function_definition",  NodeKind::Function),
        ("class_specifier",      NodeKind::Struct),
        ("struct_specifier",     NodeKind::Struct),
        ("enum_specifier",       NodeKind::Enum),
        ("preproc_def",              NodeKind::Macro),
        ("preproc_function_def",     NodeKind::Macro),
        ("field_declaration",    NodeKind::Field),
        ("enumerator",           NodeKind::Field),
        // constexpr / static const handled in cpp.rs (text inspection)
    ],
    scope_parent_kinds: &["class_specifier", "struct_specifier", "enum_specifier"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string_literal", Some("string_content"))],
    // C++: parameters are on function_declarator (child of function_definition),
    // not directly on function_definition. Return type IS field "type" on
    // function_definition. DependsOn for params needs per-language logic.
    // TODO: add per-language extractor logic for C++ DependsOn param edges.
    param_container_field: None,
    param_type_field: None,
    return_type_field: Some("type"),
    type_requires_uppercase: true,
    branch_node_types: &[
        "if_statement", "else_clause",
        "switch_statement", "case_statement",
        "for_statement", "while_statement", "do_statement",
        "binary_expression",  // covers && and ||
        "conditional_expression",  // ternary
        "try_statement", "catch_clause",
    ],
    decorator_node_kinds: &[],  // C/C++ has no decorators (attributes like [[nodiscard]] are different)
    type_param_node_kind: Some("template_parameter_list"),
    docstring_in_body: false,
    route_queries: &[],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: Some(("call_expression", "function")),
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};

// ---------------------------------------------------------------------------
// Lua
// ---------------------------------------------------------------------------

pub static LUA_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_lua::LANGUAGE.into(),
    language_name: "lua",
    extensions: &["lua"],
    node_kinds: &[
        ("function_declaration", NodeKind::Function),
        // ALL_CAPS consts handled in lua.rs
    ],
    scope_parent_kinds: &[],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string", None)],
    param_container_field: None,
    param_type_field: None,
    return_type_field: None,
    type_requires_uppercase: true,
    branch_node_types: &[
        "if_statement", "elseif_statement", "else_statement",
        "for_statement", "while_statement", "repeat_statement",
        "binary_expression",  // and, or
    ],
    decorator_node_kinds: &[],  // Lua has no decorators
    type_param_node_kind: None,  // Lua has no generics
    docstring_in_body: false,
    route_queries: &[],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: None,
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};

// ---------------------------------------------------------------------------
// Ruby
// ---------------------------------------------------------------------------

pub static RUBY_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_ruby::LANGUAGE.into(),
    language_name: "ruby",
    extensions: &["rb"],
    node_kinds: &[
        ("method",          NodeKind::Function),
        ("singleton_method",NodeKind::Function),
        ("class",           NodeKind::Struct),
        ("singleton_class", NodeKind::Struct),
        ("module",          NodeKind::Module),
        // ALL_CAPS constants handled in ruby.rs (assignment with constant LHS)
    ],
    scope_parent_kinds: &["class", "singleton_class", "module"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string", Some("string_content"))],
    param_container_field: None,
    param_type_field: None,
    return_type_field: None,
    type_requires_uppercase: true,
    branch_node_types: &[
        "if", "elsif", "else", "unless",
        "case", "when",
        "for", "while", "until",
        "binary", // and, or, &&, ||
        "conditional",  // ternary
        "rescue", "ensure",
    ],
    decorator_node_kinds: &[],  // Ruby has no decorators (uses method calls instead)
    type_param_node_kind: None,  // Ruby has no generics
    docstring_in_body: false,
    route_queries: &[RUBY_ROUTE_QUERY],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: Some(("call", "method")),
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};

// ---------------------------------------------------------------------------
// Bash
// ---------------------------------------------------------------------------

pub static BASH_CONFIG: LangConfig = LangConfig {
    language_fn: || tree_sitter_bash::LANGUAGE.into(),
    language_name: "bash",
    extensions: &["sh", "bash"],
    node_kinds: &[
        ("function_definition", NodeKind::Function),
        // ALL_CAPS variable assignments handled in bash.rs
    ],
    scope_parent_kinds: &[],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string", None)],
    param_container_field: None,
    param_type_field: None,
    return_type_field: None,
    type_requires_uppercase: true,
    branch_node_types: &[
        "if_statement", "elif_clause", "else_clause",
        "case_statement", "case_item",
        "for_statement", "while_statement",
        "pipeline",  // pipes as control flow
        "binary_expression",  // && and ||
    ],
    decorator_node_kinds: &[],  // Bash has no decorators
    type_param_node_kind: None,  // Bash has no generics
    docstring_in_body: false,
    route_queries: &[],
    compiled_route_queries: std::sync::OnceLock::new(),
    call_expr_kinds: None,
    pub_visibility_modifier: None,
    has_all_export: false,
    test_name_prefix: false,
};
