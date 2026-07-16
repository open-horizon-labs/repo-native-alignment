//! Post-extraction pass that scans package manifests for JS/TS, Python, Go,
//! and Rust projects and emits `NodeKind::Other("package")` nodes +
//! `EdgeKind::DependsOn` edges — mirroring the Rust crate graph emitted by
//! the LSP Pass 0 in `src/extract/lsp/mod.rs`.
//!
//! # Supported manifests
//!
//! | Language | File | Dependency source |
//! |----------|------|-------------------|
//! | JS / TypeScript | `package.json` | `dependencies` + `devDependencies` keys |
//! | Rust | `Cargo.toml` | `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, and target-specific dependency tables |
//! | Python | `pyproject.toml` | `[project] dependencies` array |
//! | Python | `requirements.txt` | one package per line |
//! | Go | `go.mod` | `require` directives |
//!
//! Tree-sitter is **not** used — these formats are simple enough for direct
//! text parsing.
//!
//! # Design
//!
//! `manifest_pass` mirrors the pattern of `api_link::api_link_pass`: it is a
//! standalone, synchronous function that takes the list of workspace roots and
//! their filesystem paths, and returns `(nodes, edges)` to be merged into the
//! caller's graph.  Like the crate-graph pass it runs unconditionally (no
//! quiescence guard needed — manifest files are static workspace metadata).
//!
//! # Node identity
//!
//! Every package node is anchored to its manifest file for a stable `NodeId`:
//!
//! | Language | `file` field |
//! |----------|-------------|
//! | JS/TS | `package.json` |
//! | Python | `pyproject.toml` or `requirements.txt` |
//! | Go | `go.mod` |
//!
//! The node name is the package name (or the bare dependency string for
//! requirements.txt lines).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::graph::{Confidence, Edge, EdgeKind, ExtractionSource, Node, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Result of a single manifest scan: nodes and edges to merge into the graph.
pub struct ManifestResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

/// Scan `roots` for package manifests and return package nodes + DependsOn edges.
///
/// Each element of `roots` is `(root_slug, root_path)`.  The function walks each
/// root looking for manifest files one level deep (the manifest must be directly
/// inside `root_path` or in an immediate subdirectory — we do not recurse into
/// every subdirectory because that would match vendored copies).
///
/// Duplicate package nodes (same name + manifest file within a root) are
/// deduplicated.
pub fn manifest_pass(roots: &[(String, PathBuf)]) -> ManifestResult {
    let mut all_nodes: Vec<Node> = Vec::new();
    let mut all_edges: Vec<Edge> = Vec::new();

    for (root_slug, root_path) in roots {
        let ManifestResult { nodes, edges } = scan_root(root_slug, root_path);
        all_nodes.extend(nodes);
        all_edges.extend(edges);
    }
    let package_hosts = load_explicit_package_hosts(roots);
    add_explicit_package_host_edges(&all_nodes, &mut all_edges, &package_hosts);

    ManifestResult {
        nodes: all_nodes,
        edges: all_edges,
    }
}

// ---------------------------------------------------------------------------
// Per-root scanning
// ---------------------------------------------------------------------------

fn scan_root(root_slug: &str, root_path: &Path) -> ManifestResult {
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    // Collect candidate manifest paths: root-level + one level of subdirs.
    let candidate_dirs = candidate_manifest_dirs(root_path);

    for dir in &candidate_dirs {
        // JS / TypeScript
        let pkg_json = dir.join("package.json");
        if pkg_json.exists()
            && let Ok(content) = std::fs::read_to_string(&pkg_json)
        {
            let manifest_file = relative_to(root_path, &pkg_json);
            let (n, e) = parse_package_json(&content, &manifest_file, root_slug);
            nodes.extend(n);
            edges.extend(e);
        }

        // Python — pyproject.toml
        let pyproject = dir.join("pyproject.toml");
        if pyproject.exists()
            && let Ok(content) = std::fs::read_to_string(&pyproject)
        {
            let manifest_file = relative_to(root_path, &pyproject);
            let (n, e) = parse_pyproject_toml(&content, &manifest_file, root_slug);
            nodes.extend(n);
            edges.extend(e);
        }

        // Python — requirements.txt
        let requirements = dir.join("requirements.txt");
        if requirements.exists()
            && let Ok(content) = std::fs::read_to_string(&requirements)
        {
            let manifest_file = relative_to(root_path, &requirements);
            let (n, e) = parse_requirements_txt(&content, &manifest_file, root_slug);
            nodes.extend(n);
            edges.extend(e);
        }

        // Rust — Cargo.toml
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists()
            && let Ok(content) = std::fs::read_to_string(&cargo_toml)
        {
            let manifest_file = relative_to(root_path, &cargo_toml);
            let (n, e) = parse_cargo_toml(&content, &manifest_file, root_slug);
            nodes.extend(n);
            edges.extend(e);
        }

        // Go — go.mod
        let go_mod = dir.join("go.mod");
        if go_mod.exists()
            && let Ok(content) = std::fs::read_to_string(&go_mod)
        {
            let manifest_file = relative_to(root_path, &go_mod);
            let (n, e) = parse_go_mod(&content, &manifest_file, root_slug);
            nodes.extend(n);
            edges.extend(e);
        }
    }

    ManifestResult { nodes, edges }
}

/// Return `root_path` itself plus subdirectories up to `MAX_MANIFEST_DEPTH`
/// levels deep.
///
/// We limit the scan depth to avoid matching vendored/nested copies of manifests
/// (e.g., `node_modules/foo/package.json`, `vendor/github.com/…/go.mod`).
/// Depth 2 catches monorepo layouts like `client/package.json` and
/// `packages/api/pyproject.toml`.
fn candidate_manifest_dirs(root_path: &Path) -> Vec<PathBuf> {
    const MAX_MANIFEST_DEPTH: usize = 2;
    let mut dirs = vec![root_path.to_path_buf()];
    collect_manifest_dirs(root_path, 1, MAX_MANIFEST_DEPTH, &mut dirs);
    dirs
}

/// Recursively collect subdirectories for manifest scanning, up to `max_depth`.
fn collect_manifest_dirs(
    dir: &Path,
    current_depth: usize,
    max_depth: usize,
    dirs: &mut Vec<PathBuf>,
) {
    if current_depth > max_depth {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip hidden directories and known vendor/dependency directories.
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if is_manifest_skip_dir(&name_str) {
                continue;
            }
            dirs.push(path.clone());
            collect_manifest_dirs(&path, current_depth + 1, max_depth, dirs);
        }
    }
}

