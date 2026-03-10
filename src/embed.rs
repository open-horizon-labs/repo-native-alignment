use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{Float32Array, RecordBatch, RecordBatchIterator, StringArray};
use arrow_schema::{DataType, Field, Schema};
use lancedb::query::{ExecutableQuery, QueryBase};

use crate::git;
use crate::oh;

/// Embedding batch size. Benchmarked on M4 MacBook Pro with MiniLM-L6-v2:
/// batch=32 gives best sustained throughput (~880 t/s) with ~240MB memory.
/// Larger batches (64, 128) showed no improvement or regression.
const BATCH_SIZE: usize = 32;

/// Number of recent commits to embed for temporal context.
const RECENT_COMMIT_LIMIT: usize = 100;
/// Number of PR merge commits to embed for structural context.
const PR_MERGE_LIMIT: usize = 50;

/// Maximum character budget for code embedding text.
///
/// MiniLM-L6-v2 has a 256-token effective max sequence length.  Code
/// tokenizes poorly with WordPiece (~3.5 subword tokens per identifier),
/// so we budget ~650 chars to target ~180 tokens, safely within 256.
/// The name is always included; the body (which already contains the
/// signature) gets at least 50% of the budget; metadata fills remaining space.
const CODE_EMBED_CHAR_BUDGET: usize = 650;

/// Truncate `s` to at most `max_chars` Unicode scalar values, returning a
/// valid UTF-8 slice. Safe even when a multibyte character straddles the
/// byte boundary (the original panic trigger).
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// Build embedding text for a code node within the MiniLM-L6-v2 token budget.
///
/// Layout: `name body_excerpt [metadata]`
///
/// `body` already includes the signature (it's the full AST node text),
/// so we don't push the signature separately.  The body is truncated so
/// the total stays within [`CODE_EMBED_CHAR_BUDGET`].
fn build_code_embedding_text(
    name: &str,
    body: &str,
    metadata: &std::collections::BTreeMap<String, String>,
) -> String {
    let mut t = String::with_capacity(CODE_EMBED_CHAR_BUDGET);
    t.push_str(name);

    let name_chars = name.chars().count();
    // Body always gets at least 50% of the budget
    let min_body_budget = CODE_EMBED_CHAR_BUDGET / 2;
    let after_name = CODE_EMBED_CHAR_BUDGET.saturating_sub(name_chars + 1);

    // Estimate metadata cost in chars (not bytes) and cap to leave room for body
    let meta_estimate: usize = metadata
        .iter()
        .map(|(k, v)| k.chars().count() + v.chars().count() + 3) // " key: value"
        .sum();
    let meta_budget = after_name.saturating_sub(min_body_budget).min(meta_estimate);

    // Truncate metadata entries to fit within meta_budget
    let mut meta_parts: Vec<String> = Vec::new();
    let mut meta_used = 0usize;
    for (key, value) in metadata {
        let entry = format!(" {}: {}", key, value);
        let entry_chars = entry.chars().count();
        if meta_used + entry_chars > meta_budget {
            break;
        }
        meta_used += entry_chars;
        meta_parts.push(entry);
    }

    // Body gets everything remaining after name and actual metadata used
    let body_budget = after_name.saturating_sub(meta_used);

    if body_budget > 0 && !body.is_empty() {
        t.push(' ');
        t.push_str(truncate_chars(body, body_budget));
    }

    for part in &meta_parts {
        t.push_str(part);
    }

    // Final safety truncation to hard budget
    truncate_chars(&t, CODE_EMBED_CHAR_BUDGET).to_string()
}

fn new_model() -> Result<metal_candle::embeddings::EmbeddingModel> {
    let start = std::time::Instant::now();

    #[cfg(feature = "metal")]
    let device = candle_core::Device::new_metal(0).unwrap_or_else(|_| {
        tracing::info!("EmbeddingIndex: Metal GPU not available, using CPU");
        candle_core::Device::Cpu
    });
    #[cfg(not(feature = "metal"))]
    let device = candle_core::Device::Cpu;

    #[cfg(feature = "metal")]
    let device_name = if matches!(device, candle_core::Device::Metal(_)) { "Metal GPU" } else { "CPU" };
    #[cfg(not(feature = "metal"))]
    let device_name = "CPU";

    let model = metal_candle::embeddings::EmbeddingModel::from_pretrained(
        metal_candle::embeddings::EmbeddingModelType::AllMiniLmL6V2,
        device,
    )
    .map_err(|e| anyhow::anyhow!("Failed to load embedding model: {}", e));

    match &model {
        Ok(m) => tracing::info!(
            "EmbeddingIndex: MiniLM-L6-v2 ready on {} (dim={}) in {:?}",
            device_name, m.dimension(), start.elapsed()
        ),
        Err(err) => tracing::warn!(
            "EmbeddingIndex: model load failed in {:?}: {}",
            start.elapsed(), err
        ),
    }
    model
}

