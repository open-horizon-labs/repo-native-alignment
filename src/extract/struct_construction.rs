//! Links Rust struct-literal sites to locally declared structs.

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeKind};
use std::collections::HashMap;

use super::rust::rust_module_segments;

const STRUCT_LITERAL_KIND: &str = "struct_literal";

/// Emit construction → declaration edges for unambiguous repo-local Rust structs.
///
/// Construction nodes are produced while the Rust AST is available. Resolution is
/// deliberately deferred until the full node set exists so literals can link across
/// files without LSP. Ambiguous and external names remain unlinked.
pub fn struct_construction_pass(nodes: &[Node]) -> Vec<Edge> {
    let mut declarations: HashMap<(&str, &str), Vec<&Node>> = HashMap::new();
    for node in nodes {
        if node.language == "rust" && node.id.kind == NodeKind::Struct {
            declarations
                .entry((node.id.root.as_str(), node.id.name.as_str()))
                .or_default()
                .push(node);
        }
    }

    nodes
        .iter()
        .filter(|node| {
            node.language == "rust"
                && matches!(&node.id.kind, NodeKind::Other(kind) if kind == STRUCT_LITERAL_KIND)
        })
        .filter_map(|site| {
            let target = site.metadata.get("constructed_type")?;
            let candidates = declarations.get(&(site.id.root.as_str(), target.as_str()))?;
            let declaration = resolve_declaration(site, candidates)?;
            Some(Edge {
                from: site.id.clone(),
                to: declaration.id.clone(),
                kind: EdgeKind::Constructs,
                source: ExtractionSource::TreeSitter,
                confidence: Confidence::Detected,
            })
        })
        .collect()
}

fn resolve_declaration<'a>(site: &Node, candidates: &[&'a Node]) -> Option<&'a Node> {
    let type_path = site.metadata.get("type_path")?;
    let path_segments = type_path_segments(type_path);
    if path_segments.len() > 1 {
        let target_module = resolve_qualified_module(site, &path_segments)?;
        let mut matching = candidates
            .iter()
            .copied()
            .filter(|candidate| rust_module_segments(&candidate.id.file) == target_module);
        let first = matching.next()?;
        return matching.next().is_none().then_some(first);
    }

    if candidates.len() == 1 {
        return Some(candidates[0]);
    }

    // A same-file declaration is unambiguous even when another module declares a
    // struct with the same basename. Otherwise do not guess: qualified Rust paths
    // can involve aliases/re-exports that require semantic resolution.
    let mut same_file = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.id.file == site.id.file);
    let first = same_file.next()?;
    same_file.next().is_none().then_some(first)
}

