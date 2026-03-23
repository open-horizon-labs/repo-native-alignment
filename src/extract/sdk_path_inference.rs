//! Post-extraction pass that infers full HTTP paths for FastAPI `ApiEndpoint` nodes
//! by matching local paths against full URL paths found in generated SDK files.
//!
//! # Problem
//!
//! FastAPI applications with chained routers — where sub-routers are included
//! into parent routers without an explicit `prefix=` argument — produce
//! `ApiEndpoint` nodes with only the *local* route path:
//!
//! ```python
//! # workspaces/id/expertunities.py
//! expertunities_router = APIRouter()          # no prefix
//!
//! @expertunities_router.get("/expertunities") # local path only
//! def get_expertunities(): ...
//!
//! # workspaces/id/main.py
//! workspaces_id_router = APIRouter(prefix="/{id}")
//! workspaces_id_router.include_router(expertunities_router)  # no prefix kwarg
//!
//! # workspaces/main.py
//! workspaces_router = APIRouter(prefix="/workspaces")
//! workspaces_router.include_router(workspaces_id_router)     # no prefix kwarg
//! ```
//!
//! The chain `expertunities_router` → `workspaces_id_router` → `workspaces_router`
//! results in the final path `/workspaces/{id}/expertunities`, but the extracted
//! `http_path` is just `/expertunities`.  Neither [`fastapi_router_prefix_pass`]
//! (which requires an explicit `prefix=` argument) nor the router-tree walk can
//! resolve multi-hop chains without re-implementing Python's dynamic scoping.
//!
//! # Solution: SDK path as authoritative source
//!
//! The OpenAPI-generated SDK (`sdk.gen.ts` and equivalents) contains the *full*
//! server URL for every operation as a string literal (e.g.
//! `'/workspaces/{id}/expertunities'`).  RNA's `string_literals.rs` already
//! extracts these as synthetic `Const` nodes whose `name` is the path string.
//!
//! This pass:
//! 1. Collects all `Const` nodes from detected SDK files where `name` starts with `/`
//!    (i.e., full URL paths extracted by the string-literal harvester).
//! 2. For each Python `ApiEndpoint` whose `http_path` has not yet been fully
//!    resolved (determined by checking `http_path_local` absence — meaning
//!    [`fastapi_router_prefix_pass`] has not already patched it), checks whether
//!    any SDK path ends with the local path.
//! 3. When exactly one SDK path matches, updates `http_path`, `id.name`, and
//!    `signature` to the full path (reusing [`apply_full_path`] semantics).
//!    Ambiguous matches (multiple SDK paths share the same suffix) are skipped
//!    to avoid false positives.
//!
//! # Relationship to `fastapi_router_prefix_pass`
//!
//! The two passes are complementary:
//! - [`fastapi_router_prefix_pass`] handles explicit `prefix=` args (patterns 1 & 2).
//! - [`sdk_path_inference_pass`] handles multi-hop chained routers where **no**
//!   explicit `prefix=` arg exists at any hop.  It runs **after**
//!   [`fastapi_router_prefix_pass`] so that already-resolved paths (those with
//!   `http_path_local` set) are not re-processed.
//!
//! # When to apply
//!
//! An endpoint qualifies for SDK-based path inference when:
//! - It is a Python `ApiEndpoint` node.
//! - Its `http_path` metadata looks like a *local* path — either it has no `/`
//!   beyond the leading slash (e.g. `/expertunities`), or `http_path_local` is
//!   absent (not yet patched by `fastapi_router_prefix_pass`).
//! - At least one SDK `Const` path ends with the local path.
//!
//! # Safety
//!
//! - Only *unambiguous* matches (exactly one SDK path suffix-matches the local
//!   path) produce an update.
//! - The local path must be a proper suffix: the character *before* the suffix
//!   in the full path must be `/`, preventing `/foo` from matching `/barfoo`.
//! - Already-patched endpoints (`http_path_local` present) are skipped.
//! - Non-Python nodes are skipped.
//! - Nodes without `http_path` metadata are skipped.
//!
//! [`fastapi_router_prefix_pass`]: super::fastapi_router_prefix::fastapi_router_prefix_pass

