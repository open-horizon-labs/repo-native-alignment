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
        // module-level const handled as special case in typescript.rs
    ],
    scope_parent_kinds: &["class_declaration"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string", Some("string_fragment"))],
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
};

// ---------------------------------------------------------------------------
// Go — thin config; multi-name const and receiver handled in go.rs
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
        // static final consts handled in java.rs (text inspection)
    ],
    scope_parent_kinds: &["class_declaration", "record_declaration", "enum_declaration"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string_literal", None)],
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
        // const val / companion object consts handled in kotlin.rs
    ],
    scope_parent_kinds: &["class_declaration", "object_declaration"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string_literal", Some("string_content"))],
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
        // const fields handled in csharp.rs (text inspection)
    ],
    scope_parent_kinds: &["class_declaration", "struct_declaration", "record_declaration"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string_literal", None)],
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
    ],
    scope_parent_kinds: &["class_declaration", "struct_declaration", "enum_declaration"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string_literal", Some("string_literal_segment"))],
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
        ("field_declaration",    NodeKind::Field),
        // constexpr / static const handled in cpp.rs (text inspection)
    ],
    scope_parent_kinds: &["class_specifier", "struct_specifier", "enum_specifier"],
    const_value_field: None,
    full_text_name_kinds: &[],
    string_literal_kinds: &[("string_literal", Some("string_content"))],
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
};
