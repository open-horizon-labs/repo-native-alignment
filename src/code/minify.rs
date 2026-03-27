use std::collections::HashMap;

/// Minify a function body for LLM consumption.
///
/// Phase 2: tree-sitter AST walk for supported languages.
/// - Strips comments
/// - Shortens local variable names (declarations + all references)
/// - Strips type annotations on local variables
/// - Collapses blank lines
/// - Emits a legend mapping short names to originals
///
/// Falls back to text-based minification for unsupported languages.
pub fn minify_body(body: &str, language: &str) -> String {
    if body.is_empty() {
        return String::new();
    }

    match language {
        "typescript" | "tsx" => minify_ts(body),
        "javascript" | "jsx" => minify_javascript(body),
        "rust" => minify_rust(body),
        "python" => minify_python(body),
        "go" => minify_go(body),
        _ => minify_text(body, language),
    }
}

// ---------------------------------------------------------------------------
// Tree-sitter minification for TypeScript/JavaScript
// ---------------------------------------------------------------------------

fn minify_ts(body: &str) -> String {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_typescript::LANGUAGE_TSX.into()).is_err() {
        return minify_text(body, "typescript");
    }
    minify_with_ast(&mut parser, body, "typescript", collect_ts_locals)
}

fn minify_javascript(body: &str) -> String {
    // Use the dedicated JavaScript parser for JS/JSX to avoid
    // subtle TSX-versus-JS ambiguity in edge cases.
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_javascript::LANGUAGE.into()).is_err() {
        return minify_text(body, "javascript");
    }
    minify_with_ast(&mut parser, body, "javascript", collect_ts_locals)
}

// Collect local variable declarations from tree-sitter AST.
fn collect_ts_locals(
    node: tree_sitter::Node,
    source: &[u8],
    locals: &mut HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
    used: &mut std::collections::HashSet<String>,
) {
    match node.kind() {
        "variable_declarator" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = name_node.utf8_text(source).unwrap_or("").to_string();
                // Only rename if it's a simple identifier (not destructuring)
                if name_node.kind() == "identifier" && !name.is_empty() && name.len() > 2 {
                    let short = if let Some(existing) = locals.get(&name) {
                        existing.clone()
                    } else {
                        let s = make_short_name(&name, used);
                        locals.insert(name.clone(), s.clone());
                        s
                    };
                    ranges.push((
                        name_node.start_byte(),
                        name_node.end_byte(),
                        short,
                    ));
                }
            }
        }
        "for_in_statement" | "for_statement" => {
            // for (const x of ...) or for (let i = ...)
            if let Some(left) = node.child_by_field_name("left") {
                collect_for_binding(left, source, locals, ranges, used);
            }
            if let Some(init) = node.child_by_field_name("initializer") {
                collect_ts_locals(init, source, locals, ranges, used);
            }
        }
        "catch_clause" => {
            if let Some(param) = node.child_by_field_name("parameter") {
                if param.kind() == "identifier" {
                    let name = param.utf8_text(source).unwrap_or("").to_string();
                    if name.len() > 2 {
                        let short = if let Some(existing) = locals.get(&name) {
                            existing.clone()
                        } else {
                            let s = make_short_name(&name, used);
                            locals.insert(name, s.clone());
                            s
                        };
                        ranges.push((param.start_byte(), param.end_byte(), short));
                    }
                }
            }
        }
        _ => {}
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            // Don't descend into nested function bodies — their locals are separate scope
            match child.kind() {
                "arrow_function" | "function_declaration" | "function_expression"
                | "method_definition" | "generator_function_declaration" => continue,
                _ => collect_ts_locals(child, source, locals, ranges, used),
            }
        }
    }
}

