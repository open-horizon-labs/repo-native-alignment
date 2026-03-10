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

/// Truncate `s` to at most `max_chars` Unicode scalar values, returning a
/// valid UTF-8 slice. Safe even when a multibyte character straddles the
/// byte boundary (the original panic trigger).
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
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

            let mut text = String::new();
            text.push_str(&node.id.name);
            text.push(' ');
            text.push_str(&node.signature);
            text.push(' ');
            let body_snippet = truncate_chars(&node.body, 500);
            text.push_str(body_snippet);
            // Include LSP-enriched metadata so type-level queries find these nodes.
            for (key, value) in &node.metadata {
                text.push(' ');
                text.push_str(key);
                text.push_str(": ");
                text.push_str(value);
            }

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
            // For code symbols we include signature + truncated body so that
            // intent-based queries like "error handling" or "rate limiting" can
            // match functions whose *body* implements that concept even when the
            // function name/signature doesn't mention it.  The body is truncated
            // to 500 chars to stay within the 512-token budget of MiniLM-L6-v2.
            //
            // This mirrors what `reindex_nodes` already does for LSP-enriched
            // nodes, closing the gap where the initial full build embedded only
            // the signature.
            let text = match node.id.kind {
                crate::graph::NodeKind::Other(ref s) if s == "markdown_section" || s == "Section" => {
                    // Markdown sections: just the body text, no breadcrumb prefix.
                    // Mirrors MarkdownChunk::embedding_text() — the section path
                    // adds no validated value for MiniLM-L6-v2.
                    truncate_chars(&node.body, 500).to_string()
                }
                _ => {
                    // Code: signature + body snippet + metadata for semantic richness
                    let mut t = String::new();
                    t.push_str(&node.id.name);
                    t.push(' ');
                    t.push_str(&node.signature);
                    t.push(' ');
                    let body_snippet = truncate_chars(&node.body, 500);
                    t.push_str(body_snippet);
                    // Include any metadata (e.g. LSP-enriched type info)
                    for (key, value) in &node.metadata {
                        t.push(' ');
                        t.push_str(key);
                        t.push_str(": ");
                        t.push_str(value);
                    }
                    t
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
    use super::truncate_chars;

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
}
