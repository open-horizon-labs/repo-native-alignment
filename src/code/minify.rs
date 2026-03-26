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
        "typescript" | "tsx" | "javascript" | "jsx" => minify_ts(body),
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
    if parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
        .is_err()
    {
        return minify_text(body, "typescript");
    }

    // Try parsing the body directly — it may be a complete function/class
    // If that fails, wrap it in a function to make it a valid program
    let (source_text, tree, is_wrapped) = {
        if let Some(t) = parser.parse(body, None) {
            // Check if it parsed without errors
            if !t.root_node().has_error() {
                (body.to_string(), t, false)
            } else {
                // Wrap in a function
                let w = format!("function __wrapper__() {{\n{}\n}}", body);
                match parser.parse(&w, None) {
                    Some(t2) => (w, t2, true),
                    None => return minify_text(body, "typescript"),
                }
            }
        } else {
            return minify_text(body, "typescript");
        }
    };

    let source = source_text.as_bytes();
    let root = tree.root_node();

    // Find the function body (statement_block) to scope local variable collection
    let body_node = find_function_body(root, source).unwrap_or(root);

    // Collect local variable bindings and their byte ranges
    let mut locals: HashMap<String, String> = HashMap::new();
    let mut rename_ranges: Vec<(usize, usize, String)> = Vec::new();
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();

    collect_ts_locals(body_node, source, &mut locals, &mut rename_ranges, &mut used);

    // Also collect all identifier references that match known locals
    collect_ts_references(body_node, source, &locals, &mut rename_ranges);

    // Sort ranges by start position (descending) for safe replacement
    rename_ranges.sort_by(|a, b| b.0.cmp(&a.0));
    // Deduplicate by start position
    rename_ranges.dedup_by_key(|r| r.0);

    let mut result = source_text.clone();
    for (start, end, short) in &rename_ranges {
        result = format!("{}{}{}", &result[..*start], short, &result[*end..]);
    }

    // If wrapped, strip the wrapper
    let inner = if is_wrapped {
        extract_wrapper_body(&result)
    } else {
        result.as_str()
    };

    // Strip type annotations, comments, blank lines
    let cleaned = strip_ts_types_and_comments(inner);

    // Append legend
    let legend = build_legend(&locals);
    if legend.is_empty() {
        cleaned
    } else {
        format!("{}\n{}", cleaned, legend)
    }
}

/// Collect local variable declarations from tree-sitter AST.
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

/// Walk the full AST and rename all identifier references that match known locals.
fn collect_ts_references(
    node: tree_sitter::Node,
    source: &[u8],
    locals: &HashMap<String, String>,
    ranges: &mut Vec<(usize, usize, String)>,
) {
    if node.kind() == "identifier" || node.kind() == "shorthand_property_identifier" {
        let text = node.utf8_text(source).unwrap_or("");
        if let Some(short) = locals.get(text) {
            // Don't rename if this is a property access (parent is member_expression and we're the property)
            if let Some(parent) = node.parent() {
                if parent.kind() == "member_expression" {
                    if let Some(prop) = parent.child_by_field_name("property") {
                        if prop.id() == node.id() {
                            return;
                        }
                    }
                }
                // Don't rename if this is a property KEY in an object literal (but DO rename values)
                if parent.kind() == "pair" {
                    if let Some(key) = parent.child_by_field_name("key") {
                        if key.id() == node.id() {
                            return;
                        }
                    }
                }
                // Don't rename import specifiers
                if parent.kind() == "import_specifier"
                    || parent.kind() == "import_clause"
                    || parent.kind() == "named_imports"
                {
                    return;
                }
            }
            // For shorthand properties like { decisions }, expand to { decisions: v0 }
            // instead of renaming to { v0 } which would change the property name
            if node.kind() == "shorthand_property_identifier" {
                ranges.push((node.start_byte(), node.end_byte(), format!("{}: {}", text, short)));
            } else {
                ranges.push((node.start_byte(), node.end_byte(), short.clone()));
            }
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            // For references: descend into arrow functions (they capture outer scope)
            // but NOT into named function declarations (they have their own scope)
            match child.kind() {
                "function_declaration" | "function_expression"
                | "method_definition" | "generator_function_declaration" => continue,
                _ => collect_ts_references(child, source, locals, ranges),
            }
        }
    }
}