fn collect_for_binding(
    node: tree_sitter::Node,
    source: &[u8],
    locals: &mut HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
    used: &mut std::collections::HashSet<String>,
) {
    // Handle `const x` in `for (const x of ...)` — the node is a lexical_declaration
    if node.kind() == "lexical_declaration" || node.kind() == "variable_declaration" {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if child.kind() == "variable_declarator" {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = name_node.utf8_text(source).unwrap_or("").to_string();
                        if name_node.kind() == "identifier" && !name.is_empty() && name.len() > 2 {
                            let short = if let Some(existing) = locals.get(&name) {
                                existing.clone()
                            } else {
                                let s = make_short_name(&name, used);
                                locals.insert(name, s.clone());
                                s
                            };
                            ranges.push((name_node.start_byte(), name_node.end_byte(), short));
                        }
                    }
                }
            }
        }
    } else if node.kind() == "identifier" {
        let name = node.utf8_text(source).unwrap_or("").to_string();
        if name.len() > 2 {
            let short = if let Some(existing) = locals.get(&name) {
                existing.clone()
            } else {
                let s = make_short_name(&name, used);
                locals.insert(name, s.clone());
                s
            };
            ranges.push((node.start_byte(), node.end_byte(), short));
        }
    }
}




/// Generate a deterministic 3-char base-36 name for a variable.
/// Stable across files and projects — same original name always produces the same short name.
/// Uses a collision set to handle the rare case two names hash to the same output.
fn make_short_name(original: &str, used: &mut std::collections::HashSet<String>) -> String {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;

    const ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789"; // base-36 with letter-only first char: 26 * 36 * 36 = 33,696 slots

    let mut hasher = DefaultHasher::new();
    original.hash(&mut hasher);
    let h = hasher.finish();

    let encode = |n: u64| -> String {
        let mut chars = [0u8; 3];
        let mut v = n;
        // Last two chars: base-36 (letters + digits)
        for c in chars[1..].iter_mut().rev() {
            *c = ALPHA[(v % 36) as usize];
            v /= 36;
        }
        // First char: base-26 (letters only) so name never starts with a digit
        chars[0] = b'a' + (v % 26) as u8;
        std::str::from_utf8(&chars).unwrap().to_string()
    };

    let base = encode(h);
    if !used.contains(&base) {
        used.insert(base.clone());
        return base;
    }
    // Collision: rotate hash deterministically and retry
    for i in 1u64..=1000 {
        let candidate = encode(h.wrapping_add(i.wrapping_mul(46656)));
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
    }
    // Extremely unlikely fallback
    let fb = format!("{:03}", used.len() % 1000);
    used.insert(fb.clone());
    fb
}

/// Extract the inner body from our `function __wrapper__() { ... }` wrapper.
fn extract_wrapper_body(wrapped: &str) -> &str {
    // Find first `{` after `function __wrapper__()`
    if let Some(open) = wrapped.find("{\n") {
        let inner_start = open + 2; // skip `{\n`
        if let Some(close) = wrapped.rfind("\n}") {
            return &wrapped[inner_start..close];
        }
    }
    wrapped
}


fn build_legend(locals: &HashMap<String, String>) -> String {
    if locals.is_empty() {
        return String::new();
    }
    let mut entries: Vec<_> = locals.iter().map(|(orig, short)| (short.clone(), orig.clone())).collect();
    entries.sort();
    let parts: Vec<String> = entries.iter().map(|(short, orig)| format!("{}={}", short, orig)).collect();
    format!("// {}", parts.join(" "))
}

// ---------------------------------------------------------------------------
// Generic AST minification (shared by all tree-sitter-supported languages)
// ---------------------------------------------------------------------------

type LocalCollector = fn(
    node: tree_sitter::Node,
    source: &[u8],
    locals: &mut HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
    used: &mut std::collections::HashSet<String>,
);

/// Language-specific configuration for reference collection.
#[allow(dead_code)]
struct LangConfig {
    /// Node kinds that introduce new scope (don't descend for locals OR references)
    scope_boundaries: &'static [&'static str],
    /// Node kinds that capture outer scope (descend for references, not locals)
    closures: &'static [&'static str],
    /// Node kind for property access (e.g., "member_expression", "field_expression")
    member_expr: &'static str,
    /// Node kind for shorthand property identifiers (TS/JS only)
    shorthand_prop: &'static str,
    /// Comment line prefix for text cleanup
    comment_prefix: &'static str,
}