async fn embed_texts(texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    let total = texts.len();
    if total == 0 {
        return Ok(Vec::new());
    }

    let batch_size = BATCH_SIZE;
    let total_chars: usize = texts.iter().map(|t| t.len()).sum();
    let total_batches = total.div_ceil(batch_size);
    let overall_start = std::time::Instant::now();
    tracing::info!(
        "EmbeddingIndex: embedding {} text(s) across {} batch(es) ({} chars total)",
        total, total_batches, total_chars
    );

    let model = new_model()?;
    let mut remaining = texts;
    let mut all_embeddings = Vec::with_capacity(total);
    let mut processed = 0usize;

    for batch_idx in 0..total_batches {
        let batch_size = remaining.len().min(batch_size);
        let batch: Vec<String> = remaining.drain(..batch_size).collect();
        let batch_start = std::time::Instant::now();

        let refs: Vec<&str> = batch.iter().map(|s| s.as_str()).collect();
        let tensor = model.encode(&refs)
            .map_err(|e| anyhow::anyhow!("Embedding failed: {}", e))?;
        let batch_embeddings: Vec<Vec<f32>> = tensor.to_vec2::<f32>()
            .map_err(|e| anyhow::anyhow!("Tensor conversion failed: {}", e))?;

        processed += batch_embeddings.len();
        tracing::info!(
            "EmbeddingIndex: batch {}/{} done in {:?} ({}/{})",
            batch_idx + 1, total_batches, batch_start.elapsed(), processed, total
        );
        all_embeddings.extend(batch_embeddings);
    }

    tracing::info!(
        "EmbeddingIndex: embedded {} text(s) in {:?}",
        processed, overall_start.elapsed()
    );
    Ok(all_embeddings)
}


/// The embedding index: wraps LanceDB with fastembed for semantic search over .oh/ artifacts.
pub struct EmbeddingIndex {
    db: lancedb::Connection,
    table_name: String,
}

/// A search result with the artifact and its relevance score.
pub struct SearchResult {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub score: f32,
}

impl SearchResult {
    pub fn to_markdown(&self) -> String {
        let snippet = if self.body.len() > 200 {
            format!("{}...", &self.body[..200])
        } else {
            self.body.clone()
        };
        if self.kind.starts_with("code:") {
            format!(
                "- **{}** ({}) — relevance: {:.2}\n  {}\n  ID: `{}`\n",
                self.title, self.kind, self.score, snippet, self.id
            )
        } else if self.kind == "commit" {
            format!(
                "- **{}** ({}) — relevance: {:.2}\n  {}\n  Hash: `{}` (use: `git show {}`)\n",
                self.title, self.kind, self.score, snippet, self.id, self.id
            )
        } else {
            format!(
                "- **{}** ({}) — relevance: {:.2}\n  {}\n",
                self.title, self.kind, self.score, snippet
            )
        }
    }
}

impl EmbeddingIndex {
    /// Create or open the embedding index. Stores data in memory.
    pub async fn new(repo_root: &Path) -> Result<Self> {
        let db_path = repo_root.join(".oh").join(".cache").join("embeddings");
        std::fs::create_dir_all(&db_path)?;
        tracing::debug!("EmbeddingIndex: opening LanceDB at {}", db_path.display());
        let open_start = std::time::Instant::now();

        let db = lancedb::connect(db_path.to_str().unwrap())
            .execute()
            .await
            .context("Failed to connect to LanceDB")?;
        tracing::debug!(
            "EmbeddingIndex: opened LanceDB at {} in {:?}",
            db_path.display(),
            open_start.elapsed()
        );

        Ok(Self {
            db,
            table_name: "artifacts".to_string(),
        })
    }

    /// Index all .oh/ artifacts, git commits, and optionally code symbols.
    /// Call with symbols from the graph to enable semantic code search.
    pub async fn index_all_with_symbols(
        &self,
        repo_root: &Path,
        symbols: &[crate::graph::Node],
    ) -> Result<usize> {
        self.index_all_inner(repo_root, symbols).await
    }

    /// Index all .oh/ artifacts and recent git commits. Rebuilds the table from scratch.
    pub async fn index_all(&self, repo_root: &Path) -> Result<usize> {
        self.index_all_inner(repo_root, &[]).await
    }

