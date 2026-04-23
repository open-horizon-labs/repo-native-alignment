//! Shared `Node.metadata` key constants used by the LanceDB writer and loader.
//!
//! Every typed metadata column in [`crate::graph::store::symbols_schema`]
//! corresponds to a single metadata key. When the writer reads a key and the
//! loader writes the same key back, a single typo silently drops the field
//! across a round-trip. Keeping the key names here ensures the writer and
//! loader cannot drift apart at compile time.
//!
//! Arrow column names and metadata key names are not always identical
//! (e.g. column `meta_virtual` ↔ metadata key `virtual`,
//! column `rpc_request_type` ↔ metadata key `request_type`). This module
//! holds the metadata-key side only; column names stay in the schema.

// ── Core typed-metadata keys ────────────────────────────────────────────

pub const VIRTUAL: &str = "virtual";
pub const PACKAGE: &str = "package";
pub const NAME_COL: &str = "name_col";
pub const VALUE: &str = "value";
pub const SYNTHETIC: &str = "synthetic";
pub const CYCLOMATIC: &str = "cyclomatic";
pub const IMPORTANCE: &str = "importance";
pub const STORAGE: &str = "storage";
pub const MUTABLE: &str = "mutable";
pub const DECORATORS: &str = "decorators";
pub const PARENT_SCOPE: &str = "parent_scope";
pub const PARENT_SCOPE_KIND: &str = "parent_scope_kind";
pub const FRAMEWORK_HOOK: &str = "framework_hook";
pub const TYPE_PARAMS: &str = "type_params";
pub const PATTERN_HINT: &str = "pattern_hint";
pub const IS_STATIC: &str = "is_static";
pub const IS_ASYNC: &str = "is_async";
pub const IS_TEST: &str = "is_test";
pub const VISIBILITY: &str = "visibility";
pub const EXPORTED: &str = "exported";
pub const DOC_COMMENT: &str = "doc_comment";
pub const ATTR_REFS: &str = "attr_refs";

// ── Diagnostic columns (NodeKind::Other("diagnostic")) ─────────────────

pub const DIAG_SEVERITY: &str = "diagnostic_severity";
pub const DIAG_SOURCE: &str = "diagnostic_source";
pub const DIAG_MESSAGE: &str = "diagnostic_message";
pub const DIAG_RANGE: &str = "diagnostic_range";
pub const DIAG_TIMESTAMP: &str = "diagnostic_timestamp";

// ── ApiEndpoint columns (NodeKind::ApiEndpoint) ────────────────────────

pub const HTTP_METHOD: &str = "http_method";
pub const HTTP_PATH: &str = "http_path";

// ── gRPC / proto columns (#466) ────────────────────────────────────────
// Note: metadata keys below differ from their Arrow column names
// (`rpc_request_type` / `rpc_response_type`). See module docstring.

pub const PARENT_SERVICE: &str = "parent_service";
pub const REQUEST_TYPE: &str = "request_type";
pub const RESPONSE_TYPE: &str = "response_type";