const TS_CONFIG: LangConfig = LangConfig {
    scope_boundaries: &["function_declaration", "function_expression", "method_definition", "generator_function_declaration"],
    closures: &["arrow_function"],
    member_expr: "member_expression",
    shorthand_prop: "shorthand_property_identifier",
    comment_prefix: "//",
};

const RUST_CONFIG: LangConfig = LangConfig {
    scope_boundaries: &["function_item"],
    closures: &["closure_expression"],
    member_expr: "field_expression",
    shorthand_prop: "shorthand_field_initializer",
    comment_prefix: "//",
};

const PYTHON_CONFIG: LangConfig = LangConfig {
    scope_boundaries: &["function_definition", "class_definition", "lambda"],
    closures: &[],
    member_expr: "attribute",
    shorthand_prop: "",
    comment_prefix: "#",
};

const GO_CONFIG: LangConfig = LangConfig {
    scope_boundaries: &["function_declaration", "method_declaration", "func_literal"],
    closures: &[],
    member_expr: "selector_expression",
    shorthand_prop: "",
    comment_prefix: "//",
};

fn lang_config(language: &str) -> &'static LangConfig {
    match language {
        "typescript" | "tsx" | "javascript" | "jsx" => &TS_CONFIG,
        "rust" => &RUST_CONFIG,
        "python" => &PYTHON_CONFIG,
        "go" => &GO_CONFIG,
        _ => &TS_CONFIG,
    }
}

/// Generic tree-sitter minification shared by all languages.
fn minify_with_ast(
    parser: &mut tree_sitter::Parser,
    body: &str,
    language: &str,
    collect_locals: LocalCollector,
) -> String {
    let config = lang_config(language);

    // Parse the body directly or wrap if needed
    let (source_text, tree, is_wrapped) = {
        if let Some(t) = parser.parse(body, None) {
            if !t.root_node().has_error() {
                (body.to_string(), t, false)
            } else {
                let wrapper = match language {
                    "rust" => format!("fn __wrapper__() {{\n{}\n}}", body),
                    "python" => format!("def __wrapper__():\n{}", indent_body(body, "    ")),
                    "go" => format!("func __wrapper__() {{\n{}\n}}", body),
                    _ => format!("function __wrapper__() {{\n{}\n}}", body),
                };
                match parser.parse(&wrapper, None) {
                    Some(t2) => (wrapper, t2, true),
                    None => return minify_text(body, language),
                }
            }
        } else {
            return minify_text(body, language);
        }
    };

    let source = source_text.as_bytes();
    let root = tree.root_node();

    // Find function body to scope local collection
    let body_node = find_function_body_generic(root, language).unwrap_or(root);

    let mut locals: HashMap<String, String> = HashMap::new();
    let mut rename_ranges: Vec<(usize, usize, String)> = Vec::new();
    let mut used_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    collect_locals(body_node, source, &mut locals, &mut rename_ranges, &mut used_names);
    collect_references(body_node, source, &locals, &mut rename_ranges, config);

    // Sort descending by start, deduplicate
    rename_ranges.sort_by(|a, b| b.0.cmp(&a.0));
    rename_ranges.dedup_by_key(|r| r.0);

    let mut result = source_text.clone();
    for (start, end, short) in &rename_ranges {
        result = format!("{}{}{}", &result[..*start], short, &result[*end..]);
    }

    let inner = if is_wrapped {
        match language {
            "python" => extract_python_wrapper_body(&result),
            _ => extract_wrapper_body(&result),
        }
    } else {
        result.as_str()
    };

    let cleaned = minify_text(inner, language);
    let legend = build_legend(&locals);
    if legend.is_empty() {
        cleaned
    } else {
        let prefix = if config.comment_prefix == "#" { "# " } else { "// " };
        let legend_line = format!("{}{}", prefix, legend.trim_start_matches("// "));
        format!("{}\n{}", cleaned, legend_line)
    }
}