fn type_path_segments(type_path: &str) -> Vec<&str> {
    let without_generics = type_path
        .split("::<")
        .next()
        .unwrap_or(type_path)
        .split('<')
        .next()
        .unwrap_or(type_path);
    without_generics
        .split("::")
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn resolve_qualified_module(site: &Node, path_segments: &[&str]) -> Option<Vec<String>> {
    let qualifiers = path_segments.get(..path_segments.len().checked_sub(1)?)?;
    let mut resolved = match qualifiers.first().copied()? {
        "crate" => Vec::new(),
        "self" => rust_module_segments(&site.id.file),
        "super" => {
            let mut current = rust_module_segments(&site.id.file);
            let super_count = qualifiers
                .iter()
                .take_while(|segment| **segment == "super")
                .count();
            if super_count > current.len() {
                return None;
            }
            current.truncate(current.len() - super_count);
            current
        }
        // A non-keyword qualified path is only locally resolvable when its
        // module segments exactly match a declaration file. Never fall back to
        // basename: the first segment may be an external crate.
        _ => Vec::new(),
    };

    let skip = match qualifiers.first().copied()? {
        "crate" | "self" => 1,
        "super" => qualifiers
            .iter()
            .take_while(|segment| **segment == "super")
            .count(),
        _ => 0,
    };
    resolved.extend(
        qualifiers[skip..]
            .iter()
            .map(|segment| (*segment).to_string()),
    );
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::graph::{NodeId, NodeKind};

    fn node(name: &str, kind: NodeKind, file: &str) -> Node {
        Node {
            id: NodeId {
                root: "repo".into(),
                file: PathBuf::from(file),
                name: name.into(),
                kind,
            },
            language: "rust".into(),
            line_start: 1,
            line_end: 1,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    #[test]
    fn links_unambiguous_cross_file_local_construction() {
        let declaration = node("BusOptions", NodeKind::Struct, "src/options.rs");
        let mut site = node(
            "BusOptions@12:8",
            NodeKind::Other(STRUCT_LITERAL_KIND.into()),
            "src/server.rs",
        );
        site.metadata
            .insert("constructed_type".into(), "BusOptions".into());
        site.metadata
            .insert("type_path".into(), "BusOptions".into());

        let edges = struct_construction_pass(&[declaration.clone(), site.clone()]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, site.id);
        assert_eq!(edges[0].to, declaration.id);
        assert_eq!(edges[0].kind, EdgeKind::Constructs);
    }

    #[test]
    fn leaves_external_and_ambiguous_targets_unlinked() {
        let a = node("Config", NodeKind::Struct, "src/a.rs");
        let b = node("Config", NodeKind::Struct, "src/b.rs");
        let mut ambiguous = node(
            "Config@4:4",
            NodeKind::Other(STRUCT_LITERAL_KIND.into()),
            "src/use_config.rs",
        );
        ambiguous
            .metadata
            .insert("constructed_type".into(), "Config".into());
        ambiguous
            .metadata
            .insert("type_path".into(), "Config".into());
        let mut external = node(
            "External@8:4",
            NodeKind::Other(STRUCT_LITERAL_KIND.into()),
            "src/use_config.rs",
        );
        external
            .metadata
            .insert("constructed_type".into(), "External".into());
        external
            .metadata
            .insert("type_path".into(), "External".into());

        assert!(struct_construction_pass(&[a, b, ambiguous, external]).is_empty());
    }

    #[test]
    fn qualified_paths_resolve_exact_local_modules_without_basename_fallback() {
        let a = node("Config", NodeKind::Struct, "src/a.rs");
        let b = node("Config", NodeKind::Struct, "src/b/mod.rs");
        let same_file = node("Config", NodeKind::Struct, "src/current.rs");

        let site = |name: &str, type_path: &str, file: &str| {
            let mut site = node(name, NodeKind::Other(STRUCT_LITERAL_KIND.into()), file);
            site.metadata
                .insert("constructed_type".into(), "Config".into());
            site.metadata.insert("type_path".into(), type_path.into());
            site
        };
        let crate_a = site("Config@1:1", "crate::a::Config", "src/current.rs");
        let crate_b = site("Config@2:1", "crate::b::Config", "src/current.rs");
        let qualified_other = site("Config@3:1", "b::Config", "src/current.rs");
        let external = site("Config@4:1", "external_crate::Config", "src/current.rs");

        let nodes = vec![
            a.clone(),
            b.clone(),
            same_file,
            crate_a.clone(),
            crate_b.clone(),
            qualified_other.clone(),
            external,
        ];
        let edges = struct_construction_pass(&nodes);
        assert_eq!(edges.len(), 3);
        assert!(
            edges
                .iter()
                .any(|edge| edge.from == crate_a.id && edge.to == a.id)
        );
        assert!(
            edges
                .iter()
                .any(|edge| edge.from == crate_b.id && edge.to == b.id)
        );
        assert!(
            edges
                .iter()
                .any(|edge| edge.from == qualified_other.id && edge.to == b.id)
        );
        assert!(!edges.iter().any(|edge| edge.from.name == "Config@4:1"));
    }

    #[test]
    fn self_and_super_paths_resolve_relative_to_the_site_module() {
        let parent = node("Config", NodeKind::Struct, "src/a.rs");
        let current = node("Config", NodeKind::Struct, "src/a/b.rs");
        let mut self_site = node(
            "Config@1:1",
            NodeKind::Other(STRUCT_LITERAL_KIND.into()),
            "src/a/b.rs",
        );
        self_site
            .metadata
            .insert("constructed_type".into(), "Config".into());
        self_site
            .metadata
            .insert("type_path".into(), "self::Config".into());
        let mut super_site = self_site.clone();
        super_site.id.name = "Config@2:1".into();
        super_site
            .metadata
            .insert("type_path".into(), "super::Config".into());

        let edges = struct_construction_pass(&[
            parent.clone(),
            current.clone(),
            self_site.clone(),
            super_site.clone(),
        ]);
        assert!(
            edges
                .iter()
                .any(|edge| edge.from == self_site.id && edge.to == current.id)
        );
        assert!(
            edges
                .iter()
                .any(|edge| edge.from == super_site.id && edge.to == parent.id)
        );
    }
}