/// Returns `true` if this directory name should be skipped during manifest scanning.
fn is_manifest_skip_dir(name: &str) -> bool {
    name.starts_with('.')
        || name == "node_modules"
        || name == "vendor"
        || name == "target"
        || name == "__pycache__"
        || name == ".venv"
        || name == "venv"
        || name == "dist"
        || name == "build"
        || name == "out"
        || name == "coverage"
        || name == ".next"
}

/// Compute path relative to `base`, falling back to the absolute path.
fn relative_to(base: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(base).unwrap_or(path).to_path_buf()
}

type PackageHosts = HashMap<(String, String), String>;

fn load_explicit_package_hosts(roots: &[(String, PathBuf)]) -> PackageHosts {
    let mut hosts = PackageHosts::new();

    for (root_slug, root_path) in roots {
        let config_path = root_path.join(".oh").join("config.toml");
        let Ok(content) = std::fs::read_to_string(&config_path) else {
            continue;
        };
        let Ok(value) = content.parse::<toml::Value>() else {
            tracing::debug!(
                "manifest_pass: failed to parse package host config at {}",
                config_path.display()
            );
            continue;
        };

        let Some(table) = value
            .get("workspace")
            .and_then(|workspace| workspace.get("package_hosts"))
            .and_then(|package_hosts| package_hosts.as_table())
        else {
            continue;
        };

        for (package_name, host) in table {
            if let Some(host) = host.as_str() {
                hosts.insert(
                    (root_slug.clone(), package_name.to_string()),
                    host.to_string(),
                );
            }
        }
    }

    hosts
}

fn add_explicit_package_host_edges(
    nodes: &[Node],
    edges: &mut Vec<Edge>,
    package_hosts: &PackageHosts,
) {
    if package_hosts.is_empty() {
        return;
    }

    let existing: std::collections::HashSet<(NodeId, NodeId, EdgeKind)> = edges
        .iter()
        .map(|edge| (edge.from.clone(), edge.to.clone(), edge.kind.clone()))
        .collect();
    let providers_by_name = package_provider_index(nodes);
    let mut added = std::collections::HashSet::new();

    let candidate_edges: Vec<(NodeId, String, NodeId)> = edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::DependsOn)
        .map(|edge| (edge.from.clone(), edge.to.name.clone(), edge.to.clone()))
        .collect();

    for (from, dependency_name, dependency_node) in candidate_edges {
        let host_key = (from.root.clone(), dependency_name.clone());
        let Some(host) = package_hosts.get(&host_key) else {
            continue;
        };
        let Some(provider_candidates) = providers_by_name.get(&dependency_name) else {
            tracing::debug!(
                "manifest_pass: explicit package host '{}' for '{}' did not match an indexed package",
                host,
                dependency_name
            );
            continue;
        };
        let Some(provider) = find_explicit_provider(provider_candidates, host, &dependency_node)
        else {
            tracing::debug!(
                "manifest_pass: explicit package host '{}' for '{}' did not match an indexed package",
                host,
                dependency_name
            );
            continue;
        };

        let key = (from.clone(), provider.id.clone(), EdgeKind::DependsOn);
        if existing.contains(&key) || !added.insert(key.clone()) {
            continue;
        }

        edges.push(Edge {
            from,
            to: provider.id.clone(),
            kind: EdgeKind::DependsOn,
            source: ExtractionSource::Schema,
            confidence: Confidence::Confirmed,
            evidence: Vec::new(),
        });
    }
}

fn package_provider_index(nodes: &[Node]) -> HashMap<String, Vec<&Node>> {
    let mut providers: HashMap<String, Vec<&Node>> = HashMap::new();
    for node in nodes
        .iter()
        .filter(|node| node.id.kind == NodeKind::Other("package".to_string()))
    {
        providers
            .entry(node.id.name.clone())
            .or_default()
            .push(node);
    }
    providers
}

fn find_explicit_provider<'a>(
    candidates: &'a [&'a Node],
    host: &str,
    dependency_node: &NodeId,
) -> Option<&'a Node> {
    let mut matches: Vec<(usize, &Node)> = candidates
        .iter()
        .copied()
        .filter(|node| node.id != *dependency_node)
        .filter_map(|node| package_node_host_rank(node, host).map(|rank| (rank, node)))
        .collect();

    matches.sort_by_key(|(rank, node)| (*rank, node.id.to_stable_id()));
    let best_rank = matches.first().map(|(rank, _)| *rank)?;
    let best_matches: Vec<&Node> = matches
        .into_iter()
        .filter(|(rank, _)| *rank == best_rank)
        .map(|(_, node)| node)
        .collect();

    if best_matches.len() == 1 {
        Some(best_matches[0])
    } else {
        None
    }
}

fn package_node_host_rank(node: &Node, host: &str) -> Option<usize> {
    let normalized_host = host.trim().trim_matches('/');
    if normalized_host.is_empty() {
        return None;
    }

    if node.id.root == normalized_host {
        return Some(0);
    }

    let host_path = std::path::Path::new(normalized_host);
    if node.id.file == host_path || node.id.file.starts_with(host_path) {
        return Some(1);
    }

    node.metadata
        .get("repository")
        .and_then(|url| repository_slug(url))
        .and_then(|slug| {
            let host_slug = repository_slug(normalized_host)
                .unwrap_or_else(|| normalized_host.to_ascii_lowercase());
            (slug == host_slug).then_some(2)
        })
}

fn repository_slug(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let slug = trimmed.rsplit(['/', ':']).next()?.trim();
    (!slug.is_empty()).then(|| slug.to_ascii_lowercase())
}

// ---------------------------------------------------------------------------
// Helpers: emit nodes + edges from a dep list
// ---------------------------------------------------------------------------

fn make_package_node(name: &str, manifest_file: &Path, language: &str, root_slug: &str) -> Node {
    Node {
        id: NodeId {
            root: root_slug.to_string(),
            file: manifest_file.to_path_buf(),
            name: name.to_string(),
            kind: NodeKind::Other("package".to_string()),
        },
        language: language.to_string(),
        line_start: 0,
        line_end: 0,
        signature: format!("package {}", name),
        body: name.to_string(),
        metadata: BTreeMap::new(),
        source: ExtractionSource::Schema,
    }
}