    /// Re-embed a targeted subset of nodes and upsert them into the existing table.
    ///
    /// Use this after LSP enrichment to update embeddings for only the nodes whose
    /// metadata was patched — avoiding a full table rebuild for every incremental update.
    /// If the table does not yet exist, falls back to a no-op (caller must run
    /// `index_all_with_symbols` first).
    pub async fn reindex_nodes(&self, nodes: &[crate::graph::Node]) -> Result<usize> {
        if nodes.is_empty() {
            return Ok(0);
        }

        let mut ids: Vec<String> = Vec::new();
        let mut kinds: Vec<String> = Vec::new();
        let mut titles: Vec<String> = Vec::new();
        let mut bodies: Vec<String> = Vec::new();
        let mut texts: Vec<String> = Vec::new();

        for node in nodes {
            let kind_str = match &node.id.kind {
                crate::graph::NodeKind::Other(s) => s.clone(),
                k => format!("{}", k),
            };

            let text = build_code_embedding_text(&node.id.name, &node.body, &node.metadata);

            let title = format!("{} {} ({})", kind_str, node.id.name, node.language);
            let body_display = format!(
                "{}\n\n{}:{}",
                node.signature,
                node.id.file.display(),
                node.line_start
            );

            ids.push(node.stable_id());
            kinds.push(format!("code:{}", kind_str));
            titles.push(title);
            bodies.push(body_display);
            texts.push(text);
        }

        let count = texts.len();
        let embeddings = embed_texts(texts).await?;
        let dim = embeddings[0].len();
        let flat_embeddings: Vec<f32> = embeddings.into_iter().flatten().collect();

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("kind", DataType::Utf8, false),
            Field::new("title", DataType::Utf8, false),
            Field::new("body", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    dim as i32,
                ),
                false,
            ),
        ]));

        let id_array = Arc::new(StringArray::from(ids)) as Arc<dyn arrow_array::Array>;
        let kind_array = Arc::new(StringArray::from(kinds)) as Arc<dyn arrow_array::Array>;
        let title_array = Arc::new(StringArray::from(titles)) as Arc<dyn arrow_array::Array>;
        let body_array = Arc::new(StringArray::from(bodies)) as Arc<dyn arrow_array::Array>;
        let values = Arc::new(Float32Array::from(flat_embeddings));
        let list_field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array = Arc::new(arrow_array::FixedSizeListArray::try_new(
            list_field, dim as i32, values, None,
        )?) as Arc<dyn arrow_array::Array>;

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![id_array, kind_array, title_array, body_array, vector_array],
        )?;

        // Upsert into the existing table by node id.
        let table = match self.db.open_table(&self.table_name).execute().await {
            Ok(t) => t,
            Err(_) => {
                // Table not yet created — nothing to update.
                return Ok(0);
            }
        };

        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        let mut merge = table.merge_insert(&["id"]);
        merge
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        merge
            .execute(Box::new(batches))
            .await
            .context("Failed to upsert enriched node embeddings")?;

        Ok(count)
    }

    async fn index_all_inner(&self, repo_root: &Path, symbols: &[crate::graph::Node]) -> Result<usize> {
        let index_start = std::time::Instant::now();
        tracing::info!(
            "EmbeddingIndex: rebuilding full index for {}",
            repo_root.display()
        );
        let artifacts = oh::load_oh_artifacts(repo_root)?;
        let artifact_count = artifacts.len();

        // Collect ids, kinds, titles, bodies, and embedding texts from artifacts
        let mut ids: Vec<String> = Vec::new();
        let mut kinds: Vec<String> = Vec::new();
        let mut titles: Vec<String> = Vec::new();
        let mut bodies: Vec<String> = Vec::new();
        let mut texts: Vec<String> = Vec::new();

        for a in &artifacts {
            ids.push(a.id());
            kinds.push(a.kind.to_string());
            titles.push(
                a.frontmatter
                    .get("title")
                    .and_then(|v| v.as_str())
                    .or_else(|| a.frontmatter.get("statement").and_then(|v| v.as_str()))
                    .unwrap_or(&a.id())
                    .to_string(),
            );
            bodies.push(a.body.clone());

            let mut text = String::new();
            text.push_str(&a.id());
            text.push(' ');
            for (k, v) in &a.frontmatter {
                if let Some(s) = v.as_str() {
                    text.push_str(k);
                    text.push_str(": ");
                    text.push_str(s);
                    text.push(' ');
                }
            }
            text.push_str(&a.body);
            texts.push(text);
        }

        // Index recent git commits (capped for performance)
        let commit_count = match git::load_commits(repo_root, RECENT_COMMIT_LIMIT) {
            Ok(commits) => {
                for c in &commits {
                    let changed_files_str = c
                        .changed_files
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let body = format!("{}\n\nFiles: {}", c.message, changed_files_str);
                    let title = c.message.lines().next().unwrap_or(&c.message).to_string();

                    ids.push(c.short_hash.clone());
                    kinds.push("commit".to_string());
                    titles.push(title);
                    bodies.push(body.clone());
                    texts.push(body);
                }
                commits.len()
            }
            Err(err) => {
                tracing::debug!(
                    "EmbeddingIndex: failed to load commits for {}: {}",
                    repo_root.display(),
                    err
                );
                0
            }
        };
        // Index PR merge commits for structural context (what shipped)
        let mut seen_merge_shas = std::collections::HashSet::new();
        let merge_count = match git::pr_merges::extract_pr_merges(repo_root, Some(PR_MERGE_LIMIT)) {
            Ok((merge_nodes, _edges)) => {
                for node in &merge_nodes {
                    let merge_sha = node.metadata.get("merge_sha").cloned().unwrap_or_default();
                    let short = merge_sha.get(..7).unwrap_or(&merge_sha).to_string();
                    if seen_merge_shas.contains(&short) {
                        continue;
                    }
                    seen_merge_shas.insert(short.clone());

                    let branch = node.metadata.get("branch_name").cloned().unwrap_or_default();
                    let files = node.metadata.get("files_changed").cloned().unwrap_or_default();
                    let body = format!("{}\n\nBranch: {}\nFiles: {}", node.body, branch, files);

                    ids.push(format!("merge:{}", short));
                    kinds.push("merge".to_string());
                    titles.push(node.signature.clone());
                    bodies.push(body.clone());
                    texts.push(body);
                }
                seen_merge_shas.len()
            }
            Err(_) => 0,
        };

        // Filter to embeddable node kinds before counting/indexing
        let embeddable: Vec<&crate::graph::Node> = symbols.iter()
            .filter(|n| n.id.kind.is_embeddable())
            .collect();
        let skipped = symbols.len() - embeddable.len();
        tracing::info!(
            "EmbeddingIndex: collected {} artifact(s), {} commit(s), {} merge(s), {} symbol(s) ({} embeddable, {} skipped) for indexing",
            artifact_count,
            commit_count,
            merge_count,
            symbols.len(),
            embeddable.len(),
            skipped,
        );

        // Index code symbols and markdown sections from the graph
        for node in &embeddable {
            let kind_str = match &node.id.kind {
                crate::graph::NodeKind::Other(s) => s.clone(),
                k => format!("{}", k),
            };

            // Build searchable text for embedding.
            //
            // For code symbols we include name + truncated body so that
            // intent-based queries like "error handling" or "rate limiting" can
            // match functions whose *body* implements that concept even when the
            // function name/signature doesn't mention it.
            //
            // body already contains the signature (full AST node text), so we
            // don't push the signature separately — avoids wasting tokens on
            // redundant text.
            //
            // Total text is budgeted to ~800 chars to stay within MiniLM-L6-v2's
            // 256-token effective limit (code tokenizes at ~3-4 tokens/identifier).
            //
            // This mirrors what `reindex_nodes` does for LSP-enriched nodes,
            // closing the gap where the initial full build embedded only the
            // signature.
            let text = match node.id.kind {
                crate::graph::NodeKind::Other(ref s) if s == "markdown_section" || s == "Section" => {
                    // Markdown sections: just the body text, no breadcrumb prefix.
                    // Mirrors MarkdownChunk::embedding_text() — the section path
                    // adds no validated value for MiniLM-L6-v2.
                    truncate_chars(&node.body, 500).to_string()
                }
                _ => {
                    build_code_embedding_text(&node.id.name, &node.body, &node.metadata)
                }
            };

            let title = format!("{} {} ({})", kind_str, node.id.name, node.language);
            let body_display = format!(
                "{}\n\n{}:{}",
                node.signature,
                node.id.file.display(),
                node.line_start
            );

            ids.push(node.stable_id());
            kinds.push(format!("code:{}", kind_str));
            titles.push(title);
            bodies.push(body_display);
            texts.push(text);
        }


        if texts.is_empty() {
            tracing::info!("EmbeddingIndex: no texts collected for {}", repo_root.display());
            return Ok(0);
        }

        let count = texts.len();
        tracing::info!(
            "EmbeddingIndex: preparing {} row(s) for full index rebuild",
            count
        );

        // Compute embeddings
        let embed_start = std::time::Instant::now();
        let embeddings = embed_texts(texts).await?;
        let dim = embeddings[0].len();
        let flat_embeddings: Vec<f32> = embeddings.into_iter().flatten().collect();
        tracing::info!(
            "EmbeddingIndex: computed {} embedding row(s) with dimension {} in {:?}",
            count,
            dim,
            embed_start.elapsed()
        );

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("kind", DataType::Utf8, false),
            Field::new("title", DataType::Utf8, false),
            Field::new("body", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    dim as i32,
                ),
                false,
            ),
        ]));

        // Split into batches of 2048 rows to avoid lance panics on large writes (#110).
        const WRITE_BATCH_SIZE: usize = 2048;
        let persist_start = std::time::Instant::now();
        let _ = self.db.drop_table(&self.table_name, &[]).await;

        let total_rows = ids.len();
        let mut offset = 0;
        let mut first = true;
        while offset < total_rows {
            let end = (offset + WRITE_BATCH_SIZE).min(total_rows);
            let batch_ids: Vec<String> = ids[offset..end].to_vec();
            let batch_kinds: Vec<String> = kinds[offset..end].to_vec();
            let batch_titles: Vec<String> = titles[offset..end].to_vec();
            let batch_bodies: Vec<String> = bodies[offset..end].to_vec();
            let batch_flat: Vec<f32> = flat_embeddings[offset * dim..end * dim].to_vec();

            let id_array = Arc::new(StringArray::from(batch_ids)) as Arc<dyn arrow_array::Array>;
            let kind_array = Arc::new(StringArray::from(batch_kinds)) as Arc<dyn arrow_array::Array>;
            let title_array = Arc::new(StringArray::from(batch_titles)) as Arc<dyn arrow_array::Array>;
            let body_array = Arc::new(StringArray::from(batch_bodies)) as Arc<dyn arrow_array::Array>;
            let values = Arc::new(Float32Array::from(batch_flat));
            let list_field = Arc::new(Field::new("item", DataType::Float32, true));
            let vector_array = Arc::new(arrow_array::FixedSizeListArray::try_new(
                list_field, dim as i32, values, None,
            )?) as Arc<dyn arrow_array::Array>;

            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![id_array, kind_array, title_array, body_array, vector_array],
            )?;

            if first {
                let batches = RecordBatchIterator::new(vec![Ok(batch)], schema.clone());
                self.db
                    .create_table(&self.table_name, Box::new(batches))
                    .execute()
                    .await
                    .context("Failed to create LanceDB table")?;
                first = false;
            } else {
                let table = self.db.open_table(&self.table_name).execute().await
                    .context("Failed to open table for append")?;
                let batches = RecordBatchIterator::new(vec![Ok(batch)], schema.clone());
                table.add(Box::new(batches)).execute().await
                    .context("Failed to append batch to LanceDB table")?;
            }

            tracing::info!(
                "EmbeddingIndex: persisted batch {}-{} of {} rows",
                offset, end, total_rows
            );
            offset = end;
        }
        tracing::info!(
            "EmbeddingIndex: persisted {} row(s) to LanceDB in {:?} (total {:?})",
            count,
            persist_start.elapsed(),
            index_start.elapsed()
        );

        Ok(count)
    }

    /// Semantic search over indexed artifacts.
    pub async fn search(
        &self,
        query: &str,
        artifact_types: Option<&[String]>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let table = self
            .db
            .open_table(&self.table_name)
            .execute()
            .await
            .context("Table not found — run index_all first")?;

        // Embed the query
        let query_embedding = embed_texts(vec![query.to_string()]).await?;

        let mut search = table
            .vector_search(query_embedding[0].clone())
            .context("Failed to create vector search")?;

        search = search.limit(limit * 3); // over-fetch to filter by type

        let results = search
            .execute()
            .await
            .context("Vector search failed")?;

        use futures::TryStreamExt;
        let batches: Vec<RecordBatch> = results.try_collect().await?;

        let mut search_results = Vec::new();

        for batch in &batches {
            let ids = batch
                .column_by_name("id")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let kinds = batch
                .column_by_name("kind")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let titles = batch
                .column_by_name("title")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let bodies = batch
                .column_by_name("body")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let distances = batch
                .column_by_name("_distance")
                .unwrap()
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap();

            for i in 0..batch.num_rows() {
                let kind = kinds.value(i).to_string();

                // Filter by artifact type if specified
                if let Some(types) = artifact_types {
                    if !types.iter().any(|t| t == &kind) {
                        continue;
                    }
                }

                // Convert distance to similarity score (1 - distance for L2, or just negate)
                let score = 1.0 - distances.value(i);

                search_results.push(SearchResult {
                    id: ids.value(i).to_string(),
                    kind,
                    title: titles.value(i).to_string(),
                    body: bodies.value(i).to_string(),
                    score,
                });

                if search_results.len() >= limit {
                    break;
                }
            }
        }

        Ok(search_results)
    }
}

