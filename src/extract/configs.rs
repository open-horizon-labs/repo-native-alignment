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
        // enum variants handled as special case in typescript.rs (TS uses
        // property_identifier / enum_assignment, not a dedicated enum_member node type)
        // module-level const handled as special case in typescript.rs
    ],
    scope_parent_kinds: &["class_declaration", "enum_declaration"],
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
    scope_parent_kinds: &["class_declaration", "record_declaration", "enum_declaration"],
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
    scope_parent_kinds: &["class_declaration", "struct_declaration", "record_declaration", "enum_declaration"],
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
    scope_parent_kinds: &["class_declaration", "struct_declaration", "enum_declaration"],
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
};