fn make_package_node_with_metadata(
    name: &str,
    manifest_file: &Path,
    language: &str,
    root_slug: &str,
    metadata: BTreeMap<String, String>,
) -> Node {
    Node {
        id: NodeId {
            root: root_slug.to_string(),
            file: manifest_file.to_path_buf(),
            name: name.to_string(),
            kind: NodeKind::Other("package".to_string()),
        },
        language: language.to_string(),
        line_start: 0,
        line_end: 0,
        signature: format!("package {}", name),
        body: name.to_string(),
        metadata,
        source: ExtractionSource::Schema,
    }
}

fn make_dep_edge(from_name: &str, to_name: &str, manifest_file: &Path, root_slug: &str) -> Edge {
    let from_id = NodeId {
        root: root_slug.to_string(),
        file: manifest_file.to_path_buf(),
        name: from_name.to_string(),
        kind: NodeKind::Other("package".to_string()),
    };
    let to_id = NodeId {
        root: root_slug.to_string(),
        file: manifest_file.to_path_buf(),
        name: to_name.to_string(),
        kind: NodeKind::Other("package".to_string()),
    };
    Edge {
        from: from_id,
        to: to_id,
        kind: EdgeKind::DependsOn,
        source: ExtractionSource::Schema,
        confidence: Confidence::Detected,
        evidence: Vec::new(),
    }
}

/// Emit nodes + edges for `package_name` depending on each of `deps`.
fn emit_package_graph(
    package_name: &str,
    deps: &[String],
    manifest_file: &Path,
    language: &str,
    root_slug: &str,
) -> (Vec<Node>, Vec<Edge>) {
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    // Collect all unique names so we create a node for every package we see.
    let mut all_names: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::from([package_name.to_string()]);
    for dep in deps {
        all_names.insert(dep.clone());
    }

    for name in &all_names {
        nodes.push(make_package_node(name, manifest_file, language, root_slug));
    }

    for dep in deps {
        edges.push(make_dep_edge(package_name, dep, manifest_file, root_slug));
    }

    (nodes, edges)
}

// ---------------------------------------------------------------------------
// JS / TypeScript: package.json
// ---------------------------------------------------------------------------

/// Parse a `package.json` file.
///
/// Extracts:
/// - `name` field → the package node name
/// - `dependencies` + `devDependencies` keys → DependsOn edges
///
/// If `name` is absent the manifest file name is used as a fallback.
pub fn parse_package_json(
    content: &str,
    manifest_file: &Path,
    root_slug: &str,
) -> (Vec<Node>, Vec<Edge>) {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(content) else {
        tracing::debug!(
            "manifest_pass: failed to parse package.json at {}",
            manifest_file.display()
        );
        return (Vec::new(), Vec::new());
    };

    let package_name = json
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            // Fallback: use the parent directory name.
            manifest_file
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("(unknown)")
        })
        .to_string();

    let mut deps: Vec<String> = Vec::new();

    for key in &["dependencies", "devDependencies"] {
        if let Some(obj) = json.get(key).and_then(|v| v.as_object()) {
            for dep_name in obj.keys() {
                deps.push(dep_name.clone());
            }
        }
    }

    if deps.is_empty() {
        // No deps — still emit the package node itself so the package is visible
        // in the graph even if it has no outgoing dependency edges.
        let node = make_package_node(&package_name, manifest_file, "javascript", root_slug);
        return (vec![node], Vec::new());
    }

    emit_package_graph(&package_name, &deps, manifest_file, "javascript", root_slug)
}

// ---------------------------------------------------------------------------
// Rust: Cargo.toml
// ---------------------------------------------------------------------------

/// Parse a `Cargo.toml` file.
///
/// Extracts:
/// - `[package].name` → package node name
/// - `[package].repository` → provider metadata for explicit workspace package hosting
/// - Cargo dependency tables → DependsOn edges and dependency metadata
pub fn parse_cargo_toml(
    content: &str,
    manifest_file: &Path,
    root_slug: &str,
) -> (Vec<Node>, Vec<Edge>) {
    let Ok(value) = content.parse::<toml::Value>() else {
        tracing::debug!(
            "manifest_pass: failed to parse Cargo.toml at {}",
            manifest_file.display()
        );
        return (Vec::new(), Vec::new());
    };

    let Some(package) = value.get("package") else {
        tracing::debug!(
            "manifest_pass: skipping workspace-only Cargo.toml at {}",
            manifest_file.display()
        );
        return (Vec::new(), Vec::new());
    };
    let package_name = package
        .get("name")
        .and_then(|name| name.as_str())
        .unwrap_or_else(|| {
            manifest_file
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("(unknown)")
        })
        .to_string();

    let mut package_metadata = BTreeMap::new();
    package_metadata.insert("manifest_kind".to_string(), "cargo".to_string());
    if let Some(repository) = value
        .get("package")
        .and_then(|package| package.get("repository"))
        .and_then(|repository| repository.as_str())
    {
        package_metadata.insert("repository".to_string(), repository.to_string());
    }

    let deps = cargo_dependencies(&value);
    if deps.is_empty() {
        return (
            vec![make_package_node_with_metadata(
                &package_name,
                manifest_file,
                "rust",
                root_slug,
                package_metadata,
            )],
            Vec::new(),
        );
    }

    let mut nodes = vec![make_package_node_with_metadata(
        &package_name,
        manifest_file,
        "rust",
        root_slug,
        package_metadata,
    )];
    let mut edges = Vec::new();

    let mut deps_by_package: std::collections::BTreeMap<String, BTreeMap<String, String>> =
        std::collections::BTreeMap::new();
    for dep in deps {
        match deps_by_package.entry(dep.package_name) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(dep.metadata);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                merge_cargo_dependency_metadata(entry.get_mut(), dep.metadata);
            }
        }
    }

    for (dep_name, metadata) in deps_by_package {
        nodes.push(make_package_node_with_metadata(
            &dep_name,
            manifest_file,
            "rust",
            root_slug,
            metadata,
        ));
        edges.push(make_dep_edge(
            &package_name,
            &dep_name,
            manifest_file,
            root_slug,
        ));
    }

    (nodes, edges)
}

struct CargoDependency {
    package_name: String,
    metadata: BTreeMap<String, String>,
}

fn cargo_dependencies(value: &toml::Value) -> Vec<CargoDependency> {
    let mut deps = Vec::new();
    let Some(table) = value.as_table() else {
        return deps;
    };

    collect_cargo_dependency_table_entries(table, &mut deps);

    if let Some(targets) = table.get("target").and_then(|target| target.as_table()) {
        for target in targets.values().filter_map(|target| target.as_table()) {
            collect_cargo_dependency_table_entries(target, &mut deps);
        }
    }

    deps
}

