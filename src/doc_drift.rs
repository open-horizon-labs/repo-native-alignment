//! On-demand documentation-drift verifier (#891).
//!
//! Flags markdown references to code that no longer resolve:
//! - **class (a)** `path/to/file.ext:N` line pointers (in prose, code spans, or link text)
//!   where the file is missing, or exists with fewer than `N` lines;
//! - **class (b)** a backticked symbol with enough context to bind it to a file
//!   (a bare-path or file:line reference to an existing, indexed file elsewhere in the
//!   same heading section) where that exact symbol name is absent from the file;
//! - **class (c)** bare file-path references in prose (not proper `[text](dest)` link
//!   destinations -- those are already existence-checked by `emit_link_edges` in
//!   `src/extract/markdown.rs`; this module does not duplicate that).
//!
//! Runs on-demand against the already-built graph snapshot (`search(mode="doc_drift")`);
//! it is not part of `scan` and persists nothing.
//!
//! # Conservative classification
//!
//! Every candidate resolves to exactly one of:
//! - **proven_dead**: positive filesystem or graph evidence the reference no
//!   longer resolves. Recorded as a [`DriftFinding`] and reported as drift.
//! - **unresolvable**: no location context, an excluded/unindexed target file,
//!   or an ambiguous path. Counted in [`DocDriftReport::unresolvable`] and
//!   **never** reported as drift.
//!
//! Reuses the metadata convention established by the markdown heading-anchor
//! diagnostic (`content.unresolved_anchor`, `src/extract/markdown.rs:750`):
//! `diagnostic_code` / `diagnostic_severity` / `diagnostic_message`. Because this
//! pass is on-demand rather than persisted, findings are rendered with that same
//! vocabulary rather than written back onto graph nodes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::graph::{Node, NodeKind};

/// Diagnostic code for all dead-code-anchor findings, per the established
/// `content.*` convention (sibling of `content.unresolved_anchor` /
/// `content.duplicate_anchor`).
pub const DIAGNOSTIC_CODE: &str = "content.dead_code_anchor";

/// File extensions eligible for path-shaped reference detection. Deliberately
/// conservative and code/doc-centric -- broadening this list raises the false
/// positive rate on prose that merely contains a `word.word` pattern.
const PATH_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "rb", "java", "c", "h", "hpp", "cpp", "cc", "md",
    "mdx", "toml", "yaml", "yml", "json", "sh", "proto", "sql",
];

fn path_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let ext_alt = PATH_EXTENSIONS.join("|");
        // No `\b` at the start: a word boundary never exists between a space
        // and a `.`, so `\b` silently dropped the leading dot of `.oh/...` and
        // `.claude/...` paths and then declared the dotless path dead (a false
        // positive found by the live run on this repo). Anchor on a one-char
        // non-path prefix instead; the path is capture group 1.
        Regex::new(&format!(
            r"(?:^|[^[:alnum:]_./\-])((?:[[:alnum:]_.\-]+/)+[[:alnum:]_.\-]+\.(?:{ext_alt}))(?::([0-9]+))?\b"
        ))
        .expect("static regex must compile")
    })
}

/// Matches a `[text](dest)` markdown link so its destination can be masked out
/// of prose scanning -- proper link destinations are already existence-checked
/// by `emit_link_edges` and must not be duplicated here. The bracketed link
/// *text* is intentionally left unmasked (class (a) explicitly covers link text).
fn link_dest_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\]\([^)]*\)").expect("static regex must compile"))
}

