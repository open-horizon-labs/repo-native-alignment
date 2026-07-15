//! Links Rust struct-literal sites to locally declared structs.

use std::collections::HashMap;

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeKind};

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
        let mut external = node(
            "External@8:4",
            NodeKind::Other(STRUCT_LITERAL_KIND.into()),
            "src/use_config.rs",
        );
        external
            .metadata
            .insert("constructed_type".into(), "External".into());

        assert!(struct_construction_pass(&[a, b, ambiguous, external]).is_empty());
    }
}
