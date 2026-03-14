use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::graph::Node;

/// Parsed .oh/ artifact (outcome, signal, guardrail, or metis)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OhArtifact {
    pub kind: OhArtifactKind,
    pub file_path: PathBuf,
    pub frontmatter: BTreeMap<String, serde_yaml::Value>,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OhArtifactKind {
    Outcome,
    Signal,
    Guardrail,
    Metis,
}

impl std::fmt::Display for OhArtifactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OhArtifactKind::Outcome => write!(f, "outcome"),
            OhArtifactKind::Signal => write!(f, "signal"),
            OhArtifactKind::Guardrail => write!(f, "guardrail"),
            OhArtifactKind::Metis => write!(f, "metis"),
        }
    }
}

impl OhArtifact {
    pub fn id(&self) -> String {
        self.frontmatter
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string()
    }

    pub fn to_markdown(&self) -> String {
        let mut out = format!("### {} ({})\n", self.id(), self.kind);
        for (k, v) in &self.frontmatter {
            if k != "id" {
                out.push_str(&format!("- **{}:** {}\n", k, yaml_value_to_string(v)));
            }
        }
        out.push('\n');
        out.push_str(&self.body);
        out
    }
}

/// A heading-delimited section from any markdown file.
///
/// Each chunk represents a coherent semantic block: a heading plus all body
/// text until the next heading of equal or higher level. Chunks are never
/// split mid-paragraph, mid-list, or mid-code-block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownChunk {
    pub file_path: PathBuf,
    pub heading_hierarchy: Vec<String>,
    pub heading_level: u32,
    /// The heading text without the `#` prefix (e.g., "Section A").
    /// Empty for preamble/frontmatter chunks.
    pub heading_text: String,
    /// Parent heading text (the nearest ancestor heading), if any.
    pub parent_heading: Option<String>,
    /// Whether this chunk is YAML frontmatter.
    pub is_frontmatter: bool,
    pub content: String,
    pub byte_offset: usize,
    pub byte_len: usize,
    /// Code spans found in this chunk (potential cross-references)
    pub code_spans: Vec<String>,
}

impl MarkdownChunk {
    /// Breadcrumb path through the heading hierarchy (e.g., "Aim > Mechanism > Hypothesis").
    /// Derived from `heading_hierarchy` — no separate storage needed.
    pub fn section_path(&self) -> String {
        self.heading_hierarchy
            .iter()
            .map(|h| h.trim_start_matches('#').trim())
            .collect::<Vec<_>>()
            .join(" > ")
    }

    pub fn to_markdown(&self) -> String {
        let location = format!(
            "`{}` > {}",
            self.file_path.display(),
            self.heading_hierarchy.join(" > ")
        );
        format!("{}\n\n{}", location, self.content)
    }

    /// Produce embedding text: just the body content, truncated to fit
    /// within a 512-token embedding model budget (MiniLM-L6-v2).
    ///
    /// No breadcrumb prefix — the section_path() adds no validated value
    /// for MiniLM-L6-v2 and wastes embedding budget. The body text alone
    /// is the semantic signal.
    pub fn embedding_text(&self) -> String {
        self.embedding_text_with_limit(500)
    }

    /// Internal: produce embedding text with a configurable char limit on body.
    fn embedding_text_with_limit(&self, max_body_chars: usize) -> String {
        if self.content.len() > max_body_chars {
            self.content[..self.content.floor_char_boundary(max_body_chars)].to_string()
        } else {
            self.content.clone()
        }
    }
}