fn code_span_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`([^`\n]+)`").expect("static regex must compile"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftClass {
    FileLine,
    Symbol,
    BarePath,
}

impl DriftClass {
    fn label(self) -> &'static str {
        match self {
            DriftClass::FileLine => "file:line",
            DriftClass::Symbol => "symbol",
            DriftClass::BarePath => "bare-path",
        }
    }
}

/// A confirmed drift finding (i.e. a candidate classified `proven_dead`).
#[derive(Debug, Clone)]
pub struct DriftFinding {
    /// Root-relative path of the markdown file containing the reference.
    pub markdown_file: PathBuf,
    /// Root slug of the markdown file.
    pub markdown_root: String,
    /// 1-based line in the markdown file where the reference occurs.
    pub line: usize,
    pub class: DriftClass,
    /// The exact matched reference text (e.g. `src/foo.rs:123`, `` `OldStruct` ``).
    pub reference: String,
    pub diagnostic_code: &'static str,
    pub severity: &'static str,
    pub message: String,
}

/// Summary + findings for a single `doc_drift` run.
#[derive(Debug, Default)]
pub struct DocDriftReport {
    pub findings: Vec<DriftFinding>,
    pub chunks_scanned: usize,
    pub files_scanned: usize,
    /// Total candidates considered, broken out by class.
    pub candidates_file_line: usize,
    pub candidates_symbol: usize,
    pub candidates_bare_path: usize,
    /// Candidates that classified `unresolvable` (never reported as drift).
    pub unresolvable: usize,
    pub elapsed: Duration,
}

impl DocDriftReport {
    pub fn candidates_total(&self) -> usize {
        self.candidates_file_line + self.candidates_symbol + self.candidates_bare_path
    }
}

/// Cache of per-file line counts, avoiding re-reading a file for every
/// reference to it within the same run.
struct FileCache {
    /// `None` means "checked, does not exist / unreadable"; `Some(n)` is the
    /// line count for a file that exists and was read successfully.
    line_counts: HashMap<PathBuf, Option<usize>>,
}

impl FileCache {
    fn new() -> Self {
        Self {
            line_counts: HashMap::new(),
        }
    }

    /// Returns `Some(line_count)` if `absolute` exists and is readable as UTF-8
    /// text, `None` otherwise (missing file, or unreadable/binary content --
    /// the latter is rare given the extension allowlist and is treated the
    /// same as "cannot verify", which keeps the caller in the unresolvable
    /// bucket rather than guessing).
    fn line_count(&mut self, absolute: &Path) -> Option<usize> {
        *self
            .line_counts
            .entry(absolute.to_path_buf())
            .or_insert_with(|| {
                std::fs::read_to_string(absolute)
                    .ok()
                    .map(|s| s.lines().count())
            })
    }

    fn exists(&mut self, absolute: &Path) -> bool {
        self.line_counts
            .get(absolute)
            .map(|v| v.is_some())
            .unwrap_or_else(|| absolute.is_file())
    }
}

/// Where a path-shaped reference resolved, for binding backticked symbols to a
/// concrete file within the same heading section.
enum PathResolution {
    /// Resolved to exactly one existing file, under the given root slug.
    Existing { root: String, absolute: PathBuf },
    /// Did not exist under any known root.
    Missing,
    /// No location context (couldn't even attempt resolution), or the path
    /// existed under 2+ roots simultaneously (ambiguous).
    Unresolvable,
}

fn resolve_candidate_path(
    path_text: &str,
    home_root: &str,
    root_paths: &HashMap<String, PathBuf>,
    cache: &mut FileCache,
) -> PathResolution {
    if root_paths.is_empty() {
        return PathResolution::Unresolvable;
    }
    // Reject obviously-unsafe/relative-escaping paths; conservative --
    // never guess at a target outside any known root.
    if Path::new(path_text)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
        || path_text.starts_with('/')
    {
        return PathResolution::Unresolvable;
    }

    // The doc's own root takes precedence -- if the file exists there, that's
    // the intended target regardless of what other roots contain.
    if let Some(home_path) = root_paths.get(home_root) {
        let candidate = home_path.join(path_text);
        if cache.exists(&candidate) {
            return PathResolution::Existing {
                root: home_root.to_string(),
                absolute: candidate,
            };
        }
    }

    let mut hits: Vec<(String, PathBuf)> = Vec::new();
    for (root, base) in root_paths {
        if root == home_root {
            continue;
        }
        let candidate = base.join(path_text);
        if cache.exists(&candidate) {
            hits.push((root.clone(), candidate));
        }
    }

    match hits.len() {
        0 => PathResolution::Missing,
        1 => {
            let (root, absolute) = hits.into_iter().next().unwrap();
            PathResolution::Existing { root, absolute }
        }
        _ => PathResolution::Unresolvable,
    }
}

/// Whether a backticked span "looks like" a Rust-ish symbol worth checking
/// against the graph, as opposed to ordinary prose emphasized with backticks.
/// Deliberately strict: bare lowercase words with no separator (`true`,
/// `config`, `note`) are never treated as symbol candidates, because they are
/// the dominant false-positive class for backtick-as-emphasis prose. This
/// means a single-word snake-case-free identifier that happens to be a real
/// dead symbol will be missed -- an explicit, accepted limitation in trade for
/// not flagging prose.
fn looks_like_symbol(span: &str) -> bool {
    let span = span.trim();
    if span.is_empty() || span.contains(char::is_whitespace) {
        return false;
    }
    // `.iter().find()` is a method chain fragment, not a nameable symbol.
    if span.starts_with('.') {
        return false;
    }
    // A bare filename (`import_calls.rs`, `config.toml`) names a file, not a
    // symbol; class (c) handles paths and a slash-less filename has no root to
    // resolve against, so it is simply not a candidate.
    if let Some((_, ext)) = span.rsplit_once('.')
        && PATH_EXTENSIONS.contains(&ext)
    {
        return false;
    }
    // Path-shaped or already extension-shaped text is handled by class (a)/(c).
    if span.contains('/') {
        return false;
    }
    if span.contains("::") {
        return true;
    }
    if span.ends_with("()") {
        return true;
    }
    let starts_upper = span.chars().next().is_some_and(|c| c.is_ascii_uppercase());
    let alnum_underscore = span
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if !alnum_underscore {
        return false;
    }
    if starts_upper && span.len() >= 3 {
        return true;
    }
    if span.contains('_') && span.len() >= 4 {
        return true;
    }
    false
}

/// Reduce a symbol reference like `Config::new()` or `NodeKind::MarkdownSection`
/// to the leaf identifier used for exact-name graph lookup.
fn leaf_identifier(span: &str) -> &str {
    let trimmed = span.trim_end_matches("()");
    let after_path = trimmed.rsplit("::").next().unwrap_or(trimmed);
    // `receiver.method()` names the method, not the receiver variable.
    after_path.rsplit('.').next().unwrap_or(after_path)
}

/// Byte-range overlap check.
fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// Mask markdown link destinations (`](dest)`) with spaces so prose scanning
/// for classes (a)/(c) does not re-flag proper link syntax already
/// existence-checked by `emit_link_edges`. Link *text* (`[text]`) is left
/// intact, matching the issue's requirement that class (a) also covers
/// file:line references appearing as link text.
fn mask_link_destinations(content: &str) -> String {
    let mut masked = content.to_string();
    for m in link_dest_re().find_iter(content) {
        let range = m.range();
        masked.replace_range(range.clone(), &" ".repeat(range.len()));
    }
    masked
}

fn line_at(content: &str, byte_offset: usize) -> usize {
    content[..byte_offset.min(content.len())]
        .bytes()
        .filter(|&b| b == b'\n')
        .count()
        + 1
}

/// Build the symbol index (`(root, file) -> {names}`) and the set of files
/// known to the graph (i.e. not excluded/unindexed), from already-extracted
/// non-markdown nodes. O(nodes) once per run; all subsequent lookups are O(1)
/// HashMap/HashSet membership checks.
/// `(root, file) -> names defined in that file`, for indexed code files only.
type SymbolIndex = HashMap<(String, PathBuf), HashSet<String>>;

fn build_symbol_index(nodes: &[Node]) -> (SymbolIndex, HashSet<String>) {
    let mut index: SymbolIndex = HashMap::new();
    let mut global_names: HashSet<String> = HashSet::new();
    for node in nodes {
        // Only kinds that are *definitions of code symbols* make a file
        // "indexed" for symbol binding. Synthetic nodes (`Other(..)`: co-change
        // file anchors, diagnostics, frameworks, ...) exist for markdown and
        // shell files too, and treating them as evidence let the live run bind
        // symbols to `docs/*.md` and `scripts/*.sh` and call them dead.
        if !is_code_symbol_kind(&node.id.kind) {
            continue;
        }
        if node.id.file.as_os_str().is_empty() || !is_symbol_bindable_file(&node.id.file) {
            continue;
        }
        index
            .entry((node.id.root.clone(), node.id.file.clone()))
            .or_default()
            .insert(node.id.name.clone());
        global_names.insert(node.id.name.clone());
    }
    (index, global_names)
}

/// Files whose language can *define* the kind of identifier a backticked
/// symbol names. Config, data, and shell files are excluded even though the
/// extractors emit nodes for them (TOML keys, JSON fields, shell functions):
/// the live run bound Rust identifiers like `OnceLock` to `.oh/config.toml`
/// and `CARGO_TARGET_DIR` to `scripts/prep-worktree.sh` and called them dead.
fn is_symbol_bindable_file(path: &Path) -> bool {
    const CODE_EXTENSIONS: &[&str] = &[
        "rs", "py", "pyi", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "kts", "cs",
        "rb", "php", "c", "h", "cc", "cpp", "hpp", "swift", "scala", "proto", "sql",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| CODE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Node kinds whose presence proves a file is indexed as code and whose names
/// are the universe a backticked symbol can be checked against.
fn is_code_symbol_kind(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::Enum
            | NodeKind::TypeAlias
            | NodeKind::Module
            | NodeKind::Const
            | NodeKind::Impl
            | NodeKind::Macro
            | NodeKind::Field
            | NodeKind::EnumVariant
            | NodeKind::ProtoMessage
            | NodeKind::SqlTable
            | NodeKind::ApiEndpoint
    )
}

/// Run the doc-drift verifier over every already-extracted markdown heading
/// section in `nodes`, resolving file/symbol references against `root_paths`
/// (root slug -> absolute filesystem path).
pub fn run_doc_drift(nodes: &[Node], root_paths: &HashMap<String, PathBuf>) -> DocDriftReport {
    let start = Instant::now();
    let (symbol_index, global_names) = build_symbol_index(nodes);
    let mut cache = FileCache::new();
    let mut report = DocDriftReport::default();
    let mut files_seen: HashSet<(String, PathBuf)> = HashSet::new();

    for node in nodes {
        if node.id.kind != NodeKind::MarkdownSection {
            continue;
        }
        // Heading-chunk nodes (the original per-section extraction) carry
        // `heading_level`; the finer-grained body-AST nodes (paragraph, link,
        // image, ...) carry `markdown_kind` instead and would duplicate every
        // finding if also scanned.
        if !node.metadata.contains_key("heading_level") {
            continue;
        }
        report.chunks_scanned += 1;
        files_seen.insert((node.id.root.clone(), node.id.file.clone()));

        let content = &node.body;
        let masked = mask_link_destinations(content);

        // ── classes (a) file:line and (c) bare path ──────────────────────
        let mut path_spans: Vec<(usize, usize)> = Vec::new();
        // (bound file candidates seen in this chunk, for class (b) binding)
        let mut bound_candidates: HashSet<(String, PathBuf)> = HashSet::new();

        for caps in path_line_re().captures_iter(&masked) {
            // Group 0 includes the anchoring prefix char; the reference itself
            // runs from group 1's start to the end of the full match.
            let path_match = caps.get(1).unwrap();
            let ref_start = path_match.start();
            let ref_end = caps.get(0).unwrap().end();
            let ref_text = &masked[ref_start..ref_end];
            path_spans.push((ref_start, ref_end));
            let path_text = path_match.as_str();
            let line_group = caps.get(2);
            // Placeholder paths in documentation examples (`docs/ADRs/001-...md`)
            // are not references to anything; never call them dead.
            if path_text.contains("...") {
                report.unresolvable += 1;
                continue;
            }

            let resolution =
                resolve_candidate_path(path_text, &node.id.root, root_paths, &mut cache);
            let finding_line = node.line_start + line_at(content, ref_start) - 1;

            if let Some(line_group) = line_group {
                report.candidates_file_line += 1;
                let requested_line: usize = match line_group.as_str().parse() {
                    Ok(n) => n,
                    Err(_) => {
                        report.unresolvable += 1;
                        continue;
                    }
                };
                match resolution {
                    PathResolution::Existing { root, absolute } => {
                        bound_candidates.insert((root, PathBuf::from(path_text)));
                        match cache.line_count(&absolute) {
                            Some(total_lines) => {
                                if requested_line > total_lines {
                                    report.findings.push(DriftFinding {
                                        markdown_file: node.id.file.clone(),
                                        markdown_root: node.id.root.clone(),
                                        line: finding_line,
                                        class: DriftClass::FileLine,
                                        reference: ref_text.to_string(),
                                        diagnostic_code: DIAGNOSTIC_CODE,
                                        severity: "error",
                                        message: format!(
                                            "line {requested_line} exceeds file length ({total_lines} lines) in {path_text}"
                                        ),
                                    });
                                }
                                // requested_line <= total_lines: valid, no finding.
                            }
                            None => report.unresolvable += 1,
                        }
                    }
                    PathResolution::Missing => {
                        report.findings.push(DriftFinding {
                            markdown_file: node.id.file.clone(),
                            markdown_root: node.id.root.clone(),
                            line: finding_line,
                            class: DriftClass::FileLine,
                            reference: ref_text.to_string(),
                            diagnostic_code: DIAGNOSTIC_CODE,
                            severity: "error",
                            message: format!("file does not exist: {path_text}"),
                        });
                    }
                    PathResolution::Unresolvable => report.unresolvable += 1,
                }
            } else {
                report.candidates_bare_path += 1;
                match resolution {
                    PathResolution::Existing { root, absolute: _ } => {
                        bound_candidates.insert((root, PathBuf::from(path_text)));
                        // exists: valid, no finding.
                    }
                    PathResolution::Missing => {
                        report.findings.push(DriftFinding {
                            markdown_file: node.id.file.clone(),
                            markdown_root: node.id.root.clone(),
                            line: finding_line,
                            class: DriftClass::BarePath,
                            reference: ref_text.to_string(),
                            diagnostic_code: DIAGNOSTIC_CODE,
                            severity: "error",
                            message: format!("file does not exist: {path_text}"),
                        });
                    }
                    PathResolution::Unresolvable => report.unresolvable += 1,
                }
            }
        }

        // Binding for class (b): exactly one distinct existing+indexed file
        // referenced elsewhere in this same heading section.
        let indexed_bound: Vec<&(String, PathBuf)> = bound_candidates
            .iter()
            .filter(|key| symbol_index.contains_key(*key))
            .collect();
        let binding: Option<&(String, PathBuf)> = match indexed_bound.len() {
            1 => Some(indexed_bound[0]),
            _ => None,
        };

        // ── class (b): backticked symbols ─────────────────────────────────
        for caps in code_span_re().captures_iter(&masked) {
            let whole = caps.get(0).unwrap();
            // Skip spans that overlap an already-classified path/file:line match.
            if path_spans
                .iter()
                .any(|p| overlaps(*p, (whole.start(), whole.end())))
            {
                continue;
            }
            let span_text = caps.get(1).unwrap().as_str();
            if !looks_like_symbol(span_text) {
                continue;
            }
            report.candidates_symbol += 1;
            let finding_line = node.line_start + line_at(content, whole.start()) - 1;

            let Some((bound_root, bound_file)) = binding else {
                report.unresolvable += 1;
                continue;
            };
            let key = (bound_root.clone(), bound_file.clone());
            let Some(names) = symbol_index.get(&key) else {
                // Not indexed (excluded/unsupported extension, or scanner
                // exclude) -- cannot prove absence.
                report.unresolvable += 1;
                continue;
            };
            let leaf = leaf_identifier(span_text);
            if !names.contains(leaf) {
                // Section-level binding is a heuristic. If the name exists in
                // some other indexed file, the doc may simply be talking about
                // that one; that is ambiguity, not proof of death.
                if global_names.contains(leaf) {
                    report.unresolvable += 1;
                    continue;
                }
                report.findings.push(DriftFinding {
                    markdown_file: node.id.file.clone(),
                    markdown_root: node.id.root.clone(),
                    line: finding_line,
                    class: DriftClass::Symbol,
                    reference: whole.as_str().to_string(),
                    diagnostic_code: DIAGNOSTIC_CODE,
                    severity: "error",
                    message: format!("symbol `{leaf}` not found in {}", bound_file.display()),
                });
            }
        }
    }

    report.files_scanned = files_seen.len();
    report.elapsed = start.elapsed();
    report
}

/// Render a `DocDriftReport` as agent/human-readable markdown, matching the
/// style of other `search(mode=...)` traversal reports.
pub fn render_report(report: &DocDriftReport, strip_root: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("## Documentation drift\n\n");
    out.push_str(&format!(
        "Scanned {} heading section(s) across {} markdown file(s) in {:.1}ms.\n",
        report.chunks_scanned,
        report.files_scanned,
        report.elapsed.as_secs_f64() * 1000.0,
    ));
    out.push_str(&format!(
        "Candidates considered: {} (file:line: {}, symbol: {}, bare-path: {}); unresolvable (not drift): {}.\n\n",
        report.candidates_total(),
        report.candidates_file_line,
        report.candidates_symbol,
        report.candidates_bare_path,
        report.unresolvable,
    ));

    if report.findings.is_empty() {
        out.push_str("No drift found.\n");
        return out;
    }

    out.push_str(&format!("### Findings ({})\n\n", report.findings.len()));
    for finding in &report.findings {
        let label = match strip_root {
            Some(root) if finding.markdown_root == root => {
                finding.markdown_file.display().to_string()
            }
            _ => format!(
                "{}:{}",
                finding.markdown_root,
                finding.markdown_file.display()
            ),
        };
        out.push_str(&format!(
            "- [{}] `{}:{}` — {} `{}` — {} ({})\n",
            finding.diagnostic_code,
            label,
            finding.line,
            finding.class.label(),
            finding.reference,
            finding.message,
            finding.severity,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{ExtractionSource, NodeId};
    use std::collections::BTreeMap;

    fn md_node(root: &str, file: &str, name: &str, body: &str, line_start: usize) -> Node {
        let mut metadata = BTreeMap::new();
        metadata.insert("heading_level".to_string(), "1".to_string());
        Node {
            id: NodeId {
                root: root.to_string(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind: NodeKind::MarkdownSection,
            },
            language: "markdown".to_string(),
            line_start,
            line_end: line_start + body.lines().count(),
            signature: String::new(),
            body: body.to_string(),
            metadata,
            source: ExtractionSource::Markdown,
        }
    }

    fn code_node(root: &str, file: &str, name: &str, kind: NodeKind) -> Node {
        Node {
            id: NodeId {
                root: root.to_string(),
                file: PathBuf::from(file),
                name: name.to_string(),
                kind,
            },
            language: "rust".to_string(),
            line_start: 1,
            line_end: 1,
            signature: String::new(),
            body: String::new(),
            metadata: BTreeMap::new(),
            source: ExtractionSource::TreeSitter,
        }
    }

    fn write_file(dir: &Path, rel: &str, contents: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn tmp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "doc-drift-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ── class (a): file:line ─────────────────────────────────────────────

    #[test]
    fn file_line_missing_file_is_proven_dead() {
        let root_dir = tmp_root();
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let nodes = vec![md_node(
            "main",
            ".oh/notes.md",
            "Notes",
            "See `src/gone.rs:10` for details.",
            1,
        )];
        let report = run_doc_drift(&nodes, &root_paths);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].class, DriftClass::FileLine);
        assert!(report.findings[0].message.contains("does not exist"));
    }

    #[test]
    fn file_line_out_of_range_is_distinguished_from_missing() {
        let root_dir = tmp_root();
        write_file(&root_dir, "src/small.rs", "line1\nline2\nline3\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let nodes = vec![md_node(
            "main",
            ".oh/notes.md",
            "Notes",
            "See src/small.rs:99 for details.",
            1,
        )];
        let report = run_doc_drift(&nodes, &root_paths);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].class, DriftClass::FileLine);
        assert!(report.findings[0].message.contains("exceeds file length"));
    }

    /// Off-by-one boundary: a reference to exactly the file's last line must
    /// NOT be flagged.
    #[test]
    fn file_line_at_exact_last_line_does_not_flag() {
        let root_dir = tmp_root();
        write_file(&root_dir, "src/small.rs", "line1\nline2\nline3\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let nodes = vec![md_node(
            "main",
            ".oh/notes.md",
            "Notes",
            "See src/small.rs:3 for details.",
            1,
        )];
        let report = run_doc_drift(&nodes, &root_paths);
        assert!(
            report.findings.is_empty(),
            "expected no findings, got {:?}",
            report.findings
        );
        assert_eq!(report.candidates_file_line, 1);
    }

    // ── class (c): bare path ──────────────────────────────────────────────

    #[test]
    fn bare_path_missing_file_is_proven_dead() {
        let root_dir = tmp_root();
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let nodes = vec![md_node(
            "main",
            "README.md",
            "Preamble",
            "Config lives in src/config/old_settings.rs now.",
            1,
        )];
        let report = run_doc_drift(&nodes, &root_paths);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].class, DriftClass::BarePath);
    }

    #[test]
    fn bare_path_existing_file_is_not_flagged() {
        let root_dir = tmp_root();
        write_file(&root_dir, "src/config.rs", "pub struct Config;\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let nodes = vec![md_node(
            "main",
            "README.md",
            "Preamble",
            "Config lives in src/config.rs now.",
            1,
        )];
        let report = run_doc_drift(&nodes, &root_paths);
        assert!(report.findings.is_empty());
        assert_eq!(report.candidates_bare_path, 1);
    }

    // ── link destinations are not duplicated ──────────────────────────────

    #[test]
    fn proper_link_destination_is_not_rechecked() {
        let root_dir = tmp_root();
        // note: destination file intentionally does not exist -- emit_link_edges
        // already existence-checks proper link syntax; this module must not
        // duplicate that (no finding should be produced here).
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let nodes = vec![md_node(
            "main",
            "README.md",
            "Preamble",
            "See [the gone file](src/gone.rs) for details.",
            1,
        )];
        let report = run_doc_drift(&nodes, &root_paths);
        assert!(report.findings.is_empty());
        assert_eq!(report.candidates_bare_path, 0);
    }

    #[test]
    fn file_line_in_link_text_is_still_checked() {
        let root_dir = tmp_root();
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let nodes = vec![md_node(
            "main",
            "README.md",
            "Preamble",
            "See [src/gone.rs:10](other.md#anchor) for details.",
            1,
        )];
        let report = run_doc_drift(&nodes, &root_paths);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].class, DriftClass::FileLine);
    }

    // ── class (b): backticked symbols ──────────────────────────────────────

    #[test]
    fn bound_symbol_missing_from_file_is_proven_dead() {
        let root_dir = tmp_root();
        write_file(&root_dir, "src/config.rs", "pub struct Config;\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let mut nodes = vec![md_node(
            "main",
            ".oh/notes.md",
            "Notes",
            "In `src/config.rs`, the `OldStruct` type controls parsing.",
            1,
        )];
        nodes.push(code_node(
            "main",
            "src/config.rs",
            "Config",
            NodeKind::Struct,
        ));
        let report = run_doc_drift(&nodes, &root_paths);
        assert_eq!(report.candidates_symbol, 1);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].class, DriftClass::Symbol);
    }

    #[test]
    fn bound_symbol_present_in_file_is_not_flagged() {
        let root_dir = tmp_root();
        write_file(&root_dir, "src/config.rs", "pub struct Config;\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let mut nodes = vec![md_node(
            "main",
            ".oh/notes.md",
            "Notes",
            "In `src/config.rs`, the `Config` type controls parsing.",
            1,
        )];
        nodes.push(code_node(
            "main",
            "src/config.rs",
            "Config",
            NodeKind::Struct,
        ));
        let report = run_doc_drift(&nodes, &root_paths);
        assert_eq!(report.candidates_symbol, 1);
        assert!(report.findings.is_empty());
    }

    /// Renamed symbol: the file now has a *different* symbol at the same
    /// spot. Must not be treated as "still resolving" just because the file
    /// has some symbol -- the exact name must match.
    #[test]
    fn renamed_symbol_is_not_treated_as_resolving() {
        let root_dir = tmp_root();
        write_file(&root_dir, "src/config.rs", "pub struct NewConfig;\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let mut nodes = vec![md_node(
            "main",
            ".oh/notes.md",
            "Notes",
            "In `src/config.rs`, the `OldConfig` type controls parsing.",
            1,
        )];
        nodes.push(code_node(
            "main",
            "src/config.rs",
            "NewConfig",
            NodeKind::Struct,
        ));
        let report = run_doc_drift(&nodes, &root_paths);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].class, DriftClass::Symbol);
    }

    /// Symbol reference bound to a file that exists on disk but was never
    /// extracted into the graph (excluded/unindexed) -- must be unresolvable,
    /// never proven_dead.
    #[test]
    fn symbol_in_unindexed_file_is_unresolvable() {
        let root_dir = tmp_root();
        write_file(&root_dir, "vendor/blob.rs", "whatever\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        // No code_node for vendor/blob.rs -- simulates an excluded/unindexed file.
        let nodes = vec![md_node(
            "main",
            ".oh/notes.md",
            "Notes",
            "In `vendor/blob.rs`, the `Whatever` type controls parsing.",
            1,
        )];
        let report = run_doc_drift(&nodes, &root_paths);
        assert!(report.findings.is_empty());
        assert_eq!(report.unresolvable, 1);
    }

    /// A backticked prose word shaped like a symbol, with no nearby file
    /// context, must be unresolvable -- never flagged as drift.
    #[test]
    fn prose_word_shaped_like_symbol_is_unresolvable() {
        let root_dir = tmp_root();
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let nodes = vec![md_node(
            "main",
            ".oh/notes.md",
            "Notes",
            "Remember that `SomeConcept` is not a real symbol anywhere.",
            1,
        )];
        let report = run_doc_drift(&nodes, &root_paths);
        assert!(report.findings.is_empty());
        assert_eq!(report.unresolvable, 1);
    }

    /// Live-run regression: `.oh/config.toml` was matched as `oh/config.toml`
    /// (the `\b` anchor cannot sit between a space and a `.`) and then declared
    /// dead. Dot-directory paths must keep their leading dot.
    #[test]
    fn dot_directory_path_keeps_leading_dot() {
        let root_dir = tmp_root();
        write_file(&root_dir, ".oh/config.toml", "[scanner]\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let nodes = vec![md_node(
            "main",
            "AGENTS.md",
            "Config",
            "Excludes live in `.oh/config.toml` and the missing .oh/nope.toml file.",
            1,
        )];
        let report = run_doc_drift(&nodes, &root_paths);
        assert_eq!(report.candidates_bare_path, 2);
        assert_eq!(report.findings.len(), 1, "{:?}", report.findings);
        assert_eq!(report.findings[0].reference, ".oh/nope.toml");
    }

    /// Live-run regression: co-change file anchors (`Other("file")`) exist for
    /// markdown and shell files too; they must not make such a file "indexed"
    /// for symbol binding.
    #[test]
    fn symbol_bound_to_non_code_file_is_unresolvable() {
        let root_dir = tmp_root();
        write_file(&root_dir, "docs/extractors.md", "# Extractors\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let mut nodes = vec![md_node(
            "main",
            "plugin/SKILL.md",
            "Setup",
            "See `docs/extractors.md` and the `EXTRACTOR_COVERAGE` table.",
            1,
        )];
        nodes.push(code_node(
            "main",
            "docs/extractors.md",
            "docs/extractors.md",
            NodeKind::Other("file".to_string()),
        ));
        let report = run_doc_drift(&nodes, &root_paths);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(report.unresolvable, 1);
    }

    /// A symbol absent from the section-bound file but present elsewhere in
    /// the graph is ambiguous binding, not proof of death.
    #[test]
    fn symbol_present_elsewhere_in_graph_is_unresolvable_not_dead() {
        let root_dir = tmp_root();
        write_file(&root_dir, "src/a.rs", "pub struct A;\n");
        write_file(&root_dir, "src/b.rs", "pub struct Config;\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let mut nodes = vec![md_node(
            "main",
            ".oh/notes.md",
            "Notes",
            "In `src/a.rs` we read `Config` from the environment.",
            1,
        )];
        nodes.push(code_node("main", "src/a.rs", "A", NodeKind::Struct));
        nodes.push(code_node("main", "src/b.rs", "Config", NodeKind::Struct));
        let report = run_doc_drift(&nodes, &root_paths);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(report.unresolvable, 1);
    }

    /// Live-run regression: TOML/JSON/shell files carry extractor nodes, but a
    /// Rust identifier can never be defined there; binding to them is not evidence.
    #[test]
    fn symbol_never_binds_to_config_or_script_files() {
        let root_dir = tmp_root();
        write_file(&root_dir, ".oh/config.toml", "[scanner]\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let mut nodes = vec![md_node(
            "main",
            ".oh/sessions/x.md",
            "Notes",
            "Excludes are read from `.oh/config.toml` into a `OnceLock`.",
            1,
        )];
        nodes.push(code_node(
            "main",
            ".oh/config.toml",
            "scanner",
            NodeKind::Const,
        ));
        let report = run_doc_drift(&nodes, &root_paths);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(report.unresolvable, 1);
    }

    /// `receiver.method()` refers to the method; the receiver is a local variable.
    #[test]
    fn method_call_span_uses_final_segment_as_leaf() {
        let root_dir = tmp_root();
        write_file(
            &root_dir,
            "src/cache.rs",
            "impl Event { fn canonical_bytes(&self) {} }\n",
        );
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let mut nodes = vec![md_node(
            "main",
            ".oh/sessions/x.md",
            "Notes",
            "In `src/cache.rs`, `event.canonical_bytes()` feeds the hash and `event.gone()` is removed.",
            1,
        )];
        nodes.push(code_node(
            "main",
            "src/cache.rs",
            "canonical_bytes",
            NodeKind::Function,
        ));
        let report = run_doc_drift(&nodes, &root_paths);
        assert_eq!(report.findings.len(), 1, "{:?}", report.findings);
        assert!(
            report.findings[0].message.contains("`gone`"),
            "{}",
            report.findings[0].message
        );
    }

    #[test]
    fn bare_filename_span_is_not_a_symbol_candidate() {
        assert!(!looks_like_symbol("import_calls.rs"));
        assert!(!looks_like_symbol("config.toml"));
        assert!(!looks_like_symbol("SKILL.md"));
        assert!(looks_like_symbol("Node.metadata"));
    }

    #[test]
    fn leading_dot_method_chain_is_not_a_symbol_candidate() {
        assert!(!looks_like_symbol(".iter().find()"));
        assert!(!looks_like_symbol(".unwrap()"));
        assert!(looks_like_symbol("Config::new()"));
    }

    #[test]
    fn placeholder_path_with_ellipsis_is_unresolvable() {
        let root_dir = tmp_root();
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let nodes = vec![md_node(
            "main",
            "plugin/SKILL.md",
            "ADRs",
            "Files are named like docs/ADRs/001-...md and so on.",
            1,
        )];
        let report = run_doc_drift(&nodes, &root_paths);
        assert!(report.findings.is_empty(), "{:?}", report.findings);
        assert_eq!(report.unresolvable, 1);
    }

    #[test]
    fn bare_lowercase_backtick_word_is_never_a_symbol_candidate() {
        // Regression guard for the dominant false-positive class: single
        // lowercase words used for emphasis, not identifiers.
        let root_dir = tmp_root();
        write_file(&root_dir, "src/config.rs", "pub struct Config;\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let mut nodes = vec![md_node(
            "main",
            ".oh/notes.md",
            "Notes",
            "See `src/config.rs` -- note that `true` is returned by default.",
            1,
        )];
        nodes.push(code_node(
            "main",
            "src/config.rs",
            "Config",
            NodeKind::Struct,
        ));
        let report = run_doc_drift(&nodes, &root_paths);
        // Only the `src/config.rs` bare-path candidate should be counted; `true`
        // must never become a symbol candidate at all.
        assert_eq!(report.candidates_symbol, 0);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn ambiguous_binding_two_files_in_one_section_is_unresolvable() {
        let root_dir = tmp_root();
        write_file(&root_dir, "src/a.rs", "pub struct A;\n");
        write_file(&root_dir, "src/b.rs", "pub struct B;\n");
        let root_paths: HashMap<String, PathBuf> = [("main".to_string(), root_dir.clone())]
            .into_iter()
            .collect();
        let mut nodes = vec![md_node(
            "main",
            ".oh/notes.md",
            "Notes",
            "Both `src/a.rs` and `src/b.rs` define `Missing`.",
            1,
        )];
        nodes.push(code_node("main", "src/a.rs", "A", NodeKind::Struct));
        nodes.push(code_node("main", "src/b.rs", "B", NodeKind::Struct));
        let report = run_doc_drift(&nodes, &root_paths);
        assert!(report.findings.is_empty());
        assert_eq!(report.unresolvable, 1);
    }
}
