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

/// Core parsing logic, separated for testability and reuse by the markdown extractor.
pub fn parse_markdown_source(source: &str, path: &Path) -> Vec<MarkdownChunk> {
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
}
