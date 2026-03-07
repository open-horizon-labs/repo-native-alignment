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
        format!(
            "- `{}:{}` **{}** `{}`",
            self.file_path.display(),
            self.line_start,
            self.kind,
            self.signature
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
}

fn yaml_value_to_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        _ => format!("{:?}", v),
    }
}