/// Find function body for any supported language.
fn find_function_body_generic<'a>(root: tree_sitter::Node<'a>, language: &str) -> Option<tree_sitter::Node<'a>> {
    fn find_body<'a>(node: tree_sitter::Node<'a>, fn_kinds: &[&str]) -> Option<tree_sitter::Node<'a>> {
        if fn_kinds.contains(&node.kind()) {
            return node.child_by_field_name("body");
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                if let Some(body) = find_body(child, fn_kinds) {
                    return Some(body);
                }
            }
        }
        None
    }
    let fn_kinds: &[&str] = match language {
        "rust" => &["function_item"],
        "python" => &["function_definition"],
        "go" => &["function_declaration", "method_declaration"],
        _ => &["function_declaration", "function_expression", "method_definition",
               "arrow_function", "generator_function_declaration"],
    };
    find_body(root, fn_kinds)
}

/// Generic reference collector, parameterized by language config.
fn collect_references(
    node: tree_sitter::Node,
    source: &[u8],
    locals: &HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
    config: &LangConfig,
) {
    if node.kind() == "identifier" || (!config.shorthand_prop.is_empty() && node.kind() == config.shorthand_prop) {
        let text = node.utf8_text(source).unwrap_or("");
        if let Some(short) = locals.get(text) {
            if let Some(parent) = node.parent() {
                // Don't rename property access targets
                if parent.kind() == config.member_expr {
                    if let Some(prop) = parent.child_by_field_name(if config.member_expr == "attribute" { "attribute" } else { "property" }) {
                        if prop.id() == node.id() {
                            return;
                        }
                    }
                    // Rust field_expression: field name is "field" not "property"
                    if config.member_expr == "field_expression" {
                        if let Some(field) = parent.child_by_field_name("field") {
                            if field.id() == node.id() {
                                return;
                            }
                        }
                    }
                }
                // Don't rename object/struct literal keys
                if parent.kind() == "pair" || parent.kind() == "field_initializer" {
                    if let Some(key) = parent.child_by_field_name(if parent.kind() == "field_initializer" { "field" } else { "key" }) {
                        if key.id() == node.id() {
                            return;
                        }
                    }
                }
                // Don't rename imports
                if parent.kind().contains("import") {
                    return;
                }
            }
            // Expand shorthand properties to preserve object shape
            if !config.shorthand_prop.is_empty() && node.kind() == config.shorthand_prop {
                ranges.push((node.start_byte(), node.end_byte(), format!("{}: {}", text, short)));
            } else {
                ranges.push((node.start_byte(), node.end_byte(), short.clone()));
            }
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            let kind = child.kind();
            // Skip scope boundaries entirely (new scopes with their own locals)
            if config.scope_boundaries.contains(&kind) {
                continue;
            }
            // Descend into closures for references (they capture outer scope)
            // but NOT for local declarations
            // (closures fall through here and get recursed into, which is correct)
            collect_references(child, source, locals, ranges, config);
        }
    }
}

/// Helper to register a local variable with dedup handling.
fn register_local(
    name: &str,
    name_node: tree_sitter::Node,
    locals: &mut HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
    used: &mut std::collections::HashSet<String>,
) {
    if name.len() <= 2 || name.is_empty() {
        return;
    }
    let short = if let Some(existing) = locals.get(name) {
        existing.clone()
    } else {
        let s = make_short_name(name, used);
        locals.insert(name.to_string(), s.clone());
        s
    };
    ranges.push((name_node.start_byte(), name_node.end_byte(), short));
}

/// Indent a body for Python wrapping.
fn indent_body(body: &str, indent: &str) -> String {
    body.lines().map(|l| format!("{}{}", indent, l)).collect::<Vec<_>>().join("\n")
}

/// Extract body from Python `def __wrapper__():` wrapper.
fn extract_python_wrapper_body(wrapped: &str) -> &str {
    // Skip first line (def __wrapper__():) and unindent
    if let Some(pos) = wrapped.find('\n') {
        &wrapped[pos + 1..]
    } else {
        wrapped
    }
}

// ---------------------------------------------------------------------------
// Per-language local variable collectors
// ---------------------------------------------------------------------------

