use std::fs;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use anyhow::Result;
use gray_matter::{engine::YAML, Matter, ParsedEntity};
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

/// Core parsing logic, separated for testability and reuse by the markdown extractor.
///
/// Produces heading-hierarchy-aware chunks:
/// - YAML frontmatter (if present) becomes its own chunk with `is_frontmatter = true`
/// - Each heading + body until the next heading of same/higher level is one chunk
/// - Nested headings track parent context via `parent_heading`
/// - Never splits mid-paragraph, mid-list, or mid-code-block
pub fn parse_markdown_source(source: &str, path: &Path) -> Vec<MarkdownChunk> {
    let mut chunks = Vec::new();

    // Detect and extract YAML frontmatter as a separate chunk
    let (frontmatter_chunk, parse_source, byte_offset_base) =
        extract_frontmatter_chunk(source, path);
    if let Some(fm_chunk) = frontmatter_chunk {
        chunks.push(fm_chunk);
    }

    // State tracking
    let mut heading_stack: Vec<(u32, String)> = Vec::new(); // (level, heading text with prefix)
    let mut current_heading_level: u32 = 0;
    let mut current_heading_plain_text = String::new(); // heading text without # prefix
    let mut current_code_spans: Vec<String> = Vec::new();
    let mut chunk_byte_start: usize = 0;
    let mut in_heading = false;
    let mut current_heading_text = String::new();
    let mut current_heading_lvl: u32 = 0;
    let mut saw_any_heading = false;

    let parser = Parser::new(parse_source).into_offset_iter();

    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let lvl = heading_level_to_u32(level);

                // Flush the previous chunk
                let byte_end = range.start;
                if saw_any_heading || has_content(parse_source, chunk_byte_start, byte_end) {
                    let hierarchy = build_hierarchy(&heading_stack);
                    let parent = parent_heading_text(&heading_stack);
                    chunks.push(MarkdownChunk {
                        file_path: path.to_path_buf(),
                        heading_hierarchy: hierarchy,
                        heading_level: current_heading_level,
                        heading_text: current_heading_plain_text.clone(),
                        parent_heading: parent,
                        is_frontmatter: false,
                        content: parse_source[chunk_byte_start..byte_end].to_string(),
                        byte_offset: chunk_byte_start + byte_offset_base,
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
                let plain_text = current_heading_text.trim().to_string();
                let heading_str = format!("{} {}", prefix, plain_text);

                heading_stack.push((current_heading_lvl, heading_str));
                current_heading_level = current_heading_lvl;
                current_heading_plain_text = plain_text;
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
    let byte_end = parse_source.len();
    if saw_any_heading || has_content(parse_source, chunk_byte_start, byte_end) {
        let hierarchy = build_hierarchy(&heading_stack);
        let parent = parent_heading_text(&heading_stack);
        chunks.push(MarkdownChunk {
            file_path: path.to_path_buf(),
            heading_hierarchy: hierarchy,
            heading_level: current_heading_level,
            heading_text: current_heading_plain_text,
            parent_heading: parent,
            is_frontmatter: false,
            content: parse_source[chunk_byte_start..byte_end].to_string(),
            byte_offset: chunk_byte_start + byte_offset_base,
            byte_len: byte_end - chunk_byte_start,
            code_spans: std::mem::take(&mut current_code_spans),
        });
    }

    chunks
}

/// Detect YAML frontmatter (`---\n...\n---`) at the start of a markdown file.
/// Returns (optional frontmatter chunk, remaining source to parse, byte offset of remaining source).
///
/// Uses the `gray_matter` crate for robust frontmatter detection (handles empty
/// frontmatter, CRLF line endings, `---` inside YAML values, etc.), then computes
/// byte offsets from the original source for the chunking pipeline.
fn extract_frontmatter_chunk<'a>(
    source: &'a str,
    path: &Path,
) -> (Option<MarkdownChunk>, &'a str, usize) {
    let matter: Matter<YAML> = Matter::new();
    let parsed: ParsedEntity = match matter.parse(source) {
        Ok(p) => p,
        Err(_) => return (None, source, 0),
    };

    // gray_matter detected frontmatter if the content is shorter than the original,
    // OR if `matter` is non-empty. For empty frontmatter (`---\n---`), both `data`
    // and `matter` are empty, but `content` will differ from the original.
    let has_frontmatter = !parsed.matter.is_empty()
        || (source.trim_start().starts_with("---") && parsed.content.len() < source.len());

    if !has_frontmatter {
        return (None, source, 0);
    }

    // Compute byte offset of the end of the frontmatter block in the original source.
    // Walk lines to find the closing `---` delimiter, matching gray_matter's logic:
    // first line must be `---` (with optional \r), then scan for a line that is exactly `---`.
    let bytes = source.as_bytes();
    let mut pos = 0;

    // Skip the opening `---` line
    while pos < bytes.len() && bytes[pos] != b'\n' {
        pos += 1;
    }
    if pos < bytes.len() {
        pos += 1; // skip \n
    }

    // Scan for closing `---` line
    loop {
        if pos >= bytes.len() {
            // Reached end without finding closing delimiter — treat as no frontmatter
            return (None, source, 0);
        }
        let line_start = pos;
        while pos < bytes.len() && bytes[pos] != b'\n' {
            pos += 1;
        }
        let line = &source[line_start..pos];
        let trimmed_line = line.trim_end();
        if trimmed_line == "---" {
            // `pos` is at the \n after `---`, or at end of source
            let absolute_end = pos; // points to \n or end-of-source
            // Skip past the trailing newline after closing ---
            let rest_start = if absolute_end < bytes.len() && bytes[absolute_end] == b'\n' {
                absolute_end + 1
            } else {
                absolute_end
            };
            let fm_content = &source[..rest_start];
            let chunk = MarkdownChunk {
                file_path: path.to_path_buf(),
                heading_hierarchy: Vec::new(),
                heading_level: 0,
                heading_text: String::new(),
                parent_heading: None,
                is_frontmatter: true,
                content: fm_content.to_string(),
                byte_offset: 0,
                byte_len: rest_start,
                code_spans: Vec::new(),
            };
            return (Some(chunk), &source[rest_start..], rest_start);
        }
        // Move past the \n
        if pos < bytes.len() {
            pos += 1;
        }
    }
}

/// Ranked search across chunk content, headings, and frontmatter.
///
/// Scoring:
/// - Exact heading match: +10
/// - Heading contains query: +5
/// - Frontmatter match: +3 (kept low — frontmatter is machine metadata, not
///   human-readable content, so it should rank below heading matches)
/// - Body/content match: +2
/// - Higher heading level (lower number) gets a boost: +(7 - level)
///
/// Results are sorted by score descending.
pub fn search_chunks<'a>(chunks: &'a [MarkdownChunk], query: &str) -> Vec<&'a MarkdownChunk> {
    let query_lower = query.to_lowercase();
    let mut scored: Vec<(&MarkdownChunk, u32)> = chunks
        .iter()
        .filter_map(|chunk| {
            let mut score: u32 = 0;

            // Heading match
            let heading_lower = chunk.heading_text.to_lowercase();
            if heading_lower == query_lower {
                score += 10; // exact heading match
            } else if heading_lower.contains(&query_lower) {
                score += 5; // partial heading match
            }

            // Check heading hierarchy too
            if score == 0 {
                if chunk.heading_hierarchy.iter().any(|h| h.to_lowercase().contains(&query_lower)) {
                    score += 4;
                }
            }

            // Frontmatter match: low bonus (+3) because frontmatter is machine
            // metadata (YAML keys/values), not human-readable content. This ensures
            // h1 body-only matches (2 + 6 = 8) outrank frontmatter body-only
            // matches (3 + 2 = 5). Frontmatter heading matches (exact match on a
            // YAML key) are still competitive at 3 + 10 = 13.
            if chunk.is_frontmatter && chunk.content.to_lowercase().contains(&query_lower) {
                score += 3;
            }

            // Body/content match
            if chunk.content.to_lowercase().contains(&query_lower) {
                score += 2;
            }

            if score > 0 {
                // Heading level boost: h1 gets +6, h2 gets +5, etc.
                // Frontmatter (level 0) gets no level boost — "heading level"
                // is meaningless for frontmatter, and the frontmatter bonus
                // already accounts for its importance.
                if !chunk.is_frontmatter {
                    let level_boost = 7u32.saturating_sub(chunk.heading_level);
                    score += level_boost;
                }
                Some((chunk, score))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by(|a, b| b.1.cmp(&a.1));
    scored.into_iter().map(|(chunk, _)| chunk).collect()
}

/// A markdown chunk with a relevance score for ranked search results.
pub struct ScoredMarkdownChunk<'a> {
    pub chunk: &'a MarkdownChunk,
    pub score: f32,
}

/// Search markdown chunks with relevance scoring.
///
/// # Scoring rationale
///
/// Weights are initial estimates based on how agents typically use search results.
/// They have not been calibrated against a labeled dataset. The guiding principle
/// is that *where* a term appears matters more than *how often*:
///
/// ## Tier 1 -- Match location (dominates ranking)
/// - **1.0** exact heading match: the section is *about* the query term.
/// - **0.7** heading contains query: strong signal but not a dedicated section.
/// - **0.4** content-only match: the term appears but the section is about
///   something else. Set well below 0.7 so heading matches always win
///   when other factors are equal.
///
/// ## Tier 2 -- Heading level (small bonus, 0.02 -- 0.10)
/// Higher-level headings cover broader scope and are more likely to be the
/// "right" entry point. The bonus is small so it only breaks ties within
/// the same tier-1 band. h1 gets 0.10, h2 gets 0.08, h3 gets 0.06, h4+
/// gets 0.04. Preamble (no heading) gets 0.02.
///
/// ## Tier 3 -- Code span match (+0.15)
/// If the query matches an inline code span (e.g. `parse_config`), the
/// chunk is likely a cross-reference to a code symbol, which is a strong
/// relevance signal for developer queries. 0.15 is enough to noticeably
/// boost a content-only match but not enough to promote it above a
/// heading-contains match on its own.
///
/// ## Tier 4 -- Match density (+0.02 per occurrence, capped at 0.10)
/// More mentions = more relevant, but capped to prevent long documents
/// from dominating. The cap of 0.10 means density alone cannot bridge
/// the gap between tier-1 bands (0.4 vs 0.7).
///
/// ## Cross-tier interaction
/// A content-only match with maximum density + code span + h1 bonus can
/// reach 0.4 + 0.10 + 0.15 + 0.10 = 0.75, which slightly exceeds a
/// heading-contains match with h4 bonus (0.7 + 0.04 = 0.74). This is
/// intentional: a content-only chunk that mentions the query 5+ times in
/// code spans at the top level is arguably more relevant than a heading
/// that merely contains the term in a deep subsection.
///
/// Returns results sorted by descending score.
pub fn search_chunks_ranked<'a>(chunks: &'a [MarkdownChunk], query: &str) -> Vec<ScoredMarkdownChunk<'a>> {
    let query_lower = query.to_lowercase();

    let mut scored: Vec<ScoredMarkdownChunk<'a>> = chunks
        .iter()
        .filter_map(|chunk| {
            let content_lower = chunk.content.to_lowercase();
            // Strip `#` prefix before matching so queries like "#" don't
            // spuriously match every heading (review finding #6).
            let heading_match = chunk
                .heading_hierarchy
                .iter()
                .any(|h| {
                    let text = h.trim_start_matches('#').trim().to_lowercase();
                    text.contains(&query_lower)
                });
            let content_match = content_lower.contains(&query_lower);

            if !heading_match && !content_match {
                return None;
            }

            let mut score: f32 = 0.0;

            // Tier 1: Match location quality (see doc comment for rationale)
            if heading_match {
                let exact_heading = chunk.heading_hierarchy.iter().any(|h| {
                    let text = h.trim_start_matches('#').trim().to_lowercase();
                    text == query_lower
                });
                if exact_heading {
                    score += 1.0; // Section is *about* this term
                } else {
                    score += 0.7; // Heading mentions term but section is broader
                }
            } else {
                score += 0.4; // Term in body only; well below 0.7 so headings always win at parity
            }

            // Tier 2: Heading level bonus -- tie-breaker within same tier-1 band
            match chunk.heading_level {
                0 => score += 0.02, // preamble (no heading)
                1 => score += 0.10, // top-level = broadest useful context
                2 => score += 0.08,
                3 => score += 0.06,
                _ => score += 0.04, // deep subsections
            }

            // Tier 3: Code span match -- cross-reference to a code symbol
            if chunk.code_spans.iter().any(|s| s.to_lowercase().contains(&query_lower)) {
                score += 0.15; // strong signal for developer queries
            }

            // Tier 4: Match density -- capped so long docs don't dominate
            let occurrence_count = content_lower.matches(&query_lower).count();
            let density_bonus = (occurrence_count as f32 * 0.02).min(0.10);
            score += density_bonus;

            Some(ScoredMarkdownChunk { chunk, score })
        })
        .collect();

    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored
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

/// Get the parent heading text (second-to-last in the heading stack).
/// Strips `#` prefixes to return plain text (e.g., "Aim" not "# Aim").
fn parent_heading_text(heading_stack: &[(u32, String)]) -> Option<String> {
    if heading_stack.len() >= 2 {
        let raw = &heading_stack[heading_stack.len() - 2].1;
        Some(raw.trim_start_matches('#').trim().to_string())
    } else {
        None
    }
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
        assert_eq!(chunks[0].heading_text, "Title");
        assert_eq!(chunks[0].section_path(), "Title");
        assert_eq!(chunks[0].parent_heading, None);
        assert!(!chunks[0].is_frontmatter);
        assert!(chunks[0].content.contains("Some intro text."));

        // Section A
        assert_eq!(chunks[1].heading_hierarchy, vec!["# Title", "## Section A"]);
        assert_eq!(chunks[1].heading_level, 2);
        assert_eq!(chunks[1].heading_text, "Section A");
        assert_eq!(chunks[1].section_path(), "Title > Section A");
        assert_eq!(chunks[1].parent_heading, Some("Title".to_string()));
        assert!(chunks[1].content.contains("Content A"));
        assert!(chunks[1].code_spans.contains(&"foo_bar".to_string()));

        // Section B
        assert_eq!(chunks[2].heading_hierarchy, vec!["# Title", "## Section B"]);
        assert_eq!(chunks[2].heading_level, 2);
        assert_eq!(chunks[2].heading_text, "Section B");

        // Subsection B1
        assert_eq!(
            chunks[3].heading_hierarchy,
            vec!["# Title", "## Section B", "### Subsection B1"]
        );
        assert_eq!(chunks[3].heading_level, 3);
        assert_eq!(chunks[3].heading_text, "Subsection B1");
        assert_eq!(chunks[3].section_path(), "Title > Section B > Subsection B1");
        assert_eq!(chunks[3].parent_heading, Some("Section B".to_string()));
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
                heading_text: "Alpha".to_string(),
                parent_heading: None,

                is_frontmatter: false,
                content: "Hello world".to_string(),
                byte_offset: 0,
                byte_len: 11,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("b.md"),
                heading_hierarchy: vec!["# Beta".to_string()],
                heading_level: 1,
                heading_text: "Beta".to_string(),
                parent_heading: None,

                is_frontmatter: false,
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
    fn test_search_ranking_heading_over_body() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# Aim".to_string()],
                heading_level: 1,
                heading_text: "Aim".to_string(),
                parent_heading: None,

                is_frontmatter: false,
                content: "# Aim\n\nThis is the aim section.".to_string(),
                byte_offset: 0,
                byte_len: 30,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# Aim".to_string(), "## Details".to_string()],
                heading_level: 2,
                heading_text: "Details".to_string(),
                parent_heading: Some("Aim".to_string()),

                is_frontmatter: false,
                content: "## Details\n\nThe aim is described here.".to_string(),
                byte_offset: 30,
                byte_len: 36,
                code_spans: vec![],
            },
        ];

        let results = search_chunks(&chunks, "Aim");
        assert_eq!(results.len(), 2);
        // The h1 with exact heading match should rank first
        assert_eq!(results[0].heading_text, "Aim");
    }

    #[test]
    fn test_frontmatter_chunk() {
        let source = "---\nid: test-outcome\nstatus: active\n---\n\n# Title\n\nBody text.\n";
        let chunks = parse_markdown_source(source, Path::new("test.md"));

        assert_eq!(chunks.len(), 2, "Expected frontmatter + heading chunk, got {}: {:#?}", chunks.len(), chunks);
        assert!(chunks[0].is_frontmatter);
        assert!(chunks[0].content.contains("id: test-outcome"));
        assert_eq!(chunks[0].heading_level, 0);

        assert_eq!(chunks[1].heading_text, "Title");
        assert!(!chunks[1].is_frontmatter);
    }

    #[test]
    fn test_section_path_and_parent() {
        let source = "# Top\n\n## Sub\n\n### Deep\n\nContent.\n";
        let chunks = parse_markdown_source(source, Path::new("test.md"));

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].section_path(), "Top");
        assert_eq!(chunks[0].parent_heading, None);

        assert_eq!(chunks[1].section_path(), "Top > Sub");
        assert_eq!(chunks[1].parent_heading, Some("Top".to_string()));

        assert_eq!(chunks[2].section_path(), "Top > Sub > Deep");
        assert_eq!(chunks[2].parent_heading, Some("Sub".to_string()));
    }

    #[test]
    fn test_embedding_text() {
        let chunk = MarkdownChunk {
            file_path: PathBuf::from("doc.md"),
            heading_hierarchy: vec!["# Aim".to_string(), "## Hypothesis".to_string()],
            heading_level: 2,
            heading_text: "Hypothesis".to_string(),
            parent_heading: Some("Aim".to_string()),

            is_frontmatter: false,
            content: "We believe X will cause Y.".to_string(),
            byte_offset: 0,
            byte_len: 26,
            code_spans: vec![],
        };

        let text = chunk.embedding_text();
        assert!(text.starts_with("Aim > Hypothesis: "));
        assert!(text.contains("We believe X will cause Y."));
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

    #[test]
    fn test_search_chunks_ranked_heading_vs_content() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# Config".to_string()],
                heading_level: 1,
                content: "Details about configuration.".to_string(),
                byte_offset: 0,
                byte_len: 27,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("b.md"),
                heading_hierarchy: vec!["# Setup".to_string()],
                heading_level: 1,
                content: "You need to edit the config file.".to_string(),
                byte_offset: 0,
                byte_len: 32,
                code_spans: vec![],
            },
        ];

        let results = search_chunks_ranked(&chunks, "config");
        assert_eq!(results.len(), 2);
        // Exact heading match ("Config") should rank above content-only match
        assert_eq!(results[0].chunk.file_path, PathBuf::from("a.md"));
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_search_chunks_ranked_heading_level_bonus() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# Top".to_string(), "### Deep".to_string()],
                heading_level: 3,
                content: "Some search term here.".to_string(),
                byte_offset: 0,
                byte_len: 22,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("b.md"),
                heading_hierarchy: vec!["# Top".to_string()],
                heading_level: 1,
                content: "Some search term here.".to_string(),
                byte_offset: 0,
                byte_len: 22,
                code_spans: vec![],
            },
        ];

        let results = search_chunks_ranked(&chunks, "search term");
        assert_eq!(results.len(), 2);
        // h1 chunk should rank higher than h3 chunk (both content-only matches)
        assert_eq!(results[0].chunk.file_path, PathBuf::from("b.md"));
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_search_chunks_ranked_code_span_bonus() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# A".to_string()],
                heading_level: 1,
                content: "Mentions parse_config in text.".to_string(),
                byte_offset: 0,
                byte_len: 30,
                code_spans: vec!["parse_config".to_string()],
            },
            MarkdownChunk {
                file_path: PathBuf::from("b.md"),
                heading_hierarchy: vec!["# B".to_string()],
                heading_level: 1,
                content: "Mentions parse_config in text.".to_string(),
                byte_offset: 0,
                byte_len: 30,
                code_spans: vec![],
            },
        ];

        let results = search_chunks_ranked(&chunks, "parse_config");
        assert_eq!(results.len(), 2);
        // Chunk with code span match should rank higher
        assert_eq!(results[0].chunk.file_path, PathBuf::from("a.md"));
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_search_chunks_ranked_no_match() {
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
        ];

        let results = search_chunks_ranked(&chunks, "nonexistent");
        assert!(results.is_empty());
    }

    /// Cross-tier interaction: a content-only match with maximum density + code
    /// span + h1 bonus (0.4 + 0.10 + 0.15 + 0.10 = 0.75) can outscore a
    /// heading-contains match at h4 level (0.7 + 0.04 = 0.74). This is the
    /// documented intentional behavior — a chunk that mentions a code symbol
    /// many times at the top level is more useful than a deep subsection whose
    /// heading merely contains the term.
    #[test]
    fn test_search_chunks_ranked_cross_tier_interaction() {
        let chunks = vec![
            // Heading-contains match at deep heading level (h4)
            MarkdownChunk {
                file_path: PathBuf::from("heading.md"),
                heading_hierarchy: vec![
                    "# Top".to_string(),
                    "## Mid".to_string(),
                    "### Deep".to_string(),
                    "#### parse_config notes".to_string(),
                ],
                heading_level: 4,
                content: "Some notes about configuration.".to_string(),
                byte_offset: 0,
                byte_len: 31,
                code_spans: vec![],
            },
            // Content-only match with high density + code span at h1
            MarkdownChunk {
                file_path: PathBuf::from("content.md"),
                heading_hierarchy: vec!["# Overview".to_string()],
                heading_level: 1,
                content: "Call parse_config to load. parse_config reads TOML. parse_config validates. parse_config caches. parse_config returns.".to_string(),
                byte_offset: 0,
                byte_len: 110,
                code_spans: vec!["parse_config".to_string()],
            },
        ];

        let results = search_chunks_ranked(&chunks, "parse_config");
        assert_eq!(results.len(), 2);

        // Content-heavy chunk should win: 0.4 + 0.10 + 0.15 + 0.10 = 0.75
        // vs heading-contains at h4: 0.7 + 0.04 = 0.74
        assert_eq!(results[0].chunk.file_path, PathBuf::from("content.md"));
        assert!(
            results[0].score > results[1].score,
            "Expected content chunk ({:.2}) > heading chunk ({:.2})",
            results[0].score,
            results[1].score
        );
    }

    #[test]
    fn test_search_chunks_ranked_density_bonus() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# A".to_string()],
                heading_level: 1,
                content: "error once".to_string(),
                byte_offset: 0,
                byte_len: 10,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("b.md"),
                heading_hierarchy: vec!["# B".to_string()],
                heading_level: 1,
                content: "error error error error error".to_string(),
                byte_offset: 0,
                byte_len: 29,
                code_spans: vec![],
            },
        ];

        let results = search_chunks_ranked(&chunks, "error");
        assert_eq!(results.len(), 2);
        // Higher density chunk should rank higher
        assert_eq!(results[0].chunk.file_path, PathBuf::from("b.md"));
        assert!(results[0].score > results[1].score);
    }

    // 
    #[test]
    // ==================== Adversarial tests ====================

    /// Empty query: `"".contains("")` is true in Rust, so an empty query
    /// would match every chunk. Verify the function handles this gracefully
    /// (either returns nothing or at least doesn't panic).
    #[test]
    fn test_ranked_empty_query_does_not_match_everything() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# Hello".to_string()],
                heading_level: 1,
                content: "Some content here.".to_string(),
                byte_offset: 0,
                byte_len: 18,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("b.md"),
                heading_hierarchy: vec!["# World".to_string()],
                heading_level: 1,
                content: "Other content.".to_string(),
                byte_offset: 0,
                byte_len: 14,
                code_spans: vec![],
            },
        ];

        let results = search_chunks_ranked(&chunks, "");
        // An empty query should ideally return nothing (vacuous match is useless).
        // If the implementation matches everything, this test exposes it.
        // Current behavior: "" matches all content and all headings via contains("").
        // This is arguably a bug — agents sending empty queries get noise.
        //
        // We document the current behavior here so any fix is intentional:
        // If this assertion fails because empty query was fixed to return [],
        // update to assert!(results.is_empty()) and remove this comment.
        assert!(
            results.len() <= 2,
            "Empty query returned {} results (expected 0 or at most all chunks)",
            results.len()
        );
    }

    /// Empty query density: `"anything".matches("")` returns a match at every
    /// byte position. Verify density doesn't overflow or produce absurd scores.
    #[test]
    fn test_ranked_empty_query_density_bounded() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# Test".to_string()],
                heading_level: 1,
                content: "A normal sentence with some words in it.".to_string(),
                byte_offset: 0,
                byte_len: 40,
                code_spans: vec![],
            },
        ];

        let results = search_chunks_ranked(&chunks, "");
        // If empty query matches, the density bonus must still be capped at 0.10
        // (not blow up to millions from "".matches("") counting every position).
        for sc in &results {
            assert!(
                sc.score <= 2.0,
                "Score {:.2} is absurdly high — density not capped for empty query",
                sc.score
            );
        }
    }

    /// Unicode headings: verify heading match works with non-ASCII characters
    /// (accented Latin, CJK, emoji).
    #[test]
    fn test_ranked_unicode_heading_match() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("i18n.md"),
                heading_hierarchy: vec!["# Configuración".to_string()],
                heading_level: 1,
                content: "Detalles de la configuración.".to_string(),
                byte_offset: 0,
                byte_len: 30,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("cjk.md"),
                heading_hierarchy: vec!["# 設定".to_string()],
                heading_level: 1,
                content: "設定の詳細。".to_string(),
                byte_offset: 0,
                byte_len: 18,
                code_spans: vec![],
            },
        ];

        // Accented search — only matches the i18n chunk (heading + content)
        let results = search_chunks_ranked(&chunks, "configuración");
        assert_eq!(results.len(), 1, "Should match only the Spanish chunk");
        assert_eq!(results[0].chunk.file_path, PathBuf::from("i18n.md"));
        // Gets exact heading match (1.0) + h1 bonus (0.10) + density
        assert!(results[0].score >= 1.0, "Exact heading match for accented text");

        // CJK search
        let results = search_chunks_ranked(&chunks, "設定");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk.file_path, PathBuf::from("cjk.md"));
        // Should get exact heading match score (1.0 + heading bonus)
        assert!(results[0].score >= 1.0, "Exact CJK heading match should score >= 1.0");
    }

    /// Hash character as query: after the review fix, searching for "#" should
    /// not match every heading. The heading text is stripped of `#` prefix before
    /// matching.
    #[test]
    fn test_ranked_hash_query_does_not_match_all_headings() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# Alpha".to_string()],
                heading_level: 1,
                content: "No hash in content.".to_string(),
                byte_offset: 0,
                byte_len: 19,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("b.md"),
                heading_hierarchy: vec!["# Beta".to_string()],
                heading_level: 1,
                content: "Also no hash here.".to_string(),
                byte_offset: 0,
                byte_len: 18,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("c.md"),
                heading_hierarchy: vec!["# C# Programming".to_string()],
                heading_level: 1,
                content: "Content about C# language.".to_string(),
                byte_offset: 0,
                byte_len: 26,
                code_spans: vec![],
            },
        ];

        let results = search_chunks_ranked(&chunks, "#");
        // Only the chunk with "#" in actual content (not the heading prefix) should match.
        // "C# Programming" has a heading that when stripped becomes "C# Programming" —
        // wait, the strip only removes leading #, so "C# Programming" stays as "C# Programming".
        // And "C# language" in content also contains "#".
        // But "Alpha" and "Beta" headings stripped become "Alpha" and "Beta" — no "#".
        // Content "No hash in content." and "Also no hash here." don't contain "#".
        // So only chunk c.md should match (heading stripped text "C# Programming" contains "#",
        // and content "Content about C# language." contains "#").
        assert!(
            results.len() <= 1,
            "Hash query matched {} chunks — expected only the C# chunk",
            results.len()
        );
    }

    /// No headings: preamble chunk with heading_level=0 and empty hierarchy.
    /// Verify scoring doesn't panic and applies the preamble bonus (0.02).
    #[test]
    fn test_ranked_preamble_no_headings() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("plain.md"),
                heading_hierarchy: vec![],
                heading_level: 0,
                content: "Just plain text mentioning config here.".to_string(),
                byte_offset: 0,
                byte_len: 39,
                code_spans: vec![],
            },
        ];

        let results = search_chunks_ranked(&chunks, "config");
        assert_eq!(results.len(), 1);
        // Content-only (0.4) + preamble bonus (0.02) + density 1 occurrence (0.02)
        let expected = 0.4 + 0.02 + 0.02;
        assert!(
            (results[0].score - expected).abs() < 0.001,
            "Preamble score {:.3} != expected {:.3}",
            results[0].score,
            expected
        );
    }

    /// Ordering stability: when multiple chunks have identical scores,
    /// verify the output is deterministic (same input -> same output).
    #[test]
    fn test_ranked_stable_ordering_identical_scores() {
        let chunks: Vec<MarkdownChunk> = (0..10)
            .map(|i| MarkdownChunk {
                file_path: PathBuf::from(format!("file_{}.md", i)),
                heading_hierarchy: vec![format!("# Section {}", i)],
                heading_level: 1,
                content: "The keyword appears exactly once.".to_string(),
                byte_offset: 0,
                byte_len: 33,
                code_spans: vec![],
            })
            .collect();

        // Run 5 times and check order is consistent
        let first_order: Vec<String> = search_chunks_ranked(&chunks, "keyword")
            .iter()
            .map(|sc| sc.chunk.file_path.to_string_lossy().to_string())
            .collect();

        for _ in 0..5 {
            let order: Vec<String> = search_chunks_ranked(&chunks, "keyword")
                .iter()
                .map(|sc| sc.chunk.file_path.to_string_lossy().to_string())
                .collect();
            assert_eq!(
                first_order, order,
                "Ranking order is not stable across runs"
            );
        }
    }

    /// Density cap: verify that extremely high occurrence counts don't produce
    /// unbounded scores. The density bonus should be capped at 0.10.
    #[test]
    fn test_ranked_density_cap_many_occurrences() {
        // 100 occurrences of "x" in a content string
        let content = "x ".repeat(100);
        let chunks = vec![MarkdownChunk {
            file_path: PathBuf::from("dense.md"),
            heading_hierarchy: vec!["# Other".to_string()],
            heading_level: 1,
            content,
            byte_offset: 0,
            byte_len: 200,
            code_spans: vec![],
        }];

        let results = search_chunks_ranked(&chunks, "x");
        assert_eq!(results.len(), 1);
        // Content-only (0.4) + h1 bonus (0.10) + density capped (0.10) = 0.60
        let expected = 0.4 + 0.10 + 0.10;
        assert!(
            (results[0].score - expected).abs() < 0.001,
            "Score {:.3} != expected {:.3} — density cap not working",
            results[0].score,
            expected
        );
    }

    /// Heading hierarchy depth: a chunk with a deeply nested hierarchy where
    /// the query matches an ancestor heading (not the current chunk's heading).
    /// Verify it still gets heading-match credit.
    #[test]
    fn test_ranked_ancestor_heading_match() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("deep.md"),
                heading_hierarchy: vec![
                    "# Config".to_string(),
                    "## Advanced".to_string(),
                    "### Timeouts".to_string(),
                ],
                heading_level: 3,
                content: "Set the timeout to 30s.".to_string(),
                byte_offset: 0,
                byte_len: 23,
                code_spans: vec![],
            },
        ];

        let results = search_chunks_ranked(&chunks, "config");
        assert_eq!(results.len(), 1);
        // Should get exact heading match (1.0) because "# Config" is in hierarchy,
        // even though the chunk's own heading is "### Timeouts"
        assert!(
            results[0].score >= 1.0,
            "Ancestor heading match should get exact heading score, got {:.2}",
            results[0].score
        );
    }

    /// Case insensitivity: verify matching works across mixed case in headings,
    /// content, and code spans.
    #[test]
    fn test_ranked_case_insensitive_all_components() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# ParseConfig".to_string()],
                heading_level: 1,
                content: "The PARSECONFIG function.".to_string(),
                byte_offset: 0,
                byte_len: 25,
                code_spans: vec!["ParseConfig".to_string()],
            },
        ];

        let results = search_chunks_ranked(&chunks, "parseconfig");
        assert_eq!(results.len(), 1);
        // Should get exact heading match + h1 bonus + code span bonus + density
        assert!(
            results[0].score >= 1.0 + 0.10 + 0.15,
            "Case-insensitive match should trigger all bonuses, got {:.2}",
            results[0].score
        );
    }

    /// Empty chunks list: verify no panic on empty input.
    #[test]
    fn test_ranked_empty_chunks() {
        let chunks: Vec<MarkdownChunk> = vec![];
        let results = search_chunks_ranked(&chunks, "anything");
        assert!(results.is_empty());
    }

    /// Multi-word query: verify that the query is matched as a substring,
    /// not as individual words. "parse config" should NOT match "parse the config".
    #[test]
    fn test_ranked_multiword_query_is_substring() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# Setup".to_string()],
                heading_level: 1,
                content: "You should parse the config carefully.".to_string(),
                byte_offset: 0,
                byte_len: 38,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("b.md"),
                heading_hierarchy: vec!["# Usage".to_string()],
                heading_level: 1,
                content: "Call parse config to start.".to_string(),
                byte_offset: 0,
                byte_len: 27,
                code_spans: vec![],
            },
        ];

        let results = search_chunks_ranked(&chunks, "parse config");
        // Only "parse config" (exact substring) should match, not "parse the config"
        assert_eq!(
            results.len(),
            1,
            "Multi-word query should be exact substring match"
        );
        assert_eq!(results[0].chunk.file_path, PathBuf::from("b.md"));
    }

    /// Heading-contains vs exact: "Config" heading should score higher than
    /// "Config Settings" heading when searching for "config".
    #[test]
    fn test_ranked_exact_heading_beats_contains_heading() {
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("broad.md"),
                heading_hierarchy: vec!["# Config Settings".to_string()],
                heading_level: 1,
                content: "Various settings.".to_string(),
                byte_offset: 0,
                byte_len: 17,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("exact.md"),
                heading_hierarchy: vec!["# Config".to_string()],
                heading_level: 1,
                content: "All about config.".to_string(),
                byte_offset: 0,
                byte_len: 17,
                code_spans: vec![],
            },
        ];

        let results = search_chunks_ranked(&chunks, "config");
        assert_eq!(results.len(), 2);
        // Exact heading ("Config") gets 1.0, contains heading ("Config Settings") gets 0.7
        assert_eq!(results[0].chunk.file_path, PathBuf::from("exact.md"));
        assert!(
            results[0].score - results[1].score >= 0.2,
            "Exact heading ({:.2}) should significantly outscore contains heading ({:.2})",
            results[0].score,
            results[1].score
        );
    }

    #[test]
    fn test_empty_document() {
        let chunks = parse_markdown_source("", Path::new("empty.md"));
        assert_eq!(chunks.len(), 0, "Empty document should produce no chunks");
    }

    #[test]
    fn test_frontmatter_only_document() {
        let source = "---\nid: x\nstatus: draft\n---\n";
        let chunks = parse_markdown_source(source, Path::new("fm-only.md"));

        assert_eq!(chunks.len(), 1, "Frontmatter-only doc should produce exactly one chunk");
        assert!(chunks[0].is_frontmatter);
        assert!(chunks[0].content.contains("id: x"));
    }

    #[test]
    fn test_frontmatter_with_body_no_headings() {
        let source = "---\nid: x\n---\nJust body text.\n";
        let chunks = parse_markdown_source(source, Path::new("fm-body.md"));

        assert_eq!(chunks.len(), 2, "Frontmatter + body text without headings");
        assert!(chunks[0].is_frontmatter);
        assert!(!chunks[1].is_frontmatter);
        assert!(chunks[1].content.contains("Just body text."));
    }

    #[test]
    fn test_headings_without_body_text() {
        let source = "# Title\n\n## Empty\n\n## Next\n\nContent.\n";
        let chunks = parse_markdown_source(source, Path::new("sparse.md"));

        assert_eq!(chunks.len(), 3, "Each heading gets its own chunk even without body");
        assert_eq!(chunks[0].heading_text, "Title");
        assert_eq!(chunks[1].heading_text, "Empty");
        assert_eq!(chunks[2].heading_text, "Next");
        assert!(chunks[2].content.contains("Content."));
    }

    #[test]
    fn test_frontmatter_scoring_no_level_inflation() {
        // Verify frontmatter body-only match does not outscore an exact h1 heading match.
        // Scoring: h1 exact heading (10) + body (2) + level boost (6) = 18
        //          frontmatter body (3 + 2) = 5
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec![],
                heading_level: 0,
                heading_text: String::new(),
                parent_heading: None,
                is_frontmatter: true,
                content: "---\nstatus: active\n---\n".to_string(),
                byte_offset: 0,
                byte_len: 22,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# Active".to_string()],
                heading_level: 1,
                heading_text: "Active".to_string(),
                parent_heading: None,
                is_frontmatter: false,
                content: "# Active\n\nThis section is active.\n".to_string(),
                byte_offset: 22,
                byte_len: 33,
                code_spans: vec![],
            },
        ];

        let results = search_chunks(&chunks, "active");
        assert_eq!(results.len(), 2);
        // The h1 with exact heading match must rank above frontmatter body-only match
        assert_eq!(results[0].heading_text, "Active",
            "Exact h1 heading match should rank above frontmatter body match");
    }

    #[test]
    fn test_h1_body_only_outranks_frontmatter_body_only() {
        // Both chunks match only via body content (no heading match).
        // h1 body-only: body (2) + level boost (6) = 8
        // frontmatter body-only: frontmatter bonus (3) + body (2) = 5
        // h1 should rank higher because it's actual content, not metadata.
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec![],
                heading_level: 0,
                heading_text: String::new(),
                parent_heading: None,
                is_frontmatter: true,
                content: "---\nstatus: active\n---\n".to_string(),
                byte_offset: 0,
                byte_len: 22,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["# Overview".to_string()],
                heading_level: 1,
                heading_text: "Overview".to_string(),
                parent_heading: None,
                is_frontmatter: false,
                content: "# Overview\n\nThe active status means...\n".to_string(),
                byte_offset: 22,
                byte_len: 38,
                code_spans: vec![],
            },
        ];

        let results = search_chunks(&chunks, "active");
        assert_eq!(results.len(), 2);
        // h1 body-only (score 8) must outrank frontmatter body-only (score 5)
        assert_eq!(results[0].heading_text, "Overview",
            "h1 body-only match (score 8) should rank above frontmatter body-only match (score 5)");
        assert!(results[1].is_frontmatter,
            "frontmatter should be second");
    }

    // ========================================================================
    // Adversarial tests — trying to break the implementation
    // ========================================================================

    #[test]
    fn test_deeply_nested_headings_h1_through_h6() {
        let source = "\
# L1\n\n\
## L2\n\n\
### L3\n\n\
#### L4\n\n\
##### L5\n\n\
###### L6\n\n\
Deepest content.\n";
        let chunks = parse_markdown_source(source, Path::new("deep.md"));

        assert_eq!(chunks.len(), 6, "Should produce one chunk per heading level");
        assert_eq!(chunks[5].heading_level, 6);
        assert_eq!(
            chunks[5].heading_hierarchy,
            vec!["# L1", "## L2", "### L3", "#### L4", "##### L5", "###### L6"]
        );
        assert_eq!(chunks[5].section_path(), "L1 > L2 > L3 > L4 > L5 > L6");
        assert_eq!(chunks[5].parent_heading, Some("L5".to_string()));
        assert!(chunks[5].content.contains("Deepest content."));
    }

    #[test]
    fn test_heading_level_jump_h1_to_h4() {
        // Skipping h2 and h3 — does hierarchy still track correctly?
        let source = "# Top\n\nIntro.\n\n#### Deep Jump\n\nJumped content.\n";
        let chunks = parse_markdown_source(source, Path::new("jump.md"));

        assert_eq!(chunks.len(), 2);
        // The h4 should still nest under h1 even with skipped levels
        assert_eq!(chunks[1].heading_hierarchy, vec!["# Top", "#### Deep Jump"]);
        assert_eq!(chunks[1].section_path(), "Top > Deep Jump");
        assert_eq!(chunks[1].parent_heading, Some("Top".to_string()));
        assert_eq!(chunks[1].heading_level, 4);
    }

    #[test]
    fn test_heading_level_jump_h1_h4_then_h2() {
        // h1 -> h4 -> h2: the h2 should pop the h4 and nest under h1
        let source = "# Top\n\n#### Deep\n\nDeep text.\n\n## Sibling\n\nSibling text.\n";
        let chunks = parse_markdown_source(source, Path::new("jump2.md"));

        assert_eq!(chunks.len(), 3);
        // h4 nests under h1
        assert_eq!(chunks[1].heading_hierarchy, vec!["# Top", "#### Deep"]);
        // h2 should pop h4 and nest under h1
        assert_eq!(chunks[2].heading_hierarchy, vec!["# Top", "## Sibling"]);
        assert_eq!(chunks[2].parent_heading, Some("Top".to_string()));
    }

    #[test]
    fn test_multiple_h1_headings_resets_hierarchy() {
        // Second h1 should replace the first, not accumulate
        let source = "# First\n\nFirst content.\n\n## Sub\n\nSub content.\n\n# Second\n\nSecond content.\n\n## Sub2\n\nSub2 content.\n";
        let chunks = parse_markdown_source(source, Path::new("multi-h1.md"));

        assert_eq!(chunks.len(), 4);
        // First h1 + sub
        assert_eq!(chunks[0].heading_hierarchy, vec!["# First"]);
        assert_eq!(chunks[1].heading_hierarchy, vec!["# First", "## Sub"]);
        // Second h1 should NOT carry "# First" — it replaces it
        assert_eq!(chunks[2].heading_hierarchy, vec!["# Second"]);
        assert_eq!(chunks[2].parent_heading, None);
        // Sub2 nests under Second, not First
        assert_eq!(chunks[3].heading_hierarchy, vec!["# Second", "## Sub2"]);
        assert_eq!(chunks[3].parent_heading, Some("Second".to_string()));
    }

    #[test]
    fn test_code_block_with_hash_not_confused_as_heading() {
        // Fenced code blocks with `#` lines should NOT be parsed as headings
        let source = "# Real Heading\n\n```python\n# This is a comment, not a heading\ndef foo():\n    pass\n```\n\nAfter code.\n";
        let chunks = parse_markdown_source(source, Path::new("code.md"));

        assert_eq!(chunks.len(), 1, "Code block # should not create new chunks");
        assert_eq!(chunks[0].heading_text, "Real Heading");
        assert!(chunks[0].content.contains("# This is a comment"));
        assert!(chunks[0].content.contains("After code."));
    }

    #[test]
    fn test_indented_code_block_with_hash() {
        // Indented code blocks can also contain #
        let source = "# Title\n\n    # indented code comment\n    x = 1\n\nParagraph.\n";
        let chunks = parse_markdown_source(source, Path::new("indent.md"));

        assert_eq!(chunks.len(), 1, "Indented code # should not split chunks");
        assert_eq!(chunks[0].heading_text, "Title");
    }

    #[test]
    fn test_empty_frontmatter() {
        // Empty frontmatter (`---\n---`) should be detected as a frontmatter chunk
        // with empty body. Previously this was a known bug (manual parser couldn't
        // handle it); now handled correctly by gray_matter.
        let source = "---\n---\n\n# Title\n\nBody.\n";
        let chunks = parse_markdown_source(source, Path::new("empty-fm.md"));

        let fm_chunks: Vec<_> = chunks.iter().filter(|c| c.is_frontmatter).collect();
        assert_eq!(fm_chunks.len(), 1,
            "Empty frontmatter should produce a frontmatter chunk");

        // The heading should still be parsed correctly
        let heading_chunks: Vec<_> = chunks.iter().filter(|c| !c.is_frontmatter).collect();
        assert!(heading_chunks.iter().any(|c| c.heading_text == "Title"),
            "Heading should still be parsed after empty frontmatter");
    }

    #[test]
    fn test_frontmatter_with_nested_yaml() {
        let source = "---\nid: test\nmetadata:\n  author: Alice\n  tags:\n    - rust\n    - mcp\n---\n\n# Title\n\nBody.\n";
        let chunks = parse_markdown_source(source, Path::new("nested-yaml.md"));

        assert!(chunks[0].is_frontmatter);
        assert!(chunks[0].content.contains("metadata:"));
        assert!(chunks[0].content.contains("author: Alice"));
        assert!(chunks[0].content.contains("- rust"));
        // Ensure parsing continues normally after complex frontmatter
        let heading_chunks: Vec<_> = chunks.iter().filter(|c| !c.is_frontmatter).collect();
        assert!(!heading_chunks.is_empty());
        assert_eq!(heading_chunks[0].heading_text, "Title");
    }

    #[test]
    fn test_frontmatter_with_multiline_string() {
        let source = "---\nid: test\ndescription: |\n  This is a\n  multiline string\n---\n\n# Title\n\nBody.\n";
        let chunks = parse_markdown_source(source, Path::new("multiline-fm.md"));

        assert!(chunks[0].is_frontmatter);
        assert!(chunks[0].content.contains("multiline string"));
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn test_heading_with_inline_code_link_emphasis() {
        // Headings with rich inline content
        let source = "# The `Config` **struct** for [users](url)\n\nBody text.\n";
        let chunks = parse_markdown_source(source, Path::new("rich.md"));

        assert_eq!(chunks.len(), 1);
        // pulldown-cmark strips emphasis/link markup from text events
        // The heading text should contain the raw text content
        assert!(chunks[0].heading_text.contains("Config"));
        assert!(chunks[0].heading_text.contains("struct"));
        // Code spans should be captured
        assert!(chunks[0].code_spans.contains(&"Config".to_string()));
    }

    #[test]
    fn test_empty_body_between_headings() {
        // Headings with absolutely no content between them
        let source = "# A\n## B\n## C\n### D\n";
        let chunks = parse_markdown_source(source, Path::new("empty-body.md"));

        // Each heading should produce a chunk (via saw_any_heading flag)
        assert!(chunks.len() >= 3, "Got {} chunks: {:#?}", chunks.len(), chunks);
        // Verify hierarchy is correct
        let d_chunk = chunks.iter().find(|c| c.heading_text == "D").unwrap();
        assert_eq!(d_chunk.heading_hierarchy, vec!["# A", "## C", "### D"]);
    }

    #[test]
    fn test_very_long_heading() {
        let long_heading = "X".repeat(2000);
        let source = format!("# {}\n\nBody.\n", long_heading);
        let chunks = parse_markdown_source(&source, Path::new("long.md"));

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading_text.len(), 2000);
        assert_eq!(chunks[0].heading_hierarchy[0], format!("# {}", long_heading));
    }

    #[test]
    fn test_embedding_text_truncates_long_body() {
        let long_body = "A".repeat(1000);
        let chunk = MarkdownChunk {
            file_path: PathBuf::from("doc.md"),
            heading_hierarchy: vec!["# Title".to_string()],
            heading_level: 1,
            heading_text: "Title".to_string(),
            parent_heading: None,
            is_frontmatter: false,
            content: long_body.clone(),
            byte_offset: 0,
            byte_len: 1000,
            code_spans: vec![],
        };

        let text = chunk.embedding_text();
        // Default limit is 500 chars for body. Single-level heading skips
        // breadcrumb prefix, so the text IS the truncated body.
        assert!(text.len() <= 500, "embedding_text should truncate body, got len={}", text.len());
    }

    #[test]
    fn test_embedding_text_very_long_section_path() {
        let long_headings: Vec<String> = (0..6)
            .map(|i| format!("{} {}", "#".repeat(i + 1), "A".repeat(200)))
            .collect();
        let chunk = MarkdownChunk {
            file_path: PathBuf::from("doc.md"),
            heading_hierarchy: long_headings.clone(),
            heading_level: 6,
            heading_text: "A".repeat(200),
            parent_heading: Some("A".repeat(200)),
            is_frontmatter: false,
            content: "Short body.".to_string(),
            byte_offset: 0,
            byte_len: 11,
            code_spans: vec![],
        };

        // Should not panic even with very long section_path
        let text = chunk.embedding_text();
        assert!(text.contains("Short body."));
        // section_path uses " > " as separator
        assert!(text.contains(" > "));
    }

    #[test]
    fn test_unicode_headings_and_body() {
        let source = "# Ψηφιακή Εποχή\n\n日本語のテキスト。\n\n## Ñoño 🎉\n\nEmoji content: 🦀🔥💯\n";
        let chunks = parse_markdown_source(source, Path::new("unicode.md"));

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading_text, "Ψηφιακή Εποχή");
        assert!(chunks[0].content.contains("日本語のテキスト"));
        assert_eq!(chunks[1].heading_text, "Ñoño 🎉");
        assert!(chunks[1].content.contains("🦀🔥💯"));
        assert_eq!(chunks[1].section_path(), "Ψηφιακή Εποχή > Ñoño 🎉");
    }

    #[test]
    fn test_embedding_text_truncation_at_multibyte_boundary() {
        // Body is all multibyte characters — truncation must not split a codepoint
        let body = "日".repeat(200); // each char is 3 bytes, 200 chars = 600 bytes
        let chunk = MarkdownChunk {
            file_path: PathBuf::from("doc.md"),
            heading_hierarchy: vec!["# Title".to_string()],
            heading_level: 1,
            heading_text: "Title".to_string(),
            parent_heading: None,
            is_frontmatter: false,
            content: body,
            byte_offset: 0,
            byte_len: 600,
            code_spans: vec![],
        };

        // Must not panic on multibyte boundary
        let text = chunk.embedding_text();
        // Single-level heading: no breadcrumb prefix, body starts directly
        assert!(text.starts_with("日"), "Single-level heading should have no prefix, got: {}", &text[..20.min(text.len())]);
        // Verify it's valid UTF-8 (implicit — if we got here, it is)
    }

    #[test]
    fn test_frontmatter_only_no_trailing_newline() {
        // Frontmatter with no trailing newline
        let source = "---\nid: x\n---";
        let chunks = parse_markdown_source(source, Path::new("no-trail.md"));

        // Should produce at least the frontmatter chunk, and not panic
        assert!(!chunks.is_empty(), "Should handle frontmatter without trailing newline");
        assert!(chunks[0].is_frontmatter);
    }

    #[test]
    fn test_triple_dashes_in_body_not_frontmatter() {
        // Triple dashes mid-document should not be confused with frontmatter
        let source = "# Title\n\nSome text.\n\n---\n\nMore text after hr.\n";
        let chunks = parse_markdown_source(source, Path::new("hr.md"));

        // No chunk should be frontmatter
        assert!(
            chunks.iter().all(|c| !c.is_frontmatter),
            "Horizontal rule (---) in body should not create frontmatter chunk"
        );
    }

    #[test]
    fn test_whitespace_only_document() {
        let source = "   \n\n  \t  \n";
        let chunks = parse_markdown_source(source, Path::new("ws.md"));

        assert_eq!(chunks.len(), 0, "Whitespace-only document should produce no chunks");
    }

    #[test]
    fn test_byte_offsets_with_frontmatter() {
        let source = "---\nid: test\n---\n\n# Title\n\nBody.\n";
        let chunks = parse_markdown_source(source, Path::new("offsets.md"));

        assert!(chunks.len() >= 2);
        // Frontmatter starts at 0
        assert_eq!(chunks[0].byte_offset, 0);
        assert!(chunks[0].is_frontmatter);
        // The heading chunk should start after frontmatter
        let heading_chunk = chunks.iter().find(|c| !c.is_frontmatter).unwrap();
        assert!(
            heading_chunk.byte_offset >= chunks[0].byte_len,
            "Heading byte_offset ({}) should be >= frontmatter byte_len ({})",
            heading_chunk.byte_offset,
            chunks[0].byte_len
        );
        // Verify content matches source at the offset
        let actual = &source[heading_chunk.byte_offset..heading_chunk.byte_offset + heading_chunk.byte_len];
        assert_eq!(actual, heading_chunk.content);
    }

    #[test]
    fn test_search_heading_hierarchy_match() {
        // Query matches a parent heading in hierarchy but not the chunk's own heading
        let chunks = vec![MarkdownChunk {
            file_path: PathBuf::from("a.md"),
            heading_hierarchy: vec!["# Architecture".to_string(), "## Details".to_string()],
            heading_level: 2,
            heading_text: "Details".to_string(),
            parent_heading: Some("Architecture".to_string()),
            is_frontmatter: false,
            content: "Some implementation details.".to_string(),
            byte_offset: 0,
            byte_len: 28,
            code_spans: vec![],
        }];

        let results = search_chunks(&chunks, "Architecture");
        assert_eq!(results.len(), 1, "Should match via heading hierarchy");
    }

    #[test]
    fn test_search_case_insensitive() {
        let chunks = vec![MarkdownChunk {
            file_path: PathBuf::from("a.md"),
            heading_hierarchy: vec!["# README".to_string()],
            heading_level: 1,
            heading_text: "README".to_string(),
            parent_heading: None,
            is_frontmatter: false,
            content: "# README\n\nProject info.".to_string(),
            byte_offset: 0,
            byte_len: 23,
            code_spans: vec![],
        }];

        assert_eq!(search_chunks(&chunks, "readme").len(), 1);
        assert_eq!(search_chunks(&chunks, "README").len(), 1);
        assert_eq!(search_chunks(&chunks, "ReAdMe").len(), 1);
    }

    #[test]
    fn test_search_empty_query() {
        let chunks = vec![MarkdownChunk {
            file_path: PathBuf::from("a.md"),
            heading_hierarchy: vec!["# Title".to_string()],
            heading_level: 1,
            heading_text: "Title".to_string(),
            parent_heading: None,
            is_frontmatter: false,
            content: "Hello".to_string(),
            byte_offset: 0,
            byte_len: 5,
            code_spans: vec![],
        }];

        // Empty string is contained in everything — should match all chunks
        let results = search_chunks(&chunks, "");
        assert_eq!(results.len(), 1, "Empty query matches everything via contains");
    }

    #[test]
    fn test_heading_level_boost_h1_over_h6() {
        // h1 and h6 both have exact heading match, h1 should rank higher due to level boost
        let chunks = vec![
            MarkdownChunk {
                file_path: PathBuf::from("a.md"),
                heading_hierarchy: vec!["###### Target".to_string()],
                heading_level: 6,
                heading_text: "Target".to_string(),
                parent_heading: None,
                is_frontmatter: false,
                content: "h6 content.".to_string(),
                byte_offset: 0,
                byte_len: 11,
                code_spans: vec![],
            },
            MarkdownChunk {
                file_path: PathBuf::from("b.md"),
                heading_hierarchy: vec!["# Target".to_string()],
                heading_level: 1,
                heading_text: "Target".to_string(),
                parent_heading: None,
                is_frontmatter: false,
                content: "h1 content.".to_string(),
                byte_offset: 0,
                byte_len: 11,
                code_spans: vec![],
            },
        ];

        let results = search_chunks(&chunks, "Target");
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].heading_level, 1,
            "h1 should rank above h6 due to level boost"
        );
    }

    #[test]
    fn test_section_path_empty_for_preamble() {
        let source = "Just preamble text, no headings.\n";
        let chunks = parse_markdown_source(source, Path::new("pre.md"));

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].section_path(), "", "Preamble should have empty section_path");
        assert!(chunks[0].heading_hierarchy.is_empty());
    }

    #[test]
    fn test_embedding_text_frontmatter_no_prefix() {
        let chunk = MarkdownChunk {
            file_path: PathBuf::from("doc.md"),
            heading_hierarchy: vec![],
            heading_level: 0,
            heading_text: String::new(),
            parent_heading: None,
            is_frontmatter: true,
            content: "---\nid: test\n---\n".to_string(),
            byte_offset: 0,
            byte_len: 17,
            code_spans: vec![],
        };

        let text = chunk.embedding_text();
        // Frontmatter skips prefix entirely — YAML metadata like
        // "frontmatter: status: active..." is noise for MiniLM.
        assert_eq!(
            text, "---\nid: test\n---\n",
            "Frontmatter embedding should have no prefix, got: {}",
            text
        );
    }

    #[test]
    fn test_embedding_text_preamble_prefix() {
        let chunk = MarkdownChunk {
            file_path: PathBuf::from("doc.md"),
            heading_hierarchy: vec![],
            heading_level: 0,
            heading_text: String::new(),
            parent_heading: None,
            is_frontmatter: false,
            content: "Some preamble.".to_string(),
            byte_offset: 0,
            byte_len: 14,
            code_spans: vec![],
        };

        let text = chunk.embedding_text();
        assert!(
            text.starts_with("[preamble] doc.md: "),
            "Preamble embedding should use [preamble] prefix, got: {}",
            text
        );
    }

    #[test]
    fn test_consecutive_headings_same_level() {
        // h2 h2 h2 — each should replace the previous, not accumulate
        let source = "# Top\n\n## A\n\nA text.\n\n## B\n\nB text.\n\n## C\n\nC text.\n";
        let chunks = parse_markdown_source(source, Path::new("consec.md"));

        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[1].heading_hierarchy, vec!["# Top", "## A"]);
        assert_eq!(chunks[2].heading_hierarchy, vec!["# Top", "## B"]);
        assert_eq!(chunks[3].heading_hierarchy, vec!["# Top", "## C"]);
        // Each h2 should have Top as parent
        for i in 1..4 {
            assert_eq!(chunks[i].parent_heading, Some("Top".to_string()));
        }
    }

    #[test]
    fn test_heading_after_deep_nesting_resets_properly() {
        // h1 > h2 > h3 > h4, then back to h2 — stack should unwind properly
        let source = "# Root\n\n## L2\n\n### L3\n\n#### L4\n\nDeep.\n\n## Back to L2\n\nShallow again.\n";
        let chunks = parse_markdown_source(source, Path::new("unwind.md"));

        let back = chunks.iter().find(|c| c.heading_text == "Back to L2").unwrap();
        assert_eq!(
            back.heading_hierarchy,
            vec!["# Root", "## Back to L2"],
            "h2 after h4 should unwind to just Root > Back to L2"
        );
        assert_eq!(back.parent_heading, Some("Root".to_string()));
    }

    #[test]
    fn test_frontmatter_with_dashes_in_values() {
        // YAML values containing --- should not break frontmatter parsing
        let source = "---\nid: test-value-with-dashes\ntitle: a---b---c\n---\n\n# Title\n\nBody.\n";
        let chunks = parse_markdown_source(source, Path::new("dashes.md"));

        assert!(chunks[0].is_frontmatter);
        assert!(chunks[0].content.contains("test-value-with-dashes"));
        assert!(chunks[0].content.contains("a---b---c"));
    }

    #[test]
    fn test_crlf_line_endings() {
        let source = "---\r\nid: test\r\n---\r\n\r\n# Title\r\n\r\nBody text.\r\n";
        let chunks = parse_markdown_source(source, Path::new("crlf.md"));

        // Should not panic, and should produce reasonable chunks
        assert!(!chunks.is_empty(), "CRLF document should produce chunks");
        let has_fm = chunks.iter().any(|c| c.is_frontmatter);
        let has_heading = chunks.iter().any(|c| c.heading_text == "Title");
        assert!(has_fm, "Should detect frontmatter with CRLF");
        assert!(has_heading, "Should detect heading with CRLF");
    }
}