/// A code symbol extracted by tree-sitter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeSymbol {
    pub file_path: PathBuf,
    pub name: String,
    pub kind: SymbolKind,
    pub line_start: usize,
    pub line_end: usize,
    pub signature: String,
    pub parent_scope: Option<String>,
    pub body: String,
    /// Scalar value if extractable (e.g. "5", "application/json"). None for complex/derived.
    #[serde(default)]
    pub value: Option<String>,
    /// true = inferred literal (synthetic), false = declared named constant.
    #[serde(default)]
    pub synthetic: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SymbolKind {
    Function,
    Struct,
    Trait,
    Impl,
    Enum,
    Const,
    Module,
    Import,
    Other(String),
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SymbolKind::Function => write!(f, "function"),
            SymbolKind::Struct => write!(f, "struct"),
            SymbolKind::Trait => write!(f, "trait"),
            SymbolKind::Impl => write!(f, "impl"),
            SymbolKind::Enum => write!(f, "enum"),
            SymbolKind::Const => write!(f, "const"),
            SymbolKind::Module => write!(f, "module"),
            SymbolKind::Import => write!(f, "import"),
            SymbolKind::Other(s) => write!(f, "{}", s),
        }
    }
}

impl CodeSymbol {
    pub fn to_markdown(&self) -> String {
        let value_part = match (&self.kind, &self.value) {
            (SymbolKind::Const, Some(v)) => format!(" = {}", v),
            _ => String::new(),
        };
        let synthetic_badge = if self.synthetic { " *(literal)*" } else { "" };
        format!(
            "- `{}:{}` **{}** `{}`{}{}",
            self.file_path.display(),
            self.line_start,
            self.kind,
            self.signature,
            value_part,
            synthetic_badge,
        )
    }
}

/// Git commit info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommitInfo {
    pub hash: String,
    pub short_hash: String,
    pub message: String,
    pub author: String,
    pub timestamp: i64,
    pub changed_files: Vec<PathBuf>,
}

impl GitCommitInfo {
    pub fn to_markdown(&self) -> String {
        let files = self
            .changed_files
            .iter()
            .map(|f| format!("  - `{}`", f.display()))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "- **{}** {} — _{}_\n{}",
            self.short_hash, self.message, self.author, files
        )
    }
}

/// Combined query result spanning all layers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub query: String,
    pub outcomes: Vec<OhArtifact>,
    pub markdown_chunks: Vec<MarkdownChunk>,
    pub code_symbols: Vec<Node>,
    pub commits: Vec<GitCommitInfo>,
}

