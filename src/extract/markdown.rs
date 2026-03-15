//! Markdown extractor: heading-aware sections as graph nodes with YAML frontmatter.
//!
//! Reuses the existing `pulldown-cmark` parsing from `src/markdown/mod.rs`
//! but produces graph `Node` types for the unified graph model.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::graph::{ExtractionSource, Node, NodeId, NodeKind};

use super::{ExtractionResult, Extractor};

/// Extractor for Markdown files. Produces one node per heading section,
/// with heading hierarchy and YAML frontmatter as metadata.
pub struct MarkdownExtractor;

impl MarkdownExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Extractor for MarkdownExtractor {
    fn extensions(&self) -> &[&str] {
        &["md", "mdx"]
    }

    fn name(&self) -> &str {
        "markdown"
    }

    fn extract(&self, path: &Path, content: &str) -> Result<ExtractionResult> {
        let mut nodes = Vec::new();

        // Extract YAML frontmatter if present
        let frontmatter = extract_frontmatter(content);

        // Use existing pulldown-cmark parser for heading-aware chunking
        let chunks = parse_markdown_file_from_source(content, path);

        for (i, chunk) in chunks.iter().enumerate() {
            let section_name = if chunk.is_frontmatter {
                "frontmatter".to_string()
            } else if chunk.heading_hierarchy.is_empty() {
                "preamble".to_string()
            } else {
                chunk
                    .heading_hierarchy
                    .last()
                    .map(|h| h.trim_start_matches('#').trim().to_string())
                    .unwrap_or_else(|| format!("section_{}", i))
            };

            let mut metadata = BTreeMap::new();

            // Heading hierarchy as metadata
            if !chunk.heading_hierarchy.is_empty() {
                metadata.insert(
                    "heading_hierarchy".to_string(),
                    chunk.heading_hierarchy.join(" > "),
                );
            }
            metadata.insert("heading_level".to_string(), chunk.heading_level.to_string());

            // Heading text (without # prefix)
            if !chunk.heading_text.is_empty() {
                metadata.insert("heading_text".to_string(), chunk.heading_text.clone());
            }

            // Parent heading for hierarchy context
            if let Some(ref parent) = chunk.parent_heading {
                metadata.insert("parent_heading".to_string(), parent.clone());
            }

            // Section path breadcrumbs (e.g., "Aim > Mechanism > Hypothesis")
            let sp = chunk.section_path();
            if !sp.is_empty() {
                metadata.insert("section_path".to_string(), sp.clone());
            }

            // Frontmatter flag
            if chunk.is_frontmatter {
                metadata.insert("is_frontmatter".to_string(), "true".to_string());
            }

            // Detect .oh/ artifact kind from file path
            if let Some(oh_kind) = detect_oh_kind(path) {
                metadata.insert("oh_kind".to_string(), oh_kind);
            }

            // Code spans as metadata (potential cross-references)
            if !chunk.code_spans.is_empty() {
                metadata.insert("code_spans".to_string(), chunk.code_spans.join(", "));
            }

            // Attach frontmatter key-value pairs to the frontmatter chunk itself,
            // or to the first non-frontmatter chunk if there's no frontmatter chunk.
            let attach_frontmatter = if chunk.is_frontmatter {
                true
            } else if i == 0 || (i == 1 && chunks.first().map_or(false, |c| c.is_frontmatter)) {
                // First non-frontmatter chunk: attach frontmatter for backward compat
                !frontmatter.is_empty()
            } else {
                false
            };
            if attach_frontmatter {
                for (key, value) in &frontmatter {
                    metadata.insert(format!("frontmatter.{}", key), value.clone());
                }
            }

            // Compute line numbers from byte offsets
            let line_start = content[..chunk.byte_offset]
                .chars()
                .filter(|&c| c == '\n')
                .count()
                + 1;
            let line_end = content[..chunk.byte_offset + chunk.byte_len]
                .chars()
                .filter(|&c| c == '\n')
                .count()
                + 1;

            let node = Node {
                id: NodeId {
                    root: String::new(), // populated during multi-root integration
                    file: path.to_path_buf(),
                    name: section_name,
                    kind: NodeKind::MarkdownSection,
                },
                language: "markdown".to_string(),
                line_start,
                line_end,
                signature: if chunk.is_frontmatter {
                    "[frontmatter]".to_string()
                } else if !sp.is_empty() {
                    sp
                } else {
                    chunk.heading_hierarchy.join(" > ")
                },
                body: chunk.content.clone(),
                metadata,
                source: ExtractionSource::Markdown,
            };
            nodes.push(node);
        }

        Ok(ExtractionResult {
            nodes,
            edges: Vec::new(),
        })
    }
}