/// Rust: let bindings, for loops
fn collect_rust_locals(
    node: tree_sitter::Node,
    source: &[u8],
    locals: &mut HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
    used: &mut std::collections::HashSet<String>,
) {
    match node.kind() {
        "let_declaration" => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                collect_rust_pattern_bindings(pattern, source, locals, ranges, used);
            }
        }
        "for_expression" => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                collect_rust_pattern_bindings(pattern, source, locals, ranges, used);
            }
        }
        "if_let_expression" | "while_let_expression" => {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                collect_rust_pattern_bindings(pattern, source, locals, ranges, used);
            }
        }
        _ => {}
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == "function_item" || child.kind() == "closure_expression" {
                continue;
            }
            collect_rust_locals(child, source, locals, ranges, used);
        }
    }
}

/// Recursively extract identifiers from Rust patterns (handles destructuring).
fn collect_rust_pattern_bindings(
    node: tree_sitter::Node,
    source: &[u8],
    locals: &mut HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
    used: &mut std::collections::HashSet<String>,
) {
    if node.kind() == "identifier" {
        let name = node.utf8_text(source).unwrap_or("");
        if name != "_" { register_local(name, node, locals, ranges, used); }
    } else if node.kind() == "tuple_pattern" || node.kind() == "slice_pattern" {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                collect_rust_pattern_bindings(child, source, locals, ranges, used);
            }
        }
    } else if node.kind() == "tuple_struct_pattern" || node.kind() == "struct_pattern" {
        // e.g., Some(x) or Point { x, y }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                collect_rust_pattern_bindings(child, source, locals, ranges, used);
            }
        }
    } else if node.kind() == "ref_pattern" || node.kind() == "mut_pattern" {
        // ref x, mut x
        if let Some(pattern) = node.child_by_field_name("pattern") {
            collect_rust_pattern_bindings(pattern, source, locals, ranges, used);
        } else {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    collect_rust_pattern_bindings(child, source, locals, ranges, used);
                }
            }
        }
    }
}

/// Python: assignments, for loops, with statements
fn collect_python_locals(
    node: tree_sitter::Node,
    source: &[u8],
    locals: &mut HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
    used: &mut std::collections::HashSet<String>,
) {
    match node.kind() {
        "assignment" => {
            if let Some(left) = node.child_by_field_name("left") {
                collect_python_targets(left, source, locals, ranges, used);
            }
        }
        "augmented_assignment" => {
            if let Some(left) = node.child_by_field_name("left") {
                collect_python_targets(left, source, locals, ranges, used);
            }
        }
        "for_statement" => {
            if let Some(left) = node.child_by_field_name("left") {
                collect_python_targets(left, source, locals, ranges, used);
            }
        }
        "with_statement" => {
            // with expr as name:
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if child.kind() == "as_pattern" {
                        if let Some(alias) = child.child_by_field_name("alias") {
                            collect_python_targets(alias, source, locals, ranges, used);
                        }
                    }
                }
            }
        }
        "named_expression" => {
            // walrus operator: name := expr
            if let Some(name) = node.child_by_field_name("name") {
                if name.kind() == "identifier" {
                    let text = name.utf8_text(source).unwrap_or("");
                    register_local(text, name, locals, ranges, used);
                }
            }
        }
        _ => {}
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == "function_definition" || child.kind() == "class_definition" || child.kind() == "lambda" {
                continue;
            }
            collect_python_locals(child, source, locals, ranges, used);
        }
    }
}

/// Recursively extract identifiers from Python assignment targets.
fn collect_python_targets(
    node: tree_sitter::Node,
    source: &[u8],
    locals: &mut HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
    used: &mut std::collections::HashSet<String>,
) {
    if node.kind() == "identifier" {
        let name = node.utf8_text(source).unwrap_or("");
        // Skip self, cls, common Python conventions
        if name != "self" && name != "cls" && name != "_" {
            register_local(name, node, locals, ranges, used);
        }
    } else if node.kind() == "tuple" || node.kind() == "list" || node.kind() == "pattern_list" || node.kind() == "tuple_pattern" {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                collect_python_targets(child, source, locals, ranges, used);
            }
        }
    }
    // Skip attribute access (self.x = ...) — don't rename
}