use crate::graph::{Node, NodeKind};
use crate::extract::openapi_sdk_link::is_generated_sdk_file_pub;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run the SDK path inference pass.
///
/// Mutates `nodes` in place: for each Python `ApiEndpoint` node whose
/// `http_path` is still a local (short) path, attempts to infer the full
/// path from `Const` nodes in generated SDK files.
///
/// # Arguments
///
/// * `nodes` — the full node slice (only unpatched Python `ApiEndpoint` nodes
///   are considered for modification)
///
/// # When to call
///
/// Call after [`fastapi_router_prefix_pass`] so that endpoints already
/// resolved by the prefix pass are excluded (they have `http_path_local` set).
///
/// [`fastapi_router_prefix_pass`]: super::fastapi_router_prefix::fastapi_router_prefix_pass
pub fn sdk_path_inference_pass(nodes: &mut [Node]) {
    // Phase 1: Collect all URL paths from Const nodes in generated SDK files.
    // The string-literal harvester stores the path as the node `name`.
    //
    // These are the authoritative full paths from the OpenAPI-generated SDK.
    let sdk_paths: Vec<String> = nodes
        .iter()
        .filter(|n| {
            n.id.kind == NodeKind::Const
                && is_generated_sdk_file_pub(&n.id.file)
                && n.id.name.starts_with('/')
                && n.id.name.len() > 1
        })
        .map(|n| n.id.name.clone())
        .collect();

    if sdk_paths.is_empty() {
        return;
    }

    // Phase 2: Normalise SDK paths for suffix matching.
    // Keep the list deduplicated (multiple Const nodes for the same path can
    // appear if the SDK references it in both a one-liner and an object form).
    let mut unique_sdk_paths = sdk_paths;
    unique_sdk_paths.sort_unstable();
    unique_sdk_paths.dedup();

    tracing::debug!(
        "sdk_path_inference: {} unique SDK URL paths available for suffix matching",
        unique_sdk_paths.len(),
    );

    // Phase 3: Update ApiEndpoint nodes whose http_path is still local.
    let mut patched = 0usize;
    let mut skipped_ambiguous = 0usize;

    for node in nodes.iter_mut() {
        if node.language != "python" || node.id.kind != NodeKind::ApiEndpoint {
            continue;
        }

        // Skip nodes that were already patched by fastapi_router_prefix_pass.
        // Those have `http_path_local` set.
        if node.metadata.contains_key("http_path_local") {
            continue;
        }

        let local_path = match node.metadata.get("http_path") {
            Some(p) if p.starts_with('/') && !p.is_empty() => p.clone(),
            _ => continue,
        };

        // Find all SDK paths that end with this local path, with a proper
        // segment boundary (the char before the suffix must be `/`).
        let matches: Vec<&str> = unique_sdk_paths
            .iter()
            .filter(|sdk| is_proper_suffix(sdk, &local_path))
            .map(|s| s.as_str())
            .collect();

        match matches.len() {
            0 => {
                // No SDK path covers this local path — leave unchanged.
            }
            1 => {
                let full_path = matches[0].to_string();
                if full_path == local_path {
                    // The SDK path equals the local path — already the full path.
                    continue;
                }
                apply_full_path(node, &full_path);
                tracing::debug!(
                    "sdk_path_inference: {} → {} (inferred from SDK)",
                    local_path,
                    full_path,
                );
                patched += 1;
            }
            n => {
                // Ambiguous: multiple SDK paths end with the same suffix.
                // Skip to avoid false positives.
                tracing::debug!(
                    "sdk_path_inference: skipping '{}' — {} SDK paths match (ambiguous)",
                    local_path,
                    n,
                );
                skipped_ambiguous += 1;
            }
        }
    }

    if patched > 0 || skipped_ambiguous > 0 {
        tracing::info!(
            "sdk_path_inference: patched {} endpoint(s), skipped {} ambiguous",
            patched,
            skipped_ambiguous,
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return `true` if `full_path` ends with `local_path` at a URL segment boundary.
///
/// Since both `full_path` and `local_path` begin with `/`, the boundary is guaranteed
/// by `ends_with` itself: the leading `/` of `local_path` must appear as a literal `/`
/// in `full_path` at the start of the suffix.  This prevents non-boundary substring
/// matches like `/barfoo` falsely matching `/foo` (because `/barfoo` does NOT end with
/// `/foo` — its last four characters are `rfoo`, not `/foo`).
///
/// Examples:
/// - `is_proper_suffix("/workspaces/{id}/foo", "/foo")` → `true`
/// - `is_proper_suffix("/foo", "/foo")` → `true`  (exact match)
/// - `is_proper_suffix("/barfoo", "/foo")` → `false`  (`ends_with("/foo")` fails)
/// - `is_proper_suffix("/workspaces/{id}/foo", "/workspaces/{id}/foo")` → `true`
///
/// Callers must ensure `local_path` starts with `/` (all FastAPI route paths do).
pub(crate) fn is_proper_suffix(full_path: &str, local_path: &str) -> bool {
    full_path.ends_with(local_path)
}

/// Apply an SDK-inferred full path to an `ApiEndpoint` node.
///
/// Stores the original local path in `metadata["http_path_local"]` for
/// consistency with [`apply_prefix`] in `fastapi_router_prefix.rs`, then
/// updates `http_path`, `id.name`, and `signature`.
///
/// [`apply_prefix`]: super::fastapi_router_prefix::apply_prefix
fn apply_full_path(node: &mut Node, full_path: &str) {
    // Preserve the original local path before overwriting.
    if let Some(local) = node.metadata.get("http_path") {
        let local = local.clone();
        node.metadata.insert("http_path_local".to_string(), local);
    }

    let method = node
        .metadata
        .get("http_method")
        .cloned()
        .unwrap_or_else(|| "GET".to_string());

    node.metadata.insert("http_path".to_string(), full_path.to_string());
    node.id.name = format!("{} {}", method, full_path);
    node.signature = format!("[route_decorator] {} {}", method, full_path);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::openapi_sdk_link::is_generated_sdk_file_pub;
    use crate::graph::{ExtractionSource, NodeId, NodeKind};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    // --- is_proper_suffix ---

    #[test]
    fn test_exact_match_is_proper_suffix() {
        assert!(is_proper_suffix("/expertunities", "/expertunities"));
    }

    #[test]
    fn test_segment_boundary_suffix() {
        assert!(is_proper_suffix("/workspaces/{id}/expertunities", "/expertunities"));
    }

    #[test]
    fn test_no_segment_boundary_not_suffix() {
        // "/barfoo".ends_with("/foo") is false because the last 4 chars are "rfoo", not "/foo".
        assert!(!is_proper_suffix("/barfoo", "/foo"));
    }

    #[test]
    fn test_partial_segment_not_suffix() {
        // "/expertunities" does not end with "/unities" (it ends with "unities" not "/unities").
        assert!(!is_proper_suffix("/workspaces/{id}/expertunities", "/unities"));
    }

    #[test]
    fn test_longer_local_than_full_not_suffix() {
        assert!(!is_proper_suffix("/foo", "/workspaces/{id}/foo"));
    }

    #[test]
    fn test_multi_segment_suffix() {
        assert!(is_proper_suffix("/workspaces/{id}/comment/{comment_id}/accept", "/comment/{comment_id}/accept"));
    }

    #[test]
    fn test_root_slash_suffix_of_trailing_slash_path() {
        // "/" IS a suffix match of "/workspaces/{id}/" at the string level.
        // However, the sdk_path_inference_pass collection phase guards against
        // single-char SDK paths (requires len > 1), so "/" never enters the
        // matching set. This test documents the helper's raw behaviour.
        assert!(is_proper_suffix("/workspaces/{id}/", "/"));
    }

    // --- helper factories ---

    fn make_sdk_const(path: &str) -> Node {
        Node {
            id: NodeId {
                root: "frontend".to_string(),
                file: PathBuf::from("src/api/sdk.gen.ts"),
                name: path.to_string(),
                kind: NodeKind::Const,
            },
            language: "typescript".to_string(),
            line_start: 1,
            line_end: 1,
            signature: format!("\"{}\"", path),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn make_api_endpoint(http_path: &str, method: &str, already_patched: bool) -> Node {
        let mut metadata = BTreeMap::new();
        metadata.insert("http_path".to_string(), http_path.to_string());
        metadata.insert("http_method".to_string(), method.to_string());
        if already_patched {
            metadata.insert("http_path_local".to_string(), http_path.to_string());
        }
        Node {
            id: NodeId {
                root: "backend".to_string(),
                file: PathBuf::from("src/api/workspaces/id/expertunities.py"),
                name: format!("{} {}", method, http_path),
                kind: NodeKind::ApiEndpoint,
            },
            language: "python".to_string(),
            line_start: 1,
            line_end: 5,
            signature: format!("[route_decorator] {} {}", method, http_path),
            body: String::new(),
            metadata,
            source: ExtractionSource::TreeSitter,
        }
    }

    // --- sdk_path_inference_pass ---

    /// Happy path: one SDK path suffix-matches the local path.
    #[test]
    fn test_infers_full_path_from_sdk() {
        let mut nodes = vec![
            make_sdk_const("/workspaces/{id}/expertunities"),
            make_api_endpoint("/expertunities", "GET", false),
        ];

        sdk_path_inference_pass(&mut nodes);

        let ep = nodes.iter().find(|n| n.id.kind == NodeKind::ApiEndpoint).unwrap();
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/workspaces/{id}/expertunities"),
            "full path must be inferred from SDK"
        );
        assert_eq!(ep.id.name, "GET /workspaces/{id}/expertunities");
        assert_eq!(ep.signature, "[route_decorator] GET /workspaces/{id}/expertunities");
        assert_eq!(
            ep.metadata.get("http_path_local").map(|s| s.as_str()),
            Some("/expertunities"),
            "original local path must be preserved in http_path_local"
        );
    }

    /// Nodes already patched by fastapi_router_prefix_pass (have http_path_local) are skipped.
    #[test]
    fn test_skips_already_patched_endpoint() {
        let mut nodes = vec![
            make_sdk_const("/workspaces/{id}/expertunities"),
            make_api_endpoint("/workspaces/{id}/expertunities", "GET", true), // already patched
        ];

        sdk_path_inference_pass(&mut nodes);

        let ep = nodes.iter().find(|n| n.id.kind == NodeKind::ApiEndpoint).unwrap();
        // Should remain as-is (already had full path set by prefix pass).
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/workspaces/{id}/expertunities"),
        );
        // http_path_local must still be the original value, not re-set.
        assert_eq!(
            ep.metadata.get("http_path_local").map(|s| s.as_str()),
            Some("/workspaces/{id}/expertunities"),
        );
    }

    /// When multiple SDK paths match (ambiguous), endpoint is left unchanged.
    #[test]
    fn test_ambiguous_match_leaves_path_unchanged() {
        let mut nodes = vec![
            make_sdk_const("/workspaces/{id}/items"),
            make_sdk_const("/admin/items"),
            make_api_endpoint("/items", "GET", false),
        ];

        sdk_path_inference_pass(&mut nodes);

        let ep = nodes.iter().find(|n| n.id.kind == NodeKind::ApiEndpoint).unwrap();
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/items"),
            "ambiguous match must not update the path"
        );
        assert!(
            ep.metadata.get("http_path_local").is_none(),
            "http_path_local must not be set when no update happened"
        );
    }

    /// When no SDK path matches, endpoint is left unchanged.
    #[test]
    fn test_no_matching_sdk_path_leaves_unchanged() {
        let mut nodes = vec![
            make_sdk_const("/workspaces/{id}/other"),
            make_api_endpoint("/expertunities", "GET", false),
        ];

        sdk_path_inference_pass(&mut nodes);

        let ep = nodes.iter().find(|n| n.id.kind == NodeKind::ApiEndpoint).unwrap();
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/expertunities"),
        );
    }

    /// When the SDK path equals the local path exactly, no update occurs.
    #[test]
    fn test_exact_sdk_path_equals_local_no_update() {
        let mut nodes = vec![
            make_sdk_const("/expertunities"),
            make_api_endpoint("/expertunities", "GET", false),
        ];

        sdk_path_inference_pass(&mut nodes);

        let ep = nodes.iter().find(|n| n.id.kind == NodeKind::ApiEndpoint).unwrap();
        // Path stays the same, no http_path_local written (no prefix was applied).
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/expertunities"),
        );
    }

    /// Non-SDK Const nodes are not used for inference.
    #[test]
    fn test_non_sdk_const_not_used() {
        // A Const from a regular (non-SDK) file should not be used.
        let mut non_sdk_const = make_sdk_const("/workspaces/{id}/expertunities");
        non_sdk_const.id.file = PathBuf::from("src/services/workspace_service.ts");

        let mut nodes = vec![
            non_sdk_const,
            make_api_endpoint("/expertunities", "GET", false),
        ];

        sdk_path_inference_pass(&mut nodes);

        let ep = nodes.iter().find(|n| n.id.kind == NodeKind::ApiEndpoint).unwrap();
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/expertunities"),
            "non-SDK const must not influence path inference"
        );
    }

    /// No SDK Const nodes → fast return, no changes.
    #[test]
    fn test_no_sdk_consts_is_noop() {
        let mut nodes = vec![
            make_api_endpoint("/expertunities", "GET", false),
        ];
        sdk_path_inference_pass(&mut nodes);
        let ep = &nodes[0];
        assert_eq!(ep.metadata.get("http_path").map(|s| s.as_str()), Some("/expertunities"));
    }

    /// Non-Python endpoints are not modified even if a matching SDK path exists.
    #[test]
    fn test_non_python_endpoint_not_modified() {
        let mut non_python = make_api_endpoint("/items", "GET", false);
        non_python.language = "typescript".to_string();

        let mut nodes = vec![
            make_sdk_const("/workspaces/{id}/items"),
            non_python,
        ];

        sdk_path_inference_pass(&mut nodes);

        let ep = nodes.iter().find(|n| n.id.kind == NodeKind::ApiEndpoint).unwrap();
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/items"),
            "non-python endpoint must not be modified"
        );
    }

    /// False-boundary guard: `/barfoo` must NOT match an endpoint with path `/foo`.
    #[test]
    fn test_no_false_segment_boundary_match() {
        let mut nodes = vec![
            make_sdk_const("/barfoo"),
            make_api_endpoint("/foo", "GET", false),
        ];

        sdk_path_inference_pass(&mut nodes);

        let ep = nodes.iter().find(|n| n.id.kind == NodeKind::ApiEndpoint).unwrap();
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/foo"),
            "'/barfoo' must not suffix-match '/foo'"
        );
    }

    /// Multiple endpoints with unambiguous matches — each gets its own full path.
    #[test]
    fn test_multiple_endpoints_each_inferred_independently() {
        let mut nodes = vec![
            make_sdk_const("/workspaces/{id}/expertunities"),
            make_sdk_const("/workspaces/{id}/activities"),
            make_api_endpoint("/expertunities", "GET", false),
            make_api_endpoint("/activities", "GET", false),
        ];

        sdk_path_inference_pass(&mut nodes);

        let expertunities_ep = nodes.iter().find(|n| {
            n.id.kind == NodeKind::ApiEndpoint && n.id.name.contains("expertunities")
        }).unwrap();
        let activities_ep = nodes.iter().find(|n| {
            n.id.kind == NodeKind::ApiEndpoint && n.id.name.contains("activities")
        }).unwrap();

        assert_eq!(
            expertunities_ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/workspaces/{id}/expertunities"),
        );
        assert_eq!(
            activities_ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/workspaces/{id}/activities"),
        );
    }

    /// Verify that the SDK file detection function works for sdk.gen.ts.
    #[test]
    fn test_sdk_gen_ts_recognized() {
        assert!(is_generated_sdk_file_pub(&PathBuf::from("src/api/sdk.gen.ts")));
    }

    // --- adversarial edge cases (dissent-seeded) ---

    /// Trailing slash in SDK path does NOT match endpoint without trailing slash.
    /// This is the "trailing slash mismatch" failure scenario from the dissent.
    /// If the SDK stores `/workspaces/{id}/expertunities/` but the endpoint has
    /// `/expertunities`, `ends_with` returns false and no update is made.
    #[test]
    fn test_trailing_slash_mismatch_no_match() {
        let mut nodes = vec![
            make_sdk_const("/workspaces/{id}/expertunities/"),  // trailing slash in SDK
            make_api_endpoint("/expertunities", "GET", false),   // no trailing slash
        ];

        sdk_path_inference_pass(&mut nodes);

        let ep = nodes.iter().find(|n| n.id.kind == NodeKind::ApiEndpoint).unwrap();
        // "/workspaces/{id}/expertunities/" does not end with "/expertunities" (no trailing slash)
        // so no match → path unchanged.
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/expertunities"),
            "trailing-slash SDK path must NOT match endpoint without trailing slash"
        );
    }

    /// SDK path with trailing slash DOES match endpoint with trailing slash.
    #[test]
    fn test_trailing_slash_matches_trailing_slash() {
        let mut nodes = vec![
            make_sdk_const("/workspaces/{id}/expertunities/"),
            make_api_endpoint("/expertunities/", "GET", false),
        ];

        sdk_path_inference_pass(&mut nodes);

        let ep = nodes.iter().find(|n| n.id.kind == NodeKind::ApiEndpoint).unwrap();
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/workspaces/{id}/expertunities/"),
            "trailing-slash SDK path must match endpoint with same trailing slash"
        );
    }

    /// Hyphenated SDK filename (e.g. "api-client.ts") is NOT detected as generated.
    /// This is the "SDK file not detected" failure scenario from the dissent.
    #[test]
    fn test_hyphenated_sdk_filename_not_detected() {
        let mut non_sdk_const = make_sdk_const("/workspaces/{id}/expertunities");
        non_sdk_const.id.file = PathBuf::from("src/api-client.ts"); // hyphen, not matched

        let mut nodes = vec![
            non_sdk_const,
            make_api_endpoint("/expertunities", "GET", false),
        ];

        sdk_path_inference_pass(&mut nodes);

        let ep = nodes.iter().find(|n| n.id.kind == NodeKind::ApiEndpoint).unwrap();
        assert_eq!(
            ep.metadata.get("http_path").map(|s| s.as_str()),
            Some("/expertunities"),
            "hyphenated SDK filename must not be used for inference"
        );
    }
}
