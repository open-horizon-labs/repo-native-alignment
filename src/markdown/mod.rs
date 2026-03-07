use std::fs;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use anyhow::Result;
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use crate::types::MarkdownChunk;

/// Recursively find all `.md` files under `repo_root`, respecting .gitignore rules,
/// and parse each into heading-delimited chunks.
pub fn extract_markdown_chunks(repo_root: &Path) -> Result<Vec<MarkdownChunk>> {
    let mut chunks = Vec::new();
    let md_files = crate::walk::walk_repo_files(repo_root, &["md"])?;
    for path in md_files {
        match parse_markdown_file(&path) {
            Ok(file_chunks) => chunks.extend(file_chunks),
            Err(e) => {
                tracing::warn!("Failed to parse {}: {}", path.display(), e);
            }
        }
    }
    Ok(chunks)
}

/// Parse a single markdown file into heading-delimited chunks.
///
/// Each chunk captures:
/// - The heading hierarchy (e.g., `["# Top", "## Sub"]`)
/// - All content from that heading until the next heading of equal or higher level
/// - All inline code spans found in the chunk
/// - Byte offset and length in the original file
pub fn parse_markdown_file(path: &Path) -> Result<Vec<MarkdownChunk>> {
    let source = fs::read_to_string(path)?;
    let chunks = parse_markdown_source(&source, path);
    Ok(chunks)
}

/// Core parsing logic, separated for testability.
fn parse_markdown_source(source: &str, path: &Path) -> Vec<MarkdownChunk> {
    let mut chunks = Vec::new();

    // State tracking
    let mut heading_stack: Vec<(u32, String)> = Vec::new(); // (level, heading text with prefix)
    let mut current_heading_level: u32 = 0;
    let mut current_code_spans: Vec<String> = Vec::new();
    let mut chunk_byte_start: usize = 0;
    let mut in_heading = false;
    let mut current_heading_text = String::new();
    let mut current_heading_lvl: u32 = 0;
    let mut saw_any_heading = false;

    let parser = Parser::new(source).into_offset_iter();

    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let lvl = heading_level_to_u32(level);

                // Flush the previous chunk
                let byte_end = range.start;
                if saw_any_heading || has_content(source, chunk_byte_start, byte_end) {
                    let hierarchy = build_hierarchy(&heading_stack);
                    chunks.push(MarkdownChunk {
                        file_path: path.to_path_buf(),
                        heading_hierarchy: hierarchy,
                        heading_level: current_heading_level,
                        content: source[chunk_byte_start..byte_end].to_string(),
                        byte_offset: chunk_byte_start,
                        byte_len: byte_end - chunk_byte_start,
                        code_spans: std::mem::take(&mut current_code_spans),
                    });
                }

                // Reset for new chunk
                chunk_byte_start = range.start;

                // Update heading stack: pop any headings at same or deeper level
                while let Some(&(stack_lvl, _)) = heading_stack.last() {
                    if stack_lvl >= lvl {
                        heading_stack.pop();
                    } else {
                        break;
                    }
                }

                in_heading = true;
                current_heading_text = String::new();
                current_heading_lvl = lvl;
            }

            Event::End(TagEnd::Heading(_level)) => {
                let prefix = "#".repeat(current_heading_lvl as usize);
                let heading_str = format!("{} {}", prefix, current_heading_text.trim());

                heading_stack.push((current_heading_lvl, heading_str));
                current_heading_level = current_heading_lvl;
                saw_any_heading = true;

                in_heading = false;
                current_heading_text = String::new();
            }

            Event::Text(text) => {
                if in_heading {
                    current_heading_text.push_str(&text);
                }
            }

            Event::Code(text) => {
                if in_heading {
                    current_heading_text.push_str(&text);
                }
                current_code_spans.push(text.to_string());
            }

            _ => {}
        }
    }

    // Flush the last chunk
    let byte_end = source.len();
    if saw_any_heading || has_content(source, chunk_byte_start, byte_end) {
        let hierarchy = build_hierarchy(&heading_stack);
        chunks.push(MarkdownChunk {
            file_path: path.to_path_buf(),
            heading_hierarchy: hierarchy,
            heading_level: current_heading_level,
            content: source[chunk_byte_start..byte_end].to_string(),
            byte_offset: chunk_byte_start,
            byte_len: byte_end - chunk_byte_start,
            code_spans: std::mem::take(&mut current_code_spans),
        });
    }

    chunks
}

/// Case-insensitive substring search across chunk content and heading hierarchy.
/// Returns matching chunks.
pub fn search_chunks<'a>(chunks: &'a [MarkdownChunk], query: &str) -> Vec<&'a MarkdownChunk> {
    let query_lower = query.to_lowercase();
    chunks
        .iter()
        .filter(|chunk| {
            if chunk.content.to_lowercase().contains(&query_lower) {
                return true;
            }
            chunk
                .heading_hierarchy
                .iter()
                .any(|h| h.to_lowercase().contains(&query_lower))
        })
        .collect()
}

fn heading_level_to_u32(level: HeadingLevel) -> u32 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Build the heading hierarchy from the current stack.
fn build_hierarchy(heading_stack: &[(u32, String)]) -> Vec<String> {
    heading_stack.iter().map(|(_, text)| text.clone()).collect()
}