/// Detect if a markdown file lives under `.oh/` and return the artifact kind.
///
/// Maps directory names to artifact kinds: outcomes -> outcome, signals -> signal,
/// guardrails -> guardrail, metis -> metis. Returns None for non-.oh/ files.
fn detect_oh_kind(path: &Path) -> Option<String> {
    let components: Vec<_> = path.components().collect();
    for (i, comp) in components.iter().enumerate() {
        if comp.as_os_str() == ".oh" {
            if let Some(next) = components.get(i + 1) {
                let dir = next.as_os_str().to_string_lossy();
                return match dir.as_ref() {
                    "outcomes" => Some("outcome".to_string()),
                    "signals" => Some("signal".to_string()),
                    "guardrails" => Some("guardrail".to_string()),
                    "metis" => Some("metis".to_string()),
                    _ => None,
                };
            }
        }
    }
    None
}

/// Extract YAML frontmatter from markdown content.
/// Expects `---\nkey: value\n---\n` at the start of the file.
fn extract_frontmatter(content: &str) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();

    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return result;
    }

    // Find the closing ---
    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);
    if let Some(end_idx) = after_first.find("\n---") {
        let yaml_block = &after_first[..end_idx];
        for line in yaml_block.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_string();
                let value = value.trim().trim_matches('\'').trim_matches('"').to_string();
                if !key.is_empty() {
                    result.insert(key, value);
                }
            }
        }
    }

    result
}