impl QueryResult {
    /// Navigable summary rendering targeting <10K chars.
    /// Returns stable node IDs and suggested tool calls so agents can drill deeper.
    ///
    /// - Outcome: status line + first paragraph only (max 300 chars)
    /// - Commits: only tagged commits (containing [outcome:]), capped at 15, with drill-down hint
    /// - Symbols: counts by kind + top 5 files with up to 3 key symbols each, including stable IDs
    /// - Markdown: heading names + file paths
    /// - PR Merges: count (appended by caller)
    pub fn to_summary_markdown(&self) -> String {
        let mut out = format!("# Query: {}\n\n", self.query);

        // Outcomes: status line + truncated first paragraph
        if !self.outcomes.is_empty() {
            out.push_str("## Outcomes\n\n");
            for a in &self.outcomes {
                out.push_str(&format!("### {} ({})\n", a.id(), a.kind));
                if let Some(status) = a.frontmatter.get("status") {
                    out.push_str(&format!("- **status:** {}\n", yaml_value_to_string(status)));
                }
                if let Some(aim) = a.frontmatter.get("aim") {
                    out.push_str(&format!("- **aim:** {}\n", yaml_value_to_string(aim)));
                }
                // First paragraph of body only, max 300 chars
                let first_para = a.body.split("\n\n").next().unwrap_or("");
                let truncated: String = first_para.chars().take(300).collect();
                if !truncated.is_empty() {
                    out.push('\n');
                    out.push_str(&truncated);
                    if truncated.len() < first_para.len() {
                        out.push_str("...");
                    }
                    out.push('\n');
                }
                out.push('\n');
            }
        }

        // Commits: only tagged commits (containing [outcome:]), capped at 15
        if !self.commits.is_empty() {
            let tagged: Vec<&GitCommitInfo> = self
                .commits
                .iter()
                .filter(|c| c.message.contains("[outcome:"))
                .collect();
            let total_tagged = tagged.len();
            let total_all = self.commits.len();

            out.push_str("## Commits\n\n");
            if total_tagged > 0 {
                for c in tagged.iter().take(15) {
                    let first_line = c.message.lines().next().unwrap_or(&c.message);
                    out.push_str(&format!("- **{}** {}\n", c.short_hash, first_line));
                }
                if total_tagged > 15 {
                    out.push_str(&format!(
                        "\n...and {} more tagged commits\n",
                        total_tagged - 15
                    ));
                }
                out.push_str(&format!(
                    "\n{} tagged / {} total commits\n",
                    total_tagged, total_all
                ));
            } else {
                out.push_str(&format!(
                    "{} commits (none tagged with [outcome:])\n",
                    total_all
                ));
            }
            out.push_str("\nUse `git show <hash>` for full diff\n\n");
        }

        // Symbols: counts by kind + top 5 files with navigable symbol IDs
        if !self.code_symbols.is_empty() {
            use std::collections::BTreeMap;
            let mut kind_counts: BTreeMap<String, usize> = BTreeMap::new();
            // Group symbols by file, preserving order within each file
            let mut file_symbols: BTreeMap<String, Vec<&Node>> = BTreeMap::new();
            for node in &self.code_symbols {
                *kind_counts.entry(node.id.kind.to_string()).or_insert(0) += 1;
                file_symbols
                    .entry(node.id.file.display().to_string())
                    .or_default()
                    .push(node);
            }

            out.push_str("## Code Symbols\n\n");

            // Summary counts line
            let parts: Vec<String> = kind_counts
                .iter()
                .map(|(k, count)| format!("{} {}s", count, k))
                .collect();
            out.push_str(&format!(
                "{} across {} files\n\n",
                parts.join(", "),
                file_symbols.len()
            ));

            // Top 5 files by symbol count, with up to 3 key symbols each
            let mut files_sorted: Vec<(&String, &Vec<&Node>)> =
                file_symbols.iter().collect();
            files_sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()));

            for (file, symbols) in files_sorted.iter().take(5) {
                out.push_str(&format!("### {} ({} symbols)\n", file, symbols.len()));
                for node in symbols.iter().take(3) {
                    let stable_id = node.stable_id();
                    out.push_str(&format!(
                        "- `{}` ({}) -- ID: `{}`\n",
                        node.id.name, node.id.kind, stable_id
                    ));
                }
                if symbols.len() > 3 {
                    out.push_str(&format!(
                        "- ...and {} more symbols\n",
                        symbols.len() - 3
                    ));
                }
                out.push('\n');
            }
            if files_sorted.len() > 5 {
                out.push_str(&format!(
                    "...and {} more files\n\n",
                    files_sorted.len() - 5
                ));
            }

            out.push_str(
                "Use `search` to explore further, `search(node: \"<id>\", mode: \"neighbors\")` with any ID above for impact/neighbors\n\n",
            );
        }

        // Markdown: heading names + file paths
        if !self.markdown_chunks.is_empty() {
            out.push_str("## Markdown Sections\n\n");
            out.push_str(&format!(
                "{} matching sections:\n",
                self.markdown_chunks.len()
            ));
            for m in &self.markdown_chunks {
                let heading = m
                    .heading_hierarchy
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "(untitled)".to_string());
                out.push_str(&format!("- {} (`{}`)\n", heading, m.file_path.display()));
            }
            out.push('\n');
        }

        if self.outcomes.is_empty()
            && self.commits.is_empty()
            && self.code_symbols.is_empty()
            && self.markdown_chunks.is_empty()
        {
            out.push_str("_No results found._\n");
        }

        out
    }
}

fn yaml_value_to_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Sequence(seq) => {
            let items: Vec<String> = seq.iter().map(yaml_value_to_string).collect();
            items.join(", ")
        }
        _ => format!("{:?}", v),
    }
}