#[cfg(test)]
mod tests {
    use super::{truncate_chars, build_code_embedding_text, CODE_EMBED_CHAR_BUDGET};
    use std::collections::BTreeMap;

    #[test]
    fn test_truncate_chars_ascii() {
        let s = "a".repeat(600);
        let result = truncate_chars(&s, 500);
        assert_eq!(result.len(), 500);
    }

    #[test]
    fn test_truncate_chars_multibyte_boundary() {
        let mut s = "a".repeat(498);
        s.push('—');
        s.push_str(&"b".repeat(100));
        let result = truncate_chars(&s, 500);
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        assert_eq!(result.chars().count(), 500);
    }

    #[test]
    fn test_truncate_chars_short_string() {
        let s = "hello";
        assert_eq!(truncate_chars(s, 500), "hello");
    }

    #[test]
    fn test_truncate_chars_exact_boundary() {
        let s = "a".repeat(500);
        assert_eq!(truncate_chars(&s, 500), s.as_str());
    }

    #[test]
    fn test_code_embedding_text_within_budget() {
        // Simulate a realistic Rust function body (body includes signature)
        let name = "handle_timeout_error";
        let body = concat!(
            "pub async fn handle_timeout_error(&self, req: Request) -> Result<Response> {\n",
            "    let timeout = self.config.timeout_ms;\n",
            "    let result = tokio::time::timeout(\n",
            "        Duration::from_millis(timeout),\n",
            "        self.inner.call(req),\n",
            "    ).await;\n",
            "    match result {\n",
            "        Ok(Ok(resp)) => Ok(resp),\n",
            "        Ok(Err(e)) => Err(e.into()),\n",
            "        Err(_) => {\n",
            "            tracing::warn!(\"Request timed out after {}ms\", timeout);\n",
            "            Err(AppError::Timeout { duration_ms: timeout })\n",
            "        }\n",
            "    }\n",
            "}\n",
        );
        let metadata = BTreeMap::new();
        let text = build_code_embedding_text(name, body, &metadata);

        assert!(
            text.chars().count() <= CODE_EMBED_CHAR_BUDGET,
            "embedding text {} chars exceeds budget {}",
            text.chars().count(),
            CODE_EMBED_CHAR_BUDGET,
        );
        // Name should be present
        assert!(text.starts_with(name), "text should start with the function name");
        // Body content (signature) should be present -- but NOT duplicated
        assert!(text.contains("handle_timeout_error"), "text should contain function name");
        let sig_occurrences = text.matches("handle_timeout_error").count();
        // name appears once at start, once inside the body's signature line = 2 max
        assert!(
            sig_occurrences <= 2,
            "function name appears {} times — possible duplication",
            sig_occurrences,
        );
    }

