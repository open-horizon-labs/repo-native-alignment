//! Links Rust struct-literal sites to locally declared structs.

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeKind};
use std::collections::HashMap;

#[cfg(test)]
const STRUCT_LITERAL_KIND: &str = "struct_literal";

/// Emit construction → declaration edges for unambiguous repo-local Rust structs.
///
/// Construction nodes are produced while the Rust AST is available. Resolution is
/// deliberately deferred until the full node set exists so literals can link across
/// files without LSP. Ambiguous and external names remain unlinked.
pub fn struct_construction_pass(nodes: &[Node]) -> Vec<Edge> {
    type NameKey = (String, String, String);
    type ModuleKey = (String, String, String, String);
    type FileKey = (String, String, String, std::path::PathBuf);

    let mut by_name: HashMap<NameKey, Vec<&Node>> = HashMap::new();
    let mut by_module: HashMap<ModuleKey, Vec<&Node>> = HashMap::new();
    let mut by_file: HashMap<FileKey, Vec<&Node>> = HashMap::new();
    for node in nodes {
        if node.id.kind == NodeKind::Struct {
            let name_key = (
                node.id.root.clone(),
                node.language.clone(),
                node.id.name.clone(),
            );
            by_name.entry(name_key.clone()).or_default().push(node);
            by_module
                .entry((
                    name_key.0.clone(),
                    name_key.1.clone(),
                    name_key.2.clone(),
                    node.metadata.get("module_path").cloned().unwrap_or_default(),
                ))
                .or_default()
                .push(node);
            by_file
                .entry((name_key.0, name_key.1, name_key.2, node.id.file.clone()))
                .or_default()
                .push(node);
        }
    }

    nodes
        .iter()
        .filter(|node| {
            node.metadata
                .get("construction_site")
                .is_some_and(|kind| kind == "struct")
        })
        .filter_map(|site| {
            let target = site.metadata.get("constructed_type")?;
            let name_key = (
                site.id.root.clone(),
                site.language.clone(),
                target.clone(),
            );
            let type_path = site
                .metadata
                .get("imported_type_path")
                .or_else(|| site.metadata.get("type_path"))?;
            if type_path == "<ambiguous>" {
                return None;
            }
            let path_segments = type_path_segments(type_path);
            let candidates = if path_segments.len() > 1 {
                let target_module = resolve_qualified_module(site, &path_segments)?.join("::");
                by_module.get(&(
                    name_key.0.clone(),
                    name_key.1.clone(),
                    name_key.2.clone(),
                    target_module,
                ))?
            } else {
                let named = by_name.get(&name_key)?;
                if named.len() == 1 {
                    named
                } else {
                    by_file.get(&(
                        name_key.0,
                        name_key.1,
                        name_key.2,
                        site.id.file.clone(),
                    ))?
                }
            };
            let [declaration] = candidates.as_slice() else {
                return None;
            };
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
        "self" => module_segments(site),
        "super" => {
            let mut current = module_segments(site);
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

fn module_segments(node: &Node) -> Vec<String> {
    node.metadata
        .get("module_path")
        .map(|path| {
            path.split("::")
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::extract::rust::rust_module_segments;
    use crate::graph::{NodeId, NodeKind};

    fn node(name: &str, kind: NodeKind, file: &str) -> Node {
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "module_path".into(),
            rust_module_segments(std::path::Path::new(file)).join("::"),
        );
        if matches!(kind, NodeKind::Other(_)) {
            metadata.insert("construction_site".into(), "struct".into());
        }
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
            metadata,
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

    #[test]
    fn import_binding_prevents_external_basename_false_link() {
        let declaration = node("Config", NodeKind::Struct, "src/local.rs");
        let mut site = node(
            "Config@1:1",
            NodeKind::Other(STRUCT_LITERAL_KIND.into()),
            "src/current.rs",
        );
        site.metadata
            .insert("constructed_type".into(), "Config".into());
        site.metadata.insert("type_path".into(), "Config".into());
        site.metadata
            .insert("imported_type_path".into(), "external::Config".into());

        assert!(struct_construction_pass(&[declaration, site]).is_empty());
    }

    #[test]
    fn construction_only_links_same_language_declarations() {
        let mut foreign = node("Config", NodeKind::Struct, "src/config.go");
        foreign.language = "go".into();
        let mut site = node(
            "Config@1:1",
            NodeKind::Other(STRUCT_LITERAL_KIND.into()),
            "src/current.rs",
        );
        site.metadata
            .insert("constructed_type".into(), "Config".into());
        site.metadata.insert("type_path".into(), "Config".into());

        assert!(struct_construction_pass(&[foreign, site]).is_empty());
    }

    #[test]
    fn inline_module_metadata_disambiguates_same_file_declarations() {
        let mut a = node("Config", NodeKind::Struct, "src/lib.rs");
        a.metadata.insert("module_path".into(), "a".into());
        let mut b = node("Config", NodeKind::Struct, "src/lib.rs");
        b.metadata.insert("module_path".into(), "b".into());
        let mut site = node(
            "Config@1:1",
            NodeKind::Other(STRUCT_LITERAL_KIND.into()),
            "src/lib.rs",
        );
        site.metadata
            .insert("constructed_type".into(), "Config".into());
        site.metadata
            .insert("type_path".into(), "crate::a::Config".into());

        let edges = struct_construction_pass(&[a.clone(), b, site.clone()]);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from, site.id);
        assert_eq!(edges[0].to, a.id);
    }

    /// Hot-path regression fixture following the existing post-pass convention:
    /// 100k nodes, deterministic edge-count assertion, and wall-clock diagnostics.
    #[test]
    fn struct_construction_pass_scan_time_regression() {
        use std::time::Instant;

        const PAIRS: usize = 50_000;
        let mut nodes = Vec::with_capacity(PAIRS * 2);
        for index in 0..PAIRS {
            let name = format!("Type{index}");
            let declaration = node(&name, NodeKind::Struct, &format!("src/type_{index}.rs"));
            let mut site = node(
                &format!("{name}@1:1"),
                NodeKind::Other(STRUCT_LITERAL_KIND.into()),
                &format!("src/use_{index}.rs"),
            );
            site.metadata
                .insert("constructed_type".into(), name.clone());
            site.metadata.insert("type_path".into(), name);
            nodes.push(declaration);
            nodes.push(site);
        }

        let started = Instant::now();
        let edges = struct_construction_pass(&nodes);
        let elapsed = started.elapsed();
        assert_eq!(edges.len(), PAIRS);
        println!(
            "struct_construction_pass_scan_time: {:.2}ms ({} nodes, {} edges)",
            elapsed.as_secs_f64() * 1_000.0,
            nodes.len(),
            edges.len()
        );
    }
}