/// Go: short var declarations, var specs, range clauses
fn collect_go_locals(
    node: tree_sitter::Node,
    source: &[u8],
    locals: &mut HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
    used: &mut std::collections::HashSet<String>,
) {
    match node.kind() {
        "short_var_declaration" => {
            // x, y := expr
            if let Some(left) = node.child_by_field_name("left") {
                collect_go_id_list(left, source, locals, ranges, used);
            }
        }
        "var_declaration" => {
            // var x int = ...
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if child.kind() == "var_spec" {
                        if let Some(name) = child.child_by_field_name("name") {
                            collect_go_id_list(name, source, locals, ranges, used);
                        }
                    }
                }
            }
        }
        "range_clause" => {
            // for k, v := range ...
            if let Some(left) = node.child_by_field_name("left") {
                collect_go_id_list(left, source, locals, ranges, used);
            }
        }
        _ => {}
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == "function_declaration" || child.kind() == "method_declaration"
                || child.kind() == "func_literal" {
                continue;
            }
            collect_go_locals(child, source, locals, ranges, used);
        }
    }
}

/// Extract identifiers from Go expression_list (left side of :=, etc.).
fn collect_go_id_list(
    node: tree_sitter::Node,
    source: &[u8],
    locals: &mut HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
    used: &mut std::collections::HashSet<String>,
) {
    if node.kind() == "identifier" {
        let name = node.utf8_text(source).unwrap_or("");
        if name != "_" && name != "err" { // keep common Go conventions short
            register_local(name, node, locals, ranges, used);
        }
    } else if node.kind() == "expression_list" {
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                collect_go_id_list(child, source, locals, ranges, used);
            }
        }
    }
}


// ---------------------------------------------------------------------------
// Rust minification (simpler — no type stripping since types are meaningful)
// ---------------------------------------------------------------------------

fn minify_rust(body: &str) -> String {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_rust::LANGUAGE.into()).is_err() {
        return minify_text(body, "rust");
    }
    minify_with_ast(&mut parser, body, "rust", collect_rust_locals)
}

// ---------------------------------------------------------------------------
// Python minification
// ---------------------------------------------------------------------------

fn minify_python(body: &str) -> String {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_python::LANGUAGE.into()).is_err() {
        return minify_text(body, "python");
    }
    minify_with_ast(&mut parser, body, "python", collect_python_locals)
}

// ---------------------------------------------------------------------------
// Go minification
// ---------------------------------------------------------------------------

fn minify_go(body: &str) -> String {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&tree_sitter_go::LANGUAGE.into()).is_err() {
        return minify_text(body, "go");
    }
    minify_with_ast(&mut parser, body, "go", collect_go_locals)
}

// ---------------------------------------------------------------------------
// Text-based fallback (phase 1 logic, retained)
// ---------------------------------------------------------------------------

fn minify_text(body: &str, language: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let mut result: Vec<String> = Vec::new();
    let mut in_block_comment = false;
    let mut prev_blank = false;

    let line_comment_prefix = match language {
        "python" => "#",
        _ => "//",
    };

    for line in &lines {
        let mut line = line.to_string();

        if in_block_comment {
            if let Some(end) = line.find("*/") {
                line = line[end + 2..].to_string();
                in_block_comment = false;
            } else {
                continue;
            }
        }

        while let Some(start) = line.find("/*") {
            if let Some(end) = line[start..].find("*/") {
                line = format!("{}{}", &line[..start], &line[start + end + 2..]);
            } else {
                line = line[..start].to_string();
                in_block_comment = true;
                break;
            }
        }

        if let Some(pos) = find_line_comment(&line, line_comment_prefix) {
            line = line[..pos].to_string();
        }

        let trimmed = line.trim_end().to_string();

        if trimmed.is_empty() {
            if prev_blank {
                continue;
            }
            prev_blank = true;
        } else {
            prev_blank = false;
        }

        result.push(trimmed);
    }

    let start = result.iter().position(|s| !s.is_empty()).unwrap_or(result.len());
    let end = result
        .iter()
        .rposition(|s| !s.is_empty())
        .map(|i| i + 1)
        .unwrap_or(0);
    if start >= end {
        result.clear();
    } else {
        result.drain(0..start);
        result.truncate(end - start);
    }

    result.join("\n")
}