/// Parse markdown source directly (avoids re-reading the file from disk
/// since the extractor framework already provides the content).
fn parse_markdown_file_from_source(
    source: &str,
    path: &Path,
) -> Vec<crate::types::MarkdownChunk> {
    crate::markdown::parse_markdown_source(source, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_markdown_extractor_basic() {
        let extractor = MarkdownExtractor::new();
        let content = "# Title\n\nIntro text.\n\n## Section A\n\nContent A.\n\n## Section B\n\nContent B.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        assert_eq!(result.nodes.len(), 3);

        // First node: Title section
        assert_eq!(result.nodes[0].id.name, "Title");
        assert_eq!(
            result.nodes[0].metadata.get("heading_level"),
            Some(&"1".to_string())
        );

        // Second node: Section A
        assert_eq!(result.nodes[1].id.name, "Section A");
        assert_eq!(
            result.nodes[1].metadata.get("heading_hierarchy"),
            Some(&"# Title > ## Section A".to_string())
        );

        // Third node: Section B
        assert_eq!(result.nodes[2].id.name, "Section B");
    }

    #[test]
    fn test_markdown_extractor_with_frontmatter() {
        let extractor = MarkdownExtractor::new();
        let content = "---\nid: my-outcome\nstatus: active\ntitle: 'Test Outcome'\n---\n\n# My Outcome\n\nSome content.\n";
        let result = extractor.extract(Path::new("outcome.md"), content).unwrap();

        assert!(!result.nodes.is_empty());

        // Frontmatter should be on the first node
        let first = &result.nodes[0];
        assert_eq!(
            first.metadata.get("frontmatter.id"),
            Some(&"my-outcome".to_string())
        );
        assert_eq!(
            first.metadata.get("frontmatter.status"),
            Some(&"active".to_string())
        );
        assert_eq!(
            first.metadata.get("frontmatter.title"),
            Some(&"Test Outcome".to_string())
        );
    }

    #[test]
    fn test_markdown_extractor_preamble() {
        let extractor = MarkdownExtractor::new();
        let content = "Some preamble text.\n\n# First Heading\n\nBody.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        assert_eq!(result.nodes.len(), 2);
        assert_eq!(result.nodes[0].id.name, "preamble");
        assert_eq!(
            result.nodes[0].metadata.get("heading_level"),
            Some(&"0".to_string())
        );
    }

    #[test]
    fn test_markdown_extractor_code_spans() {
        let extractor = MarkdownExtractor::new();
        let content = "# The `Config` struct\n\nUse `Config::new()` to create.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        assert_eq!(result.nodes.len(), 1);
        let meta = &result.nodes[0].metadata;
        assert!(meta.get("code_spans").unwrap().contains("Config"));
        assert!(meta.get("code_spans").unwrap().contains("Config::new()"));
    }

    #[test]
    fn test_markdown_extractor_nested_headings() {
        let extractor = MarkdownExtractor::new();
        let content =
            "# Top\n\n## Sub\n\n### Deep\n\nDeep content.\n\n## Another Sub\n\nMore.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        assert_eq!(result.nodes.len(), 4);

        // Deep section should have full hierarchy
        assert_eq!(result.nodes[2].id.name, "Deep");
        assert_eq!(
            result.nodes[2].metadata.get("heading_hierarchy"),
            Some(&"# Top > ## Sub > ### Deep".to_string())
        );

        // "Another Sub" should reset to level 2
        assert_eq!(result.nodes[3].id.name, "Another Sub");
        assert_eq!(
            result.nodes[3].metadata.get("heading_hierarchy"),
            Some(&"# Top > ## Another Sub".to_string())
        );
    }

    #[test]
    fn test_markdown_extractor_extensions() {
        let extractor = MarkdownExtractor::new();
        assert!(extractor.extensions().contains(&"md"));
        assert!(extractor.extensions().contains(&"mdx"));
    }

    #[test]
    fn test_frontmatter_extraction() {
        let fm = extract_frontmatter(
            "---\nid: test\nstatus: active\n---\n\n# Hello\n",
        );
        assert_eq!(fm.get("id"), Some(&"test".to_string()));
        assert_eq!(fm.get("status"), Some(&"active".to_string()));
    }

    #[test]
    fn test_frontmatter_no_frontmatter() {
        let fm = extract_frontmatter("# Just a heading\n\nContent.\n");
        assert!(fm.is_empty());
    }

    #[test]
    fn test_frontmatter_quoted_values() {
        let fm = extract_frontmatter(
            "---\ntitle: 'My Title'\ndesc: \"A description\"\n---\n",
        );
        assert_eq!(fm.get("title"), Some(&"My Title".to_string()));
        assert_eq!(fm.get("desc"), Some(&"A description".to_string()));
    }

    #[test]
    fn test_line_numbers() {
        let extractor = MarkdownExtractor::new();
        let content = "# Title\n\nLine 3.\n\n## Section\n\nLine 7.\n";
        let result = extractor.extract(Path::new("doc.md"), content).unwrap();

        assert_eq!(result.nodes[0].line_start, 1);
        assert_eq!(result.nodes[1].line_start, 5); // ## Section starts on line 5
    }

    #[test]
    fn test_detect_oh_kind_outcome() {
        assert_eq!(detect_oh_kind(Path::new(".oh/outcomes/my-outcome.md")), Some("outcome".to_string()));
    }

    #[test]
    fn test_detect_oh_kind_signal() {
        assert_eq!(detect_oh_kind(Path::new(".oh/signals/my-signal.md")), Some("signal".to_string()));
    }

    #[test]
    fn test_detect_oh_kind_guardrail() {
        assert_eq!(detect_oh_kind(Path::new(".oh/guardrails/my-guardrail.md")), Some("guardrail".to_string()));
    }

    #[test]
    fn test_detect_oh_kind_metis() {
        assert_eq!(detect_oh_kind(Path::new(".oh/metis/my-learning.md")), Some("metis".to_string()));
    }

    #[test]
    fn test_detect_oh_kind_not_oh() {
        assert_eq!(detect_oh_kind(Path::new("src/main.rs")), None);
        assert_eq!(detect_oh_kind(Path::new("docs/README.md")), None);
    }

    #[test]
    fn test_detect_oh_kind_unknown_subdir() {
        assert_eq!(detect_oh_kind(Path::new(".oh/sessions/123.md")), None);
        assert_eq!(detect_oh_kind(Path::new(".oh/.cache/data.md")), None);
    }

    #[test]
    fn test_oh_artifact_gets_oh_kind_metadata() {
        let extractor = MarkdownExtractor::new();
        let content = "---\nid: test-outcome\nstatus: active\n---\n\n# My Outcome\n\nContent.\n";
        let result = extractor.extract(Path::new(".oh/outcomes/test-outcome.md"), content).unwrap();
        assert!(!result.nodes.is_empty());
        for node in &result.nodes {
            assert_eq!(node.metadata.get("oh_kind"), Some(&"outcome".to_string()),
                "node {} should have oh_kind=outcome", node.id.name);
        }
    }

    #[test]
    fn test_non_oh_file_no_oh_kind() {
        let extractor = MarkdownExtractor::new();
        let content = "# Regular Doc\n\nContent.\n";
        let result = extractor.extract(Path::new("docs/readme.md"), content).unwrap();
        for node in &result.nodes {
            assert!(node.metadata.get("oh_kind").is_none(),
                "non-.oh/ node should not have oh_kind metadata");
        }
    }

    #[test]
    fn test_oh_artifact_with_absolute_path() {
        assert_eq!(
            detect_oh_kind(Path::new("/home/user/repo/.oh/metis/learning.md")),
            Some("metis".to_string())
        );
    }
}