    #[test]
    fn test_code_embedding_text_long_body_truncated() {
        let name = "process";
        // A body much larger than the budget
        let body = format!(
            "fn process(data: &[u8]) -> Result<()> {{\n{}\n}}",
            "x".repeat(2000)
        );
        let metadata = BTreeMap::new();
        let text = build_code_embedding_text(name, &body, &metadata);

        assert!(
            text.chars().count() <= CODE_EMBED_CHAR_BUDGET,
            "embedding text {} chars exceeds budget {}",
            text.chars().count(),
            CODE_EMBED_CHAR_BUDGET,
        );
    }

    #[test]
    fn test_code_embedding_text_with_metadata() {
        let name = "foo";
        let body = "fn foo() -> i32 { 42 }";
        let mut metadata = BTreeMap::new();
        metadata.insert("return_type".to_string(), "i32".to_string());
        metadata.insert("visibility".to_string(), "pub".to_string());
        let text = build_code_embedding_text(name, body, &metadata);

        assert!(
            text.chars().count() <= CODE_EMBED_CHAR_BUDGET,
            "embedding text {} chars exceeds budget {}",
            text.chars().count(),
            CODE_EMBED_CHAR_BUDGET,
        );
        // Should contain the metadata
        assert!(text.contains("return_type: i32"), "metadata should be in text");
    }