/// Check if the source slice between start..end has non-whitespace content.
fn has_content(source: &str, start: usize, end: usize) -> bool {
    if start >= end || start >= source.len() {
        return false;
    }
    let end = end.min(source.len());
    source[start..end].chars().any(|c| !c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_headings() {
        let dir = tempfile::tempdir().unwrap();
        let md_path = dir.path().join("test.md");
        fs::write(
            &md_path,
            "# Title\n\nSome intro text.\n\n## Section A\n\nContent A with `foo_bar`.\n\n## Section B\n\nContent B.\n\n### Subsection B1\n\nDeep content with `baz`.\n",
        )
        .unwrap();

        let chunks = parse_markdown_file(&md_path).unwrap();

        assert_eq!(chunks.len(), 4, "Expected 4 chunks, got {}: {:#?}", chunks.len(), chunks);

        // Title
        assert_eq!(chunks[0].heading_hierarchy, vec!["# Title"]);
        assert_eq!(chunks[0].heading_level, 1);
        assert!(chunks[0].content.contains("Some intro text."));

        // Section A
        assert_eq!(chunks[1].heading_hierarchy, vec!["# Title", "## Section A"]);
        assert_eq!(chunks[1].heading_level, 2);
        assert!(chunks[1].content.contains("Content A"));
        assert!(chunks[1].code_spans.contains(&"foo_bar".to_string()));

        // Section B
        assert_eq!(chunks[2].heading_hierarchy, vec!["# Title", "## Section B"]);
        assert_eq!(chunks[2].heading_level, 2);

        // Subsection B1
        assert_eq!(
            chunks[3].heading_hierarchy,
            vec!["# Title", "## Section B", "### Subsection B1"]
        );
        assert_eq!(chunks[3].heading_level, 3);
        assert!(chunks[3].code_spans.contains(&"baz".to_string()));
    }

    #[test]
    fn test_preamble_before_heading() {
        let dir = tempfile::tempdir().unwrap();
        let md_path = dir.path().join("test.md");
        fs::write(&md_path, "Some preamble.\n\n# First Heading\n\nBody.\n").unwrap();

        let chunks = parse_markdown_file(&md_path).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading_level, 0);
        assert!(chunks[0].content.contains("Some preamble."));
        assert_eq!(chunks[1].heading_hierarchy, vec!["# First Heading"]);
    }

    #[test]
    fn test_search_chunks() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# Alpha".to_string()],
                heading_level: 1,
                content: "Hello world".to_string(),
                byte_offset: 0,
                byte_len: 11,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("b.md"),
                heading_hierarchy: vec!["# Beta".to_string()],
                heading_level: 1,
                content: "Goodbye world".to_string(),
                byte_offset: 0,
                byte_len: 13,
                code_spans: vec![],
            },
        ];

        let results = search_chunks(&chunks, "hello");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].heading_hierarchy, vec!["# Alpha"]);

        let results = search_chunks(&chunks, "world");
        assert_eq!(results.len(), 2);

        let results = search_chunks(&chunks, "Beta");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Goodbye world");
    }

    #[test]
    fn test_extract_skips_git_and_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD.md"), "# git").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/debug.md"), "# target").unwrap();

        fs::write(root.join("README.md"), "# Readme\n\nHello.\n").unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/guide.md"), "# Guide\n\nContent.\n").unwrap();

        let chunks = extract_markdown_chunks(root).unwrap();
        let files: Vec<_> = chunks.iter().map(|c| c.file_path.clone()).collect();

        assert!(files.iter().any(|p| p.ends_with("README.md")));
        assert!(files.iter().any(|p| p.ends_with("guide.md")));
        assert!(!files.iter().any(|p| p.to_string_lossy().contains(".git")));
        assert!(!files.iter().any(|p| p.to_string_lossy().contains("target")));
    }

    #[test]
    fn test_byte_offsets() {
        let source = "# A\n\nText.\n\n## B\n\nMore text.\n";
        let chunks = parse_markdown_source(source, Path::new("test.md"));

        assert_eq!(chunks.len(), 2);
        // First chunk starts at 0
        assert_eq!(chunks[0].byte_offset, 0);
        // Second chunk content should match the source slice
        let c1 = &chunks[1];
        assert_eq!(&source[c1.byte_offset..c1.byte_offset + c1.byte_len], c1.content);
    }

    #[test]
    fn test_no_headings() {
        let source = "Just some plain text.\n\nWith paragraphs.\n";
        let chunks = parse_markdown_source(source, Path::new("test.md"));

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading_level, 0);
        assert!(chunks[0].heading_hierarchy.is_empty());
        assert!(chunks[0].content.contains("Just some plain text."));
    }

    #[test]
    fn test_code_spans_in_heading() {
        let source = "# The `Config` struct\n\nDetails about config.\n";
        let chunks = parse_markdown_source(source, Path::new("test.md"));

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading_hierarchy, vec!["# The Config struct"]);
        assert!(chunks[0].code_spans.contains(&"Config".to_string()));
    }
}