fn collect_cargo_dependency_table_entries(
    table: &toml::map::Map<String, toml::Value>,
    deps: &mut Vec<CargoDependency>,
) {
    for name in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(dep_table) = table.get(name).and_then(|value| value.as_table()) else {
            continue;
        };
        for (alias, spec) in dep_table {
            deps.push(cargo_dependency(alias, spec));
        }
    }
}

fn cargo_dependency(alias: &str, spec: &toml::Value) -> CargoDependency {
    let mut package_name = alias.to_string();
    let mut metadata = BTreeMap::new();
    metadata.insert("dependency_kind".to_string(), "cargo".to_string());
    metadata.insert("dependency_alias".to_string(), alias.to_string());

    if let Some(table) = spec.as_table() {
        if let Some(actual_package) = table.get("package").and_then(|value| value.as_str()) {
            package_name = actual_package.to_string();
            metadata.insert("dependency_package".to_string(), actual_package.to_string());
        }
        for key in ["git", "path", "branch", "tag", "rev", "version"] {
            if let Some(value) = table.get(key).and_then(|value| value.as_str()) {
                metadata.insert(key.to_string(), value.to_string());
            }
        }
    } else if let Some(version) = spec.as_str() {
        metadata.insert("version".to_string(), version.to_string());
    }

    CargoDependency {
        package_name,
        metadata,
    }
}

fn merge_cargo_dependency_metadata(
    existing: &mut BTreeMap<String, String>,
    incoming: BTreeMap<String, String>,
) {
    let mut aliases = std::collections::BTreeSet::new();
    if let Some(alias) = existing.get("dependency_alias") {
        aliases.insert(alias.clone());
    }
    if let Some(alias_list) = existing.get("dependency_aliases") {
        aliases.extend(
            alias_list
                .split(',')
                .map(str::trim)
                .filter(|alias| !alias.is_empty())
                .map(ToOwned::to_owned),
        );
    }
    if let Some(alias) = incoming.get("dependency_alias") {
        aliases.insert(alias.clone());
    }

    for (key, value) in incoming {
        existing.entry(key).or_insert(value);
    }

    if aliases.len() > 1 {
        existing.insert(
            "dependency_aliases".to_string(),
            aliases.into_iter().collect::<Vec<_>>().join(","),
        );
    }
}

// ---------------------------------------------------------------------------
// Python: pyproject.toml
// ---------------------------------------------------------------------------

/// Parse a `pyproject.toml` file.
///
/// Extracts:
/// - `[project] name` → the package node name
/// - `[project] dependencies` array → DependsOn edges (PEP 508 specifiers
///   are stripped to the bare package name before the version constraint)
pub fn parse_pyproject_toml(
    content: &str,
    manifest_file: &Path,
    root_slug: &str,
) -> (Vec<Node>, Vec<Edge>) {
    // Minimal TOML parsing — pull out [project].name and [project].dependencies
    // without a full TOML library dependency.  We look for the [project] section
    // and parse its `name` and `dependencies` keys directly.
    //
    // This covers the PEP 517/518/621 standard layout used by virtually every
    // modern Python project.  For legacy `setup.cfg` / `setup.py` we skip
    // (rare in new projects and would require more complex parsing).

    let package_name = extract_toml_string(content, "name");
    let package_name = package_name.as_deref().unwrap_or("(unknown)");

    let deps = extract_toml_string_array(content, "dependencies");

    // Normalise PEP 508 specifiers: "requests>=2.0" → "requests"
    let dep_names: Vec<String> = deps.iter().filter_map(|s| pep508_name(s)).collect();

    if dep_names.is_empty() {
        let node = make_package_node(package_name, manifest_file, "python", root_slug);
        return (vec![node], Vec::new());
    }

    emit_package_graph(package_name, &dep_names, manifest_file, "python", root_slug)
}

/// Extract the first `key = "value"` within the `[project]` TOML section.
/// Handles both `key = "value"` and `key = 'value'` forms.
///
/// Only matches inside the `[project]` table to avoid picking up identically-
/// named keys from `[tool.poetry]` or other sections that appear before
/// `[project]` in some pyproject.toml layouts.
fn extract_toml_string(content: &str, key: &str) -> Option<String> {
    let mut in_project = false;
    for line in content.lines() {
        let trimmed = line.trim();
        // Section header: update section tracking.
        if trimmed.starts_with('[') {
            in_project = trimmed == "[project]";
            continue;
        }
        if !in_project {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix(key) {
            let rest = rest.trim();
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim();
                // Strip surrounding quotes
                if let Some(inner) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
                    return Some(inner.to_string());
                }
                if let Some(inner) = rest.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
                    return Some(inner.to_string());
                }
            }
        }
    }
    None
}

/// Extract a TOML inline array or multi-line array for `key = [...]`
/// within the `[project]` section only.
fn extract_toml_string_array(content: &str, key: &str) -> Vec<String> {
    let mut result = Vec::new();

    let mut in_project = false;
    let mut inside_array = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if !inside_array {
            // Section header: update section tracking.
            if trimmed.starts_with('[') {
                in_project = trimmed == "[project]";
                continue;
            }
            if !in_project {
                continue;
            }
            // Look for `key = [` or `key = ["...`
            if let Some(rest) = trimmed.strip_prefix(key) {
                let rest = rest.trim();
                if let Some(rest) = rest.strip_prefix('=') {
                    let rest = rest.trim();
                    if let Some(array_part) = rest.strip_prefix('[') {
                        // Collect items from this line and subsequent lines until `]`
                        let buf = array_part.to_string();
                        if buf.contains(']') {
                            // Inline array
                            parse_toml_array_items(&buf, &mut result);
                            break;
                        } else {
                            inside_array = true;
                            parse_toml_array_items(&buf, &mut result);
                        }
                    }
                }
            }
        } else {
            // Continuation line inside the array.
            if trimmed.starts_with(']') {
                break;
            }
            parse_toml_array_items(trimmed, &mut result);
        }
    }

    result
}