    #[test]
    fn test_code_embedding_no_signature_duplication() {
        // The key review finding: body already contains the signature.
        // We should NOT see name + signature + body (which would duplicate).
        let name = "search";
        let signature = "pub fn search(&self, query: &str) -> Vec<Result>";
        let body = format!(
            "{} {{\n    self.db.query(query).collect()\n}}",
            signature
        );
        let metadata = BTreeMap::new();
        let text = build_code_embedding_text(name, &body, &metadata);

        // The signature text should appear exactly once in the embedding
        // (inside the body excerpt), NOT separately prepended.
        let sig_count = text.matches(signature).count();
        assert_eq!(
            sig_count, 1,
            "signature should appear exactly once in embedding text, found {}",
            sig_count,
        );
    }

    // -----------------------------------------------------------------------
    // ADVERSARIAL TESTS: edge cases, budget attacks, consistency probes
    // -----------------------------------------------------------------------

    #[test]
    fn truncate_chars_empty_string() {
        assert_eq!(truncate_chars("", 500), "");
    }

    #[test]
    fn truncate_chars_zero_limit() {
        assert_eq!(truncate_chars("hello", 0), "");
    }

    /// Empty body should degrade gracefully: just the name, no crash.
    #[test]
    fn empty_body_degrades_gracefully() {
        let text = build_code_embedding_text("foo", "", &BTreeMap::new());
        assert!(text.contains("foo"), "must contain name");
        // Should not be just whitespace or have excessive padding
        assert_eq!(text.trim(), "foo", "empty body should produce just name (trimmed)");
    }

    /// Empty name: the function must not panic and body should still appear.
    #[test]
    fn empty_name_no_panic() {
        let text = build_code_embedding_text("", "fn () { 42 }", &BTreeMap::new());
        assert!(text.contains("fn () { 42 }"), "body must be present even with empty name");
    }

    /// Both name and body empty: should produce an empty or near-empty string.
    #[test]
    fn all_empty_inputs() {
        let text = build_code_embedding_text("", "", &BTreeMap::new());
        assert!(text.chars().count() <= CODE_EMBED_CHAR_BUDGET);
        // Should not crash
    }