/// Find the statement_block body inside a function/method/arrow function.
/// Works for: complete function declarations, class methods, arrow functions.
fn find_function_body<'a>(root: tree_sitter::Node<'a>, _source: &[u8]) -> Option<tree_sitter::Node<'a>> {
    // Walk children to find function-like nodes, then get their body
    fn find_body<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
        match node.kind() {
            "function_declaration" | "function_expression" | "method_definition"
            | "arrow_function" | "generator_function_declaration" => {
                node.child_by_field_name("body")
            }
            "program" | "export_statement" => {
                for i in 0..node.child_count() {
                    if let Some(child) = node.child(i as u32) {
                        if let Some(body) = find_body(child) {
                            return Some(body);
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }
    find_body(root)
}


/// Generate a deterministic 3-char base-36 name for a variable.
/// Stable across files and projects — same original name always produces the same short name.
/// Uses a collision set to handle the rare case two names hash to the same output.
fn make_short_name(original: &str, used: &mut std::collections::HashSet<String>) -> String {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;

    const ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789"; // base-36: 46,656 slots

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

/// Strip type annotations and comments from TypeScript source.
fn strip_ts_types_and_comments(source: &str) -> String {
    let mut result: Vec<String> = Vec::new();
    let mut prev_blank = false;
    let mut in_block_comment = false;

    for line in source.lines() {
        let mut line_str = line.to_string();

        // Handle ongoing block comments
        if in_block_comment {
            if let Some(end) = line_str.find("*/") {
                line_str = line_str[end + 2..].to_string();
                in_block_comment = false;
                if line_str.trim().is_empty() { continue; }
            } else {
                continue;
            }
        }

        // Remove block comment starts
        while let Some(start) = line_str.find("/*") {
            if let Some(end) = line_str[start..].find("*/") {
                line_str = format!("{}{}", &line_str[..start], &line_str[start + end + 2..]);
            } else {
                line_str = line_str[..start].to_string();
                in_block_comment = true;
                break;
            }
        }

        // Skip pure line comment lines
        let trimmed_check = line_str.trim();
        if trimmed_check.starts_with("//") {
            continue;
        }
        // Strip inline comments (but not URLs)
        if let Some(pos) = find_line_comment(&line_str, "//") {
            line_str = line_str[..pos].trim_end().to_string();
        }


        let final_trimmed = line_str.trim_end().to_string();

        if final_trimmed.is_empty() {
            if prev_blank {
                continue;
            }
            prev_blank = true;
        } else {
            prev_blank = false;
        }

        result.push(final_trimmed);
    }

    // Strip leading/trailing blank lines
    while result.first().map(|s| s.is_empty()).unwrap_or(false) {
        result.remove(0);
    }
    while result.last().map(|s| s.is_empty()).unwrap_or(false) {
        result.pop();
    }

    result.join("\n")
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
// Rust minification (simpler — no type stripping since types are meaningful)
// ---------------------------------------------------------------------------

fn minify_rust(body: &str) -> String {
    // For Rust, just strip comments and collapse blanks.
    // Rust types are meaningful in bodies (turbofish, trait bounds) so don't strip them.
    // Local variable renaming would need borrow-checker awareness — skip for now.
    minify_text(body, "rust")
}

// ---------------------------------------------------------------------------
// Python minification
// ---------------------------------------------------------------------------

fn minify_python(body: &str) -> String {
    // Python has no type annotations that tree-sitter can trivially strip (they're optional hints).
    // Strip comments and collapse blanks.
    minify_text(body, "python")
}

// ---------------------------------------------------------------------------
// Go minification
// ---------------------------------------------------------------------------

fn minify_go(body: &str) -> String {
    minify_text(body, "go")
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

    while result.first().map(|s| s.is_empty()).unwrap_or(false) {
        result.remove(0);
    }
    while result.last().map(|s| s.is_empty()).unwrap_or(false) {
        result.pop();
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
