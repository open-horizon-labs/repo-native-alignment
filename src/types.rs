use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

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

/// A heading-delimited section from any markdown file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownChunk {
    pub file_path: PathBuf,
    pub heading_hierarchy: Vec<String>,
    pub heading_level: u32,
    pub content: String,
    pub byte_offset: usize,
    pub byte_len: usize,
    /// Code spans found in this chunk (potential cross-references)
    pub code_spans: Vec<String>,
}

impl MarkdownChunk {
    pub fn to_markdown(&self) -> String {
        let location = format!(
            "`{}` > {}",
            self.file_path.display(),
            self.heading_hierarchy.join(" > ")
        );
        format!("{}\n\n{}", location, self.content)
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
    pub code_symbols: Vec<CodeSymbol>,
    pub commits: Vec<GitCommitInfo>,
}

impl QueryResult {
    pub fn to_markdown(&self) -> String {
        let mut out = format!("# Query: {}\n\n", self.query);

        if !self.outcomes.is_empty() {
            out.push_str("## Matching Outcomes / Signals / Guardrails / Metis\n\n");
            for a in &self.outcomes {
                out.push_str(&a.to_markdown());
                out.push_str("\n---\n\n");
            }
        }

        if !self.commits.is_empty() {
            out.push_str("## Relevant Commits\n\n");
            for c in &self.commits {
                out.push_str(&c.to_markdown());
                out.push('\n');
            }
            out.push('\n');
        }

        if !self.code_symbols.is_empty() {
            out.push_str("## Matching Code Symbols\n\n");
            for s in &self.code_symbols {
                out.push_str(&s.to_markdown());
                out.push('\n');
            }
            out.push('\n');
        }

        if !self.markdown_chunks.is_empty() {
            out.push_str("## Matching Markdown Sections\n\n");
            for m in &self.markdown_chunks {
                out.push_str(&m.to_markdown());
                out.push_str("\n\n---\n\n");
            }
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

    /// Compact summary rendering targeting <5K chars.
    /// - Outcome: status line + first paragraph only (max 300 chars)
    /// - Commits: only tagged commits (containing [outcome:]), capped at 15
    /// - Symbols: counts by kind only
    /// - Markdown: count + heading names only
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

        // Commits: only tagged commits (containing [outcome:])
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
                    "\n{} tagged / {} total commits\n\n",
                    total_tagged,
                    total_all
                ));
            } else {
                out.push_str(&format!(
                    "{} commits (none tagged with [outcome:])\n\n",
                    total_all
                ));
            }
        }

        // Symbols: counts by kind only; fall back to file count from commits
        if !self.code_symbols.is_empty() {
            use std::collections::BTreeMap;
            let mut kind_counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for s in &self.code_symbols {
                *kind_counts.entry(s.kind.to_string()).or_insert(0) += 1;
                files.insert(s.file_path.display().to_string());
            }
            out.push_str("## Code Symbols\n\n");
            let parts: Vec<String> = kind_counts
                .iter()
                .map(|(k, count)| format!("{} {}s", count, k))
                .collect();
            out.push_str(&format!(
                "{} across {} files\n\n",
                parts.join(", "),
                files.len()
            ));
        } else if !self.commits.is_empty() {
            // No symbols extracted (summary mode) — show file count from commits
            let changed_files: std::collections::BTreeSet<String> = self
                .commits
                .iter()
                .flat_map(|c| c.changed_files.iter())
                .map(|f| f.display().to_string())
                .collect();
            if !changed_files.is_empty() {
                out.push_str(&format!(
                    "## Code\n\nChanges across {} files\n\n",
                    changed_files.len()
                ));
            }
        }

        // Markdown: count + heading names only
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
                out.push_str(&format!(
                    "- {} ({})\n",
                    heading,
                    m.file_path.display()
                ));
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