    /// BUG PROBE: meta_estimate uses .len() (bytes) but body budget
    /// subtracts from a char-counted budget. For metadata with multibyte
    /// characters, the byte-based estimate overcounts, stealing more body
    /// budget than necessary.
    #[test]
    fn multibyte_metadata_steals_body_budget() {
        let name = "f";
        // 100 chars of body content
        let body = "x".repeat(100);

        // Metadata with multibyte values: 10 chars but 30 bytes each
        let mut metadata = BTreeMap::new();
        // Each value is 10 chars of 3-byte emoji = 30 bytes
        let emoji_val: String = std::iter::repeat('\u{1f600}').take(10).collect();
        metadata.insert("key".into(), emoji_val.clone());

        let text = build_code_embedding_text(name, &body, &metadata);

        // The metadata byte estimate will be: 3 + 30 + 3 = 36 bytes
        // But the actual char cost is: 3 + 10 + 3 = 16 chars
        // This means body_budget is reduced by 36 instead of 16,
        // wasting 20 chars of body budget.
        //
        // Verify the body content is still present (it's short enough
        // to fit even with the overcounting)
        assert!(text.contains(&"x".repeat(50)),
            "body should contain at least 50 x's, but meta byte overcounting \
             may have stolen body budget. Text: {}", text);
    }

    /// Attack: enormous name that exceeds CODE_EMBED_CHAR_BUDGET.
    /// The safety truncation must catch this.
    #[test]
    fn enormous_name_exceeds_budget() {
        let name = "a".repeat(2000);
        let body = "fn big() { }";
        let text = build_code_embedding_text(&name, body, &BTreeMap::new());

        assert!(
            text.chars().count() <= CODE_EMBED_CHAR_BUDGET,
            "enormous name ({} chars) must be truncated to budget ({}), got {}",
            name.len(), CODE_EMBED_CHAR_BUDGET, text.chars().count()
        );
    }

    /// Attack: enormous metadata that floods past budget.
    /// Even with final safety truncation, the metadata loop runs to
    /// completion before truncating. This is wasteful but not incorrect.
    #[test]
    fn enormous_metadata_within_budget() {
        let name = "f";
        let body = "fn f() {}";
        let mut metadata = BTreeMap::new();
        for i in 0..50 {
            metadata.insert(
                format!("type_ref_{:03}", i),
                format!("some::deeply::nested::module::Type{}<Generic{}, Another{}>", i, i, i),
            );
        }

        let text = build_code_embedding_text(name, body, &metadata);
        assert!(
            text.chars().count() <= CODE_EMBED_CHAR_BUDGET,
            "50 metadata entries must stay within budget, got {} chars",
            text.chars().count()
        );
    }

    /// BUG PROBE: When metadata is very large, body_budget saturates to 0.
    /// The body content (which includes the signature!) gets entirely
    /// dropped. The embedding becomes: name + metadata -- losing the
    /// function's actual code, which defeats the purpose of this PR.
    #[test]
    fn large_metadata_evicts_body_entirely() {
        let name = "search";
        let body = "pub fn search(&self, q: &str) -> Vec<R> { self.db.query(q) }";
        let mut metadata = BTreeMap::new();
        // ~600 bytes of metadata, which will consume most of the 800-char budget
        for i in 0..15 {
            metadata.insert(
                format!("param_type_{}", i),
                format!("std::collections::HashMap<String, Vec<Arc<Mutex<Type{}>>>>", i),
            );
        }

        let text = build_code_embedding_text(name, body, &metadata);
        // The body should still be present, at least partially
        // If meta_estimate > budget - name, body_budget = 0 and body is gone
        let has_body = text.contains("search") && text.contains("query");
        if !has_body {
            // Document the failure mode -- body was evicted
            panic!(
                "BUG: large metadata evicted the function body entirely. \
                 The embedding text is: name + metadata with no code content. \
                 This defeats the purpose of embedding function bodies.\n\
                 Text ({} chars): {}",
                text.chars().count(),
                &text[..text.len().min(300)]
            );
        }
    }

    /// Single-line function: body IS the signature. Embedding should
    /// contain it exactly once (from body), plus the name.
    #[test]
    fn single_line_function_no_duplication() {
        let sig_and_body = "fn default_port() -> u16 { 8080 }";
        let text = build_code_embedding_text("default_port", sig_and_body, &BTreeMap::new());

        let count = text.matches(sig_and_body).count();
        assert_eq!(
            count, 1,
            "single-line function should appear once in text, found {}. Text: {}",
            count, text
        );
    }

    /// Unicode in name and body: must not panic on multibyte boundaries.
    #[test]
    fn unicode_identifiers_safe() {
        let text = build_code_embedding_text(
            "\u{1f600}_handler",
            "fn \u{1f600}_handler() { let \u{03b1} = \u{03b2} + \u{03b3}; }",
            &BTreeMap::new(),
        );
        assert!(std::str::from_utf8(text.as_bytes()).is_ok(), "must be valid UTF-8");
        assert!(text.contains("\u{1f600}"), "emoji must survive");
    }