/// Find position of a line comment, avoiding false positives in strings and URLs.
fn find_line_comment(line: &str, prefix: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let prefix_bytes = prefix.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut string_char: u8 = 0;

    while i < bytes.len() {
        if in_string {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == string_char {
                in_string = false;
            }
            i += 1;
            continue;
        }

        if bytes[i] == b'"' || bytes[i] == b'\'' || bytes[i] == b'`' {
            in_string = true;
            string_char = bytes[i];
            i += 1;
            continue;
        }

        if i + prefix_bytes.len() <= bytes.len()
            && &bytes[i..i + prefix_bytes.len()] == prefix_bytes
        {
            if prefix == "//" && i > 0 && bytes[i - 1] == b':' {
                i += 2;
                continue;
            }
            return Some(i);
        }

        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Text-based fallback tests (retained from phase 1) --

    #[test]
    fn test_strips_line_comments() {
        let input = "let x = 1; // set x\nlet y = 2;";
        let result = minify_body(input, "typescript");
        assert!(!result.contains("set x"));
        assert!(result.contains("= 1;"));
        assert!(result.contains("= 2;"));
    }

    #[test]
    fn test_preserves_urls() {
        let input = "let url = \"https://example.com\";";
        let result = minify_body(input, "typescript");
        assert!(result.contains("https://example.com"));
    }

    #[test]
    fn test_strips_block_comments() {
        let input = "let x = 1;\n/* this is\na comment */\nlet y = 2;";
        let result = minify_body(input, "typescript");
        assert!(result.contains("= 1;"));
        assert!(result.contains("= 2;"));
        assert!(!result.contains("comment"));
    }

    #[test]
    fn test_collapses_blank_lines() {
        let input = "let x = 1;\n\n\n\nlet y = 2;";
        let result = minify_body(input, "typescript");
        // Should have at most one blank line between
        assert!(!result.contains("\n\n\n"));
    }

    #[test]
    fn test_strips_python_comments() {
        let input = "x = 1  # set x\ny = 2";
        let result = minify_body(input, "python");
        assert_eq!(result, "x = 1\ny = 2");
    }

    #[test]
    fn test_preserves_string_with_comment_chars() {
        let input = "let s = \"hello // world\";";
        let result = minify_body(input, "typescript");
        assert!(result.contains("hello // world"));
    }

    // -- Tree-sitter variable renaming tests --

    #[test]
    fn test_renames_long_locals() {
        let input = "const userCount = 5;\nconst result = userCount + 1;\nreturn result;";
        let result = minify_body(input, "typescript");
        // Code lines should use short names, not originals
        let code_lines: Vec<&str> = result.lines().filter(|l| !l.starts_with("// ")).collect();
        let code = code_lines.join("\n");
        assert!(!code.contains("userCount"), "userCount should be renamed in code: {}", code);
        // Legend should map short names back to originals
        let legend = result.lines().find(|l| l.starts_with("// ")).expect("should have legend");
        assert!(legend.contains("userCount"), "legend should mention userCount: {}", legend);
    }

    #[test]
    fn test_preserves_short_names() {
        let input = "const x = 5;\nreturn x;";
        let result = minify_body(input, "typescript");
        // Short names (<=2 chars) should NOT be renamed
        assert!(result.contains("x"));
        assert!(!result.contains("// ")); // no legend needed
    }


    #[test]
    fn test_preserves_property_access() {
        let input = "const result = obj.propertyName;\nreturn result;";
        let result = minify_body(input, "typescript");
        // Property names should NOT be renamed
        assert!(result.contains(".propertyName"));
    }

    #[test]
    fn test_preserves_function_calls() {
        let input = "const data = fetchUserById(id);\nreturn data;";
        let result = minify_body(input, "typescript");
        // Function names should be preserved
        assert!(result.contains("fetchUserById"));
    }

    #[test]
    fn test_empty_body() {
        assert_eq!(minify_body("", "typescript"), "");
    }

    #[test]
    fn test_legend_format() {
        let input = "const longName = 1;\nreturn longName;";
        let result = minify_body(input, "typescript");
        // Legend should be on the last line
        let last_line = result.lines().last().unwrap();
        assert!(last_line.starts_with("// "));
        assert!(last_line.contains("longName"));
    }
}