/// Extract quoted strings from a chunk of TOML array content.
fn parse_toml_array_items(chunk: &str, out: &mut Vec<String>) {
    // Simple: scan for "..." or '...' patterns
    let mut rest = chunk;
    while let Some(start) = rest.find('"').or_else(|| rest.find('\'')) {
        let quote_char = rest.chars().nth(start).unwrap();
        rest = &rest[start + 1..];
        let end = rest.find(quote_char).unwrap_or(rest.len());
        let item = &rest[..end];
        if !item.is_empty() {
            out.push(item.to_string());
        }
        if end < rest.len() {
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
}

/// Strip version constraints from a PEP 508 dependency specifier.
///
/// Examples:
/// - `"requests>=2.0"` → `"requests"`
/// - `"Django [security] >= 2.0"` → `"Django"`
/// - `"pkg-name"` → `"pkg-name"`
fn pep508_name(spec: &str) -> Option<String> {
    // PEP 508: name is everything before the first `[`, `;`, `>`, `<`, `=`, `~`, `!`
    let name: String = spec
        .chars()
        .take_while(|c| !matches!(c, '[' | ';' | '>' | '<' | '=' | '~' | '!'))
        .collect();
    let name = name.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

// ---------------------------------------------------------------------------
// Python: requirements.txt
// ---------------------------------------------------------------------------

/// Parse a `requirements.txt` file.
///
/// Each non-blank, non-comment line is treated as a dependency specifier.
/// PEP 508 version constraints are stripped; the bare package name is used
/// as both the node name and the edge target.
///
/// The synthetic "package" node for the project itself uses the requirements
/// file's parent directory name as its name.
pub fn parse_requirements_txt(
    content: &str,
    manifest_file: &Path,
    root_slug: &str,
) -> (Vec<Node>, Vec<Edge>) {
    let project_name = manifest_file
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("(project)")
        .to_string();

    let deps: Vec<String> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('-'))
        .filter_map(pep508_name)
        .collect();

    if deps.is_empty() {
        let node = make_package_node(&project_name, manifest_file, "python", root_slug);
        return (vec![node], Vec::new());
    }

    emit_package_graph(&project_name, &deps, manifest_file, "python", root_slug)
}

// ---------------------------------------------------------------------------
// Go: go.mod
// ---------------------------------------------------------------------------

/// Parse a `go.mod` file.
///
/// Extracts:
/// - `module <path>` directive → the package node name (last path component)
/// - `require` block (single or multi-line) → DependsOn edges
///
/// Version strings are stripped; only the module path is used as the name.
pub fn parse_go_mod(
    content: &str,
    manifest_file: &Path,
    root_slug: &str,
) -> (Vec<Node>, Vec<Edge>) {
    let mut module_name = String::from("(unknown)");
    let mut deps: Vec<String> = Vec::new();

    let mut in_require_block = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(stripped) = trimmed.strip_prefix("module ") {
            let path = stripped.trim();
            // Use the full module path as the node name (e.g. "github.com/foo/bar").
            module_name = path.to_string();
        } else if trimmed == "require (" {
            in_require_block = true;
        } else if in_require_block {
            if trimmed == ")" {
                in_require_block = false;
            } else {
                // Each line: `<module-path> vX.Y.Z` or `<module-path> vX.Y.Z // indirect`
                if let Some(dep) = parse_go_require_line(trimmed) {
                    deps.push(dep);
                }
            }
        } else if let Some(require_rest) = trimmed.strip_prefix("require ") {
            // Single-line require: `require github.com/foo/bar v1.0.0`
            let rest = require_rest.trim();
            if let Some(dep) = parse_go_require_line(rest) {
                deps.push(dep);
            }
        }
    }

    if deps.is_empty() {
        let node = make_package_node(&module_name, manifest_file, "go", root_slug);
        return (vec![node], Vec::new());
    }

    emit_package_graph(&module_name, &deps, manifest_file, "go", root_slug)
}

/// Extract the module path from a single `require` line.
/// Returns `None` for blank lines, comments, or unparseable entries.
fn parse_go_require_line(line: &str) -> Option<String> {
    // Strip inline comments (`// indirect`, etc.)
    let line = if let Some(pos) = line.find("//") {
        line[..pos].trim()
    } else {
        line.trim()
    };

    if line.is_empty() {
        return None;
    }

    // Split on whitespace: first token is the module path, second is the version.
    let module_path = line.split_whitespace().next()?;
    if module_path.is_empty() {
        None
    } else {
        Some(module_path.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::EdgeKind;
    use std::path::PathBuf;

    fn manifest(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    // -----------------------------------------------------------------------
    // package.json
    // -----------------------------------------------------------------------

    #[test]
    fn test_package_json_basic() {
        let content = r#"{
            "name": "my-app",
            "dependencies": {
                "react": "^18.0.0",
                "lodash": "^4.0.0"
            }
        }"#;
        let (nodes, edges) = parse_package_json(content, &manifest("package.json"), "root");

        assert!(
            nodes.iter().any(|n| n.id.name == "my-app"),
            "package node missing"
        );
        assert!(
            nodes.iter().any(|n| n.id.name == "react"),
            "react dep node missing"
        );
        assert!(
            nodes.iter().any(|n| n.id.name == "lodash"),
            "lodash dep node missing"
        );
        assert_eq!(edges.len(), 2, "expected 2 DependsOn edges");
        assert!(edges.iter().all(|e| e.from.name == "my-app"));
        assert!(edges.iter().all(|e| e.kind == EdgeKind::DependsOn));
    }

    #[test]
    fn test_package_json_dev_dependencies() {
        let content = r#"{
            "name": "my-lib",
            "devDependencies": {
                "typescript": "^5.0.0",
                "jest": "^29.0.0"
            }
        }"#;
        let (nodes, edges) = parse_package_json(content, &manifest("package.json"), "root");

        assert!(nodes.iter().any(|n| n.id.name == "typescript"));
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn test_package_json_no_name_uses_fallback() {
        let content = r#"{"dependencies": {"axios": "^1.0"}}"#;
        let (nodes, edges) = parse_package_json(content, &manifest("package.json"), "root");
        // Fallback: parent dir of "package.json" is "" so the fallback is "(unknown)"
        assert!(!nodes.is_empty());
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn test_package_json_no_dependencies_emits_node_only() {
        let content = r#"{"name": "empty-pkg"}"#;
        let (nodes, edges) = parse_package_json(content, &manifest("package.json"), "root");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id.name, "empty-pkg");
        assert!(edges.is_empty());
    }

    #[test]
    fn test_package_json_invalid_json_returns_empty() {
        let (nodes, edges) = parse_package_json("not json", &manifest("package.json"), "root");
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
    }

    // -----------------------------------------------------------------------
    // Cargo.toml
    // -----------------------------------------------------------------------

    #[test]
    fn test_cargo_toml_git_dependency_metadata() {
        let content = r#"
[package]
name = "unified-hifi-control"
repository = "https://github.com/open-horizon-labs/unified-hifi-control"

[dependencies]
roon-api = { git = "https://github.com/open-horizon-labs/rust-roon-api.git", branch = "ohc/main", features = ["transport"] }
serde = "1"

[target.'cfg(unix)'.dependencies]
libc = "0.2"
"#;
        let (nodes, edges) = parse_cargo_toml(content, &manifest("Cargo.toml"), "root");

        assert!(
            nodes
                .iter()
                .any(|node| node.id.name == "unified-hifi-control"
                    && node.metadata.get("repository").map(String::as_str)
                        == Some("https://github.com/open-horizon-labs/unified-hifi-control"))
        );
        let roon_dep = nodes
            .iter()
            .find(|node| node.id.name == "roon-api")
            .expect("roon-api dependency node");
        assert_eq!(
            roon_dep.metadata.get("git").map(String::as_str),
            Some("https://github.com/open-horizon-labs/rust-roon-api.git")
        );
        assert_eq!(
            roon_dep.metadata.get("branch").map(String::as_str),
            Some("ohc/main")
        );
        assert!(nodes.iter().any(|node| node.id.name == "serde"));
        assert!(nodes.iter().any(|node| node.id.name == "libc"));
        assert_eq!(edges.len(), 3);
    }

    #[test]
    fn test_workspace_only_cargo_toml_emits_no_package_or_edges() {
        let content = r#"
[workspace]
members = ["crates/app"]

[workspace.dependencies]
serde = "1"
tokio = "1"
"#;
        let (nodes, edges) = parse_cargo_toml(content, &manifest("Cargo.toml"), "root");

        assert!(
            nodes.is_empty(),
            "workspace-only manifests are not packages"
        );
        assert!(
            edges.is_empty(),
            "workspace.dependencies must not become package dependency edges"
        );
    }

    #[test]
    fn test_package_cargo_toml_ignores_workspace_dependencies() {
        let content = r#"
[package]
name = "app"

[dependencies]
serde = "1"

[workspace.dependencies]
tokio = "1"
parking_lot = "1"
"#;
        let (nodes, edges) = parse_cargo_toml(content, &manifest("Cargo.toml"), "root");

        assert!(nodes.iter().any(|node| node.id.name == "serde"));
        assert!(!nodes.iter().any(|node| node.id.name == "tokio"));
        assert!(!nodes.iter().any(|node| node.id.name == "parking_lot"));
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn test_cargo_toml_preserves_multiple_aliases_for_same_package() {
        let content = r#"
[package]
name = "app"

[dependencies]
serde_v1 = { package = "serde", version = "1" }
serde_alt = { package = "serde", version = "1" }
"#;
        let (nodes, edges) = parse_cargo_toml(content, &manifest("Cargo.toml"), "root");

        let serde_nodes: Vec<_> = nodes
            .iter()
            .filter(|node| node.id.name == "serde")
            .collect();
        assert_eq!(serde_nodes.len(), 1);
        assert_eq!(
            serde_nodes[0]
                .metadata
                .get("dependency_aliases")
                .map(String::as_str),
            Some("serde_alt,serde_v1")
        );
        assert_eq!(
            edges.iter().filter(|edge| edge.to.name == "serde").count(),
            1,
            "aliases to the same package should not emit duplicate package edges"
        );
    }

    #[test]
    fn test_explicit_package_host_adds_cross_root_edge() {
        let mut hosts = PackageHosts::new();
        hosts.insert(
            ("hiphi-repos".to_string(), "roon-api".to_string()),
            "rust-roon-api".to_string(),
        );

        let (mut app_nodes, mut edges) = parse_cargo_toml(
            r#"
[package]
name = "unified-hifi-control"

[dependencies]
roon-api = { git = "https://github.com/open-horizon-labs/rust-roon-api.git" }
"#,
            &manifest("unified-hifi-control/Cargo.toml"),
            "hiphi-repos",
        );
        let (provider_nodes, _provider_edges) = parse_cargo_toml(
            r#"
[package]
name = "roon-api"
repository = "https://github.com/open-horizon-labs/rust-roon-api"
"#,
            &manifest("rust-roon-api/Cargo.toml"),
            "hiphi-repos",
        );
        app_nodes.extend(provider_nodes);

        add_explicit_package_host_edges(&app_nodes, &mut edges, &hosts);

        assert!(
            edges
                .iter()
                .any(|edge| edge.from.name == "unified-hifi-control"
                    && edge.to.name == "roon-api"
                    && edge.to.file == manifest("rust-roon-api/Cargo.toml")
                    && edge.confidence == Confidence::Confirmed)
        );
    }

    #[test]
    fn test_explicit_package_host_matches_nested_directory() {
        let mut hosts = PackageHosts::new();
        hosts.insert(
            ("hiphi-repos".to_string(), "roon-api".to_string()),
            "libs/rust-roon-api".to_string(),
        );

        let (mut app_nodes, mut edges) = parse_cargo_toml(
            r#"
[package]
name = "unified-hifi-control"

[dependencies]
roon-api = "1"
"#,
            &manifest("unified-hifi-control/Cargo.toml"),
            "hiphi-repos",
        );
        let (provider_nodes, _) = parse_cargo_toml(
            r#"
[package]
name = "roon-api"
"#,
            &manifest("libs/rust-roon-api/Cargo.toml"),
            "hiphi-repos",
        );
        app_nodes.extend(provider_nodes);

        add_explicit_package_host_edges(&app_nodes, &mut edges, &hosts);

        assert!(
            edges.iter().any(|edge| edge.to.name == "roon-api"
                && edge.to.file == manifest("libs/rust-roon-api/Cargo.toml")
                && edge.confidence == Confidence::Confirmed),
            "nested package host directories should match provider manifest paths"
        );
    }

    #[test]
    fn test_explicit_package_hosts_are_scoped_to_declaring_root() {
        let mut hosts = PackageHosts::new();
        hosts.insert(
            ("app-a".to_string(), "shared-lib".to_string()),
            "providers/a".to_string(),
        );
        hosts.insert(
            ("app-b".to_string(), "shared-lib".to_string()),
            "providers/b".to_string(),
        );

        let (mut nodes, mut edges) = parse_cargo_toml(
            r#"
[package]
name = "app-a"

[dependencies]
shared-lib = "1"
"#,
            &manifest("app-a/Cargo.toml"),
            "app-a",
        );
        let (app_b_nodes, app_b_edges) = parse_cargo_toml(
            r#"
[package]
name = "app-b"

[dependencies]
shared-lib = "1"
"#,
            &manifest("app-b/Cargo.toml"),
            "app-b",
        );
        let (provider_a_nodes, _) = parse_cargo_toml(
            r#"
[package]
name = "shared-lib"
"#,
            &manifest("providers/a/Cargo.toml"),
            "repo-family",
        );
        let (provider_b_nodes, _) = parse_cargo_toml(
            r#"
[package]
name = "shared-lib"
"#,
            &manifest("providers/b/Cargo.toml"),
            "repo-family",
        );
        nodes.extend(app_b_nodes);
        nodes.extend(provider_a_nodes);
        nodes.extend(provider_b_nodes);
        edges.extend(app_b_edges);

        add_explicit_package_host_edges(&nodes, &mut edges, &hosts);

        assert!(edges.iter().any(|edge| edge.from.root == "app-a"
            && edge.to.file == manifest("providers/a/Cargo.toml")
            && edge.confidence == Confidence::Confirmed));
        assert!(edges.iter().any(|edge| edge.from.root == "app-b"
            && edge.to.file == manifest("providers/b/Cargo.toml")
            && edge.confidence == Confidence::Confirmed));
        assert!(!edges.iter().any(|edge| edge.from.root == "app-a"
            && edge.to.file == manifest("providers/b/Cargo.toml")
            && edge.confidence == Confidence::Confirmed));
    }

    #[test]
    fn test_explicit_package_host_prefers_declared_root_over_primary_directory_copy() {
        let mut hosts = PackageHosts::new();
        hosts.insert(
            ("hiphi-repos".to_string(), "roon-api".to_string()),
            "rust-roon-api".to_string(),
        );

        let (_app_nodes, mut edges) = parse_cargo_toml(
            r#"
[package]
name = "unified-hifi-control"

[dependencies]
roon-api = { git = "https://github.com/open-horizon-labs/rust-roon-api.git" }
"#,
            &manifest("unified-hifi-control/Cargo.toml"),
            "hiphi-repos",
        );
        let (primary_provider_nodes, _) = parse_cargo_toml(
            r#"
[package]
name = "roon-api"
"#,
            &manifest("rust-roon-api/Cargo.toml"),
            "hiphi-repos",
        );
        let (declared_provider_nodes, _) = parse_cargo_toml(
            r#"
[package]
name = "roon-api"
"#,
            &manifest("Cargo.toml"),
            "rust-roon-api",
        );
        let mut nodes = _app_nodes;
        nodes.extend(primary_provider_nodes);
        nodes.extend(declared_provider_nodes);

        add_explicit_package_host_edges(&nodes, &mut edges, &hosts);

        assert!(
            edges
                .iter()
                .any(|edge| edge.from.name == "unified-hifi-control"
                    && edge.to.root == "rust-roon-api"
                    && edge.to.name == "roon-api"
                    && edge.confidence == Confidence::Confirmed)
        );
    }

    #[test]
    fn test_package_host_is_explicit_not_unique_name_guess() {
        let (_app_nodes, mut edges) = parse_cargo_toml(
            r#"
[package]
name = "app"

[dependencies]
local-lib = "1"
"#,
            &manifest("app/Cargo.toml"),
            "root",
        );
        let (mut nodes, _) = parse_cargo_toml(
            r#"
[package]
name = "app"

[dependencies]
local-lib = "1"
"#,
            &manifest("app/Cargo.toml"),
            "root",
        );
        let (provider_nodes, _) = parse_cargo_toml(
            r#"
[package]
name = "local-lib"
"#,
            &manifest("local-lib/Cargo.toml"),
            "root",
        );
        nodes.extend(provider_nodes);

        add_explicit_package_host_edges(&nodes, &mut edges, &PackageHosts::new());

        assert!(
            !edges
                .iter()
                .any(|edge| edge.to.file == manifest("local-lib/Cargo.toml"))
        );
    }

    // -----------------------------------------------------------------------
    // pyproject.toml
    // -----------------------------------------------------------------------

    #[test]
    fn test_pyproject_toml_basic() {
        let content = r#"
[project]
name = "my-python-pkg"
dependencies = [
    "requests>=2.0",
    "click>=7.0",
]
"#;
        let (nodes, edges) = parse_pyproject_toml(content, &manifest("pyproject.toml"), "root");

        assert!(nodes.iter().any(|n| n.id.name == "my-python-pkg"));
        assert!(nodes.iter().any(|n| n.id.name == "requests"));
        assert!(nodes.iter().any(|n| n.id.name == "click"));
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|e| e.from.name == "my-python-pkg"));
    }

    #[test]
    fn test_pyproject_toml_inline_array() {
        let content = r#"
[project]
name = "pkg"
dependencies = ["fastapi>=0.100", "uvicorn"]
"#;
        let (_nodes, edges) = parse_pyproject_toml(content, &manifest("pyproject.toml"), "root");
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn test_pyproject_toml_no_dependencies() {
        let content = r#"
[project]
name = "solo"
"#;
        let (nodes, edges) = parse_pyproject_toml(content, &manifest("pyproject.toml"), "root");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id.name, "solo");
        assert!(edges.is_empty());
    }

    #[test]
    fn test_pep508_name_strips_version() {
        assert_eq!(pep508_name("requests>=2.0").as_deref(), Some("requests"));
        assert_eq!(
            pep508_name("Django [security] >= 2.0").as_deref(),
            Some("Django")
        );
        assert_eq!(pep508_name("pkg-name").as_deref(), Some("pkg-name"));
        assert_eq!(pep508_name("").as_deref(), None);
    }

    // -----------------------------------------------------------------------
    // requirements.txt
    // -----------------------------------------------------------------------

    #[test]
    fn test_requirements_txt_basic() {
        let content = "requests>=2.0\nclick\n# a comment\n\nflask==2.3";
        let (nodes, edges) = parse_requirements_txt(content, &manifest("requirements.txt"), "root");

        assert!(nodes.iter().any(|n| n.id.name == "requests"));
        assert!(nodes.iter().any(|n| n.id.name == "click"));
        assert!(nodes.iter().any(|n| n.id.name == "flask"));
        assert_eq!(edges.len(), 3);
    }

    #[test]
    fn test_requirements_txt_skips_flags() {
        // Lines starting with `-` (like `-r other.txt`) should be skipped
        let content = "-r base.txt\nrequests\n";
        let (nodes, edges) = parse_requirements_txt(content, &manifest("requirements.txt"), "root");
        assert!(nodes.iter().any(|n| n.id.name == "requests"));
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn test_requirements_txt_empty_emits_project_node() {
        let content = "# just a comment\n";
        let (nodes, edges) = parse_requirements_txt(content, &manifest("requirements.txt"), "root");
        assert_eq!(nodes.len(), 1);
        assert!(edges.is_empty());
    }

    // -----------------------------------------------------------------------
    // go.mod
    // -----------------------------------------------------------------------

    #[test]
    fn test_go_mod_basic() {
        let content = r#"module github.com/myorg/myapp

go 1.21

require (
    github.com/gin-gonic/gin v1.9.1
    github.com/stretchr/testify v1.8.4 // indirect
)
"#;
        let (nodes, edges) = parse_go_mod(content, &manifest("go.mod"), "root");

        assert!(
            nodes.iter().any(|n| n.id.name == "github.com/myorg/myapp"),
            "module node missing"
        );
        assert!(
            nodes
                .iter()
                .any(|n| n.id.name == "github.com/gin-gonic/gin")
        );
        assert!(
            nodes
                .iter()
                .any(|n| n.id.name == "github.com/stretchr/testify")
        );
        assert_eq!(edges.len(), 2);
        assert!(
            edges
                .iter()
                .all(|e| e.from.name == "github.com/myorg/myapp")
        );
    }

    #[test]
    fn test_go_mod_single_require() {
        let content = r#"module example.com/foo

require golang.org/x/text v0.14.0
"#;
        let (nodes, edges) = parse_go_mod(content, &manifest("go.mod"), "root");
        assert!(nodes.iter().any(|n| n.id.name == "example.com/foo"));
        assert!(nodes.iter().any(|n| n.id.name == "golang.org/x/text"));
        assert_eq!(edges.len(), 1);
    }

    #[test]
    fn test_go_mod_no_require_emits_module_node() {
        let content = "module example.com/minimal\n\ngo 1.20\n";
        let (nodes, edges) = parse_go_mod(content, &manifest("go.mod"), "root");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id.name, "example.com/minimal");
        assert!(edges.is_empty());
    }

    #[test]
    fn test_go_mod_indirect_deps_included() {
        let content = r#"module example.com/app

require (
    github.com/direct v1.0.0
    github.com/indirect v2.0.0 // indirect
)
"#;
        let (_nodes, edges) = parse_go_mod(content, &manifest("go.mod"), "root");
        assert_eq!(edges.len(), 2, "indirect deps should be included");
    }

    // -----------------------------------------------------------------------
    // Edge/node properties
    // -----------------------------------------------------------------------

    #[test]
    fn test_package_node_kind_is_other_package() {
        let content = r#"{"name": "my-pkg", "dependencies": {"dep": "1.0"}}"#;
        let (nodes, _) = parse_package_json(content, &manifest("package.json"), "root");
        for node in &nodes {
            assert_eq!(node.id.kind, NodeKind::Other("package".to_string()));
        }
    }

    #[test]
    fn test_edge_kind_is_depends_on() {
        let content = r#"{"name": "a", "dependencies": {"b": "1.0"}}"#;
        let (_, edges) = parse_package_json(content, &manifest("package.json"), "root");
        assert_eq!(edges[0].kind, EdgeKind::DependsOn);
    }

    #[test]
    fn test_root_slug_propagated() {
        let content = r#"{"name": "pkg", "dependencies": {"dep": "1.0"}}"#;
        let (nodes, edges) = parse_package_json(content, &manifest("package.json"), "myroot");
        assert!(nodes.iter().all(|n| n.id.root == "myroot"));
        assert!(
            edges
                .iter()
                .all(|e| e.from.root == "myroot" && e.to.root == "myroot")
        );
    }

    // -----------------------------------------------------------------------
    // Adversarial tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_package_json_both_dep_types() {
        let content = r#"{
            "name": "full",
            "dependencies": {"react": "18"},
            "devDependencies": {"jest": "29"}
        }"#;
        let (_, edges) = parse_package_json(content, &manifest("package.json"), "root");
        assert_eq!(
            edges.len(),
            2,
            "should include both dependencies and devDependencies"
        );
    }

    #[test]
    fn test_go_mod_empty_require_block() {
        let content = "module example.com/foo\n\nrequire (\n)\n";
        let (nodes, edges) = parse_go_mod(content, &manifest("go.mod"), "root");
        assert_eq!(nodes.len(), 1);
        assert!(edges.is_empty());
    }

    #[test]
    fn test_pyproject_toml_pep508_extras() {
        // "Pillow[jpeg]>=9.0" should strip to "Pillow"
        let content = r#"
[project]
name = "img-tool"
dependencies = ["Pillow[jpeg]>=9.0"]
"#;
        let (nodes, edges) = parse_pyproject_toml(content, &manifest("pyproject.toml"), "root");
        assert!(
            nodes.iter().any(|n| n.id.name == "Pillow"),
            "extras should be stripped"
        );
        assert_eq!(edges.len(), 1);
    }

    /// Poetry projects often have [tool.poetry] before [project].
    /// extract_toml_string must not pick up [tool.poetry].name as the package name.
    #[test]
    fn test_pyproject_toml_poetry_section_before_project_uses_correct_name() {
        let content = r#"
[tool.poetry]
name = "poetry-name"
version = "1.0"

[project]
name = "pep621-name"
dependencies = ["requests"]
"#;
        let (nodes, _) = parse_pyproject_toml(content, &manifest("pyproject.toml"), "root");
        assert!(
            nodes.iter().any(|n| n.id.name == "pep621-name"),
            "should use [project].name, not [tool.poetry].name"
        );
        assert!(
            !nodes.iter().any(|n| n.id.name == "poetry-name"),
            "should not pick up [tool.poetry].name"
        );
    }

    /// Ensure [project].dependencies is not confused with [tool.foo].dependencies.
    #[test]
    fn test_pyproject_toml_dependencies_only_from_project_section() {
        let content = r#"
[tool.foo]
dependencies = ["wrong-dep"]

[project]
name = "mypkg"
dependencies = ["correct-dep"]
"#;
        let (_, edges) = parse_pyproject_toml(content, &manifest("pyproject.toml"), "root");
        assert_eq!(edges.len(), 1, "only [project].dependencies should be used");
        assert_eq!(edges[0].to.name, "correct-dep");
    }
}