    /// Whitespace-only body should not produce excessive padding.
    #[test]
    fn whitespace_only_body() {
        let text = build_code_embedding_text("blank", "   \n\t\n   ", &BTreeMap::new());
        assert!(text.starts_with("blank"), "name must be at start");
        assert!(text.chars().count() <= CODE_EMBED_CHAR_BUDGET);
    }

    /// Determinism: same inputs always produce same output.
    #[test]
    fn embedding_text_deterministic() {
        let meta = BTreeMap::from([("k".into(), "v".into())]);
        let t1 = build_code_embedding_text("f", "fn f() {}", &meta);
        let t2 = build_code_embedding_text("f", "fn f() {}", &meta);
        assert_eq!(t1, t2, "embedding text must be deterministic");
    }

    /// BUG PROBE: The budget is 800 chars, but MiniLM-L6-v2 only accepts
    /// 256 tokens. At ~2.8 chars per code token (WordPiece), 800 chars =
    /// ~286 tokens, which exceeds the model's limit.
    ///
    /// This test constructs a maximal embedding text and estimates whether
    /// it could fit in 256 tokens.
    #[test]
    fn budget_800_chars_may_exceed_256_tokens() {
        // Fill exactly to budget with code-like content
        let name = "handle_complex_request";
        let body_content = concat!(
            "pub async fn handle_complex_request(&self, req: HttpRequest<Body>, ",
            "ctx: &RequestContext, middleware: &[Box<dyn Middleware>]) ",
            "-> Result<HttpResponse<Body>, AppError> {\n",
            "    let session = self.session_store.get_or_create(req.headers());\n",
            "    let auth_result = self.auth_service.validate_token(\n",
            "        req.headers().get(\"Authorization\"),\n",
            "        &session,\n",
            "    ).await?;\n",
            "    if !auth_result.has_permission(ctx.required_permission()) {\n",
            "        return Err(AppError::Forbidden {\n",
            "            user: auth_result.user_id().to_string(),\n",
            "            resource: ctx.resource_path().to_string(),\n",
            "        });\n",
            "    }\n",
            "    let mut response = self.inner_handler.handle(req, ctx).await?;\n",
            "    for mw in middleware.iter().rev() {\n",
            "        response = mw.after(response, ctx).await?;\n",
            "    }\n",
            "    self.metrics.record_request(ctx, &response);\n",
            "    Ok(response)\n",
            "}\n",
        );
        let text = build_code_embedding_text(name, body_content, &BTreeMap::new());
        let char_count = text.chars().count();

        // Conservative token estimate for code: ~2.8 chars per token
        // (identifiers like handle_complex_request split into multiple
        // subword tokens with WordPiece)
        let estimated_tokens = char_count as f64 / 2.8;

        // If this fails, the 800-char budget is too generous for 256 tokens.
        // It's a documentation assertion, not necessarily a hard failure.
        if estimated_tokens > 256.0 {
            panic!(
                "WARNING: embedding text of {} chars (~{:.0} estimated tokens) \
                 likely exceeds MiniLM-L6-v2's 256-token limit. The model will \
                 silently truncate, potentially discarding meaningful content.\n\
                 Consider reducing CODE_EMBED_CHAR_BUDGET to ~700 chars.\n\
                 Text (first 200 chars): {}",
                char_count,
                estimated_tokens,
                &text[..text.len().min(200)]
            );
        }
    }

    /// Verify that body_budget calculation doesn't underflow when name is
    /// close to CODE_EMBED_CHAR_BUDGET.
    #[test]
    fn body_budget_no_underflow() {
        // Name that's exactly at budget
        let name = "a".repeat(CODE_EMBED_CHAR_BUDGET);
        let text = build_code_embedding_text(&name, "body content", &BTreeMap::new());
        // Should not panic from underflow; body should be omitted
        assert!(text.chars().count() <= CODE_EMBED_CHAR_BUDGET);
    }

    /// When metadata byte-length equals char-length (ASCII only),
    /// the body budget calculation should be accurate.
    #[test]
    fn ascii_metadata_budget_accurate() {
        let name = "f";
        // Fill body to exactly what should fit
        let mut metadata = BTreeMap::new();
        metadata.insert("key".into(), "val".into());
        // meta entry " key: val" = 9 chars
        // after_name = 650 - 2 = 648
        // min_body_budget = 325
        // meta_budget = min(648 - 325, 9) = 9
        // body_budget = 648 - 9 = 639

        let body = "x".repeat(639);
        let text = build_code_embedding_text(name, &body, &metadata);

        // With ASCII metadata, byte == char, so budget should be exact
        assert!(
            text.chars().count() <= CODE_EMBED_CHAR_BUDGET,
            "text {} chars exceeds budget {}",
            text.chars().count(), CODE_EMBED_CHAR_BUDGET
        );
        // Body should be fully included (not truncated)
        assert!(
            text.contains(&"x".repeat(639)),
            "full body should fit within budget when metadata is small"
        );
    }
}
