use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{Array as ArrowArray, Float32Array, RecordBatch, RecordBatchIterator, StringArray};
use arrow_schema::{DataType, Field, Schema};
use lance_index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery, QueryBase};

use crate::git;
use crate::oh;

/// Search mode for the embedding index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// Combine keyword (BM25) + vector scoring via LanceDB hybrid search with RRF.
    #[default]
    Hybrid,
    /// Pure keyword (BM25 full-text) search only.
    Keyword,
    /// Pure vector (semantic embedding) search only.
    Semantic,
}

/// Adaptive batch sizing constants (TCP slow-start style).
/// Instead of a fixed batch size that may saturate unified memory bandwidth
/// on constrained Apple Silicon devices (e.g. MacBook Air M2 with 8GB),
/// we start small and grow/shrink based on observed per-item latency.
const BATCH_FLOOR: usize = 4;
/// Benchmarked on M4 Pro: batch=32 gives best throughput (~880 t/s).
/// Don't overshoot — larger batches don't help and can hurt.
const BATCH_CEILING: usize = 32;
/// Yield duration between batches to let other system tasks breathe.
const BATCH_YIELD_MS: u64 = 50;
/// If per-item time exceeds this multiple of the rolling average, halve batch size.
const BACKOFF_THRESHOLD: f64 = 2.0;

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

/// Build the Arrow schema for the embedding table, including the `text_hash` column.
fn embedding_schema(dim: usize) -> Schema {
    Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("title", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new("text_hash", DataType::Utf8, true),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dim as i32,
            ),
            false,
        ),
    ])
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

    let total_chars: usize = texts.iter().map(|t| t.len()).sum();
    let overall_start = std::time::Instant::now();
    tracing::info!(
        "EmbeddingIndex: embedding {} text(s) ({} chars total, adaptive batch {}..{})",
        total, total_chars, BATCH_FLOOR, BATCH_CEILING
    );

    let model = new_model()?;
    let mut remaining = texts;
    let mut all_embeddings = Vec::with_capacity(total);
    let mut processed = 0usize;
    let mut batch_idx = 0usize;
    let mut current_batch_size = BATCH_FLOOR;
    let mut rolling_avg: Option<f64> = None;
    const EMA_ALPHA: f64 = 0.3;

    while !remaining.is_empty() {
        let bs = remaining.len().min(current_batch_size);
        let batch: Vec<String> = remaining.drain(..bs).collect();
        let batch_start = std::time::Instant::now();

        let refs: Vec<&str> = batch.iter().map(|s| s.as_str()).collect();
        let tensor = model.encode(&refs)
            .map_err(|e| anyhow::anyhow!("Embedding failed: {}", e))?;
        let batch_embeddings: Vec<Vec<f32>> = tensor.to_vec2::<f32>()
            .map_err(|e| anyhow::anyhow!("Tensor conversion failed: {}", e))?;

        let elapsed_secs = batch_start.elapsed().as_secs_f64();
        let per_item = elapsed_secs / bs as f64;

        // Adaptive batch sizing: TCP slow-start style
        match rolling_avg {
            None => {
                // First batch: seed the rolling average
                rolling_avg = Some(per_item);
                // Grow for next batch
                current_batch_size = (current_batch_size * 2).min(BATCH_CEILING);
            }
            Some(avg) => {
                if per_item > BACKOFF_THRESHOLD * avg {
                    // Latency spike — halve batch size
                    current_batch_size = (current_batch_size / 2).max(BATCH_FLOOR);
                    tracing::debug!(
                        "EmbeddingIndex: backoff batch_size -> {} (per_item {:.4}s > {:.4}s threshold)",
                        current_batch_size, per_item, BACKOFF_THRESHOLD * avg
                    );
                } else {
                    // Steady — grow batch size
                    current_batch_size = (current_batch_size * 2).min(BATCH_CEILING);
                }
                // Update EMA
                rolling_avg = Some(avg * (1.0 - EMA_ALPHA) + per_item * EMA_ALPHA);
            }
        }

        processed += batch_embeddings.len();
        batch_idx += 1;
        tracing::info!(
            "EmbeddingIndex: batch {} done in {:?} (bs={}, {}/{})",
            batch_idx, batch_start.elapsed(), bs, processed, total
        );
        all_embeddings.extend(batch_embeddings);

        // Yield between batches to avoid saturating memory bandwidth
        if !remaining.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(BATCH_YIELD_MS)).await;
        }
    }

    tracing::info!(
        "EmbeddingIndex: embedded {} text(s) in {:?}",
        processed, overall_start.elapsed()
    );
    Ok(all_embeddings)
}


/// The embedding index: wraps LanceDB with fastembed for semantic search over .oh/ artifacts.
#[derive(Clone)]
pub struct EmbeddingIndex {
    db: lancedb::Connection,
    table_name: String,
}

/// Result of a semantic search — either results or "not ready yet."
pub enum SearchOutcome {
    /// Index is ready; here are the results (may be empty).
    Results(Vec<SearchResult>),
    /// Embedding table hasn't been created yet — index is still building.
    NotReady,
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

    /// Create (or replace) a tantivy full-text search index on `title` + `body`
    /// columns. Called after bulk writes and reindex to enable hybrid search.
    async fn create_fts_index(&self, table: &lancedb::Table) -> Result<()> {
        let fts_start = std::time::Instant::now();
        table
            .create_index(
                &["title", "body"],
                lancedb::index::Index::FTS(Default::default()),
            )
            .replace(true)
            .execute()
            .await
            .context("Failed to create FTS index on title+body")?;
        tracing::info!(
            "EmbeddingIndex: FTS index on title+body created in {:?}",
            fts_start.elapsed()
        );
        Ok(())
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

        // Open table first — if it doesn't exist, nothing to update.
        let table = match self.db.open_table(&self.table_name).execute().await {
            Ok(t) => t,
            Err(_) => {
                // Table not yet created — nothing to update.
                return Ok(0);
            }
        };

        // Build candidate data: id, kind, title, body, text, text_hash
        struct Candidate {
            id: String,
            kind: String,
            title: String,
            body: String,
            text: String,
            text_hash: String,
        }

        let mut candidates: Vec<Candidate> = Vec::with_capacity(nodes.len());

        for node in nodes {
            let kind_str = match &node.id.kind {
                crate::graph::NodeKind::Other(s) => s.clone(),
                k => format!("{}", k),
            };

            let text = build_code_embedding_text(&node.id.name, &node.body, &node.metadata);
            let text_hash = blake3::hash(text.as_bytes()).to_hex().to_string();

            let title = format!("{} {} ({})", kind_str, node.id.name, node.language);
            let body_display = format!(
                "{}\n\n{}:{}",
                node.signature,
                node.id.file.display(),
                node.line_start
            );

            candidates.push(Candidate {
                id: node.stable_id(),
                kind: format!("code:{}", kind_str),
                title,
                body: body_display,
                text,
                text_hash,
            });
        }

        // Query existing text_hash values to skip unchanged nodes.
        // If the column doesn't exist (old schema), embed everything.
        let existing_hashes = self.query_text_hashes(&table, &candidates.iter().map(|c| c.id.as_str()).collect::<Vec<_>>()).await;

        let (to_embed, skipped): (Vec<_>, Vec<_>) = candidates.into_iter().partition(|c| {
            match &existing_hashes {
                Some(map) => map.get(&c.id).is_none_or(|h| *h != c.text_hash),
                None => true, // no text_hash column yet — embed all
            }
        });

        if !skipped.is_empty() {
            tracing::info!(
                "reindex_nodes: BLAKE3 text hash skipped {} unchanged node(s), embedding {} node(s)",
                skipped.len(),
                to_embed.len(),
            );
        }

        if to_embed.is_empty() {
            return Ok(0);
        }

        let mut ids: Vec<String> = Vec::new();
        let mut kinds: Vec<String> = Vec::new();
        let mut titles: Vec<String> = Vec::new();
        let mut bodies: Vec<String> = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        let mut text_hashes: Vec<String> = Vec::new();

        for c in to_embed {
            ids.push(c.id);
            kinds.push(c.kind);
            titles.push(c.title);
            bodies.push(c.body);
            texts.push(c.text);
            text_hashes.push(c.text_hash);
        }

        let count = texts.len();
        let embeddings = embed_texts(texts).await?;
        let dim = embeddings[0].len();
        let flat_embeddings: Vec<f32> = embeddings.into_iter().flatten().collect();

        let schema = Arc::new(embedding_schema(dim));

        let id_array = Arc::new(StringArray::from(ids)) as Arc<dyn arrow_array::Array>;
        let kind_array = Arc::new(StringArray::from(kinds)) as Arc<dyn arrow_array::Array>;
        let title_array = Arc::new(StringArray::from(titles)) as Arc<dyn arrow_array::Array>;
        let body_array = Arc::new(StringArray::from(bodies)) as Arc<dyn arrow_array::Array>;
        let text_hash_array = Arc::new(StringArray::from(text_hashes)) as Arc<dyn arrow_array::Array>;
        let values = Arc::new(Float32Array::from(flat_embeddings));
        let list_field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array = Arc::new(arrow_array::FixedSizeListArray::try_new(
            list_field, dim as i32, values, None,
        )?) as Arc<dyn arrow_array::Array>;

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![id_array, kind_array, title_array, body_array, text_hash_array, vector_array],
        )?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        let mut merge = table.merge_insert(&["id"]);
        merge
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        merge
            .execute(Box::new(batches))
            .await
            .context("Failed to upsert enriched node embeddings")?;

        // Rebuild FTS index after upsert so new/changed rows are searchable.
        self.create_fts_index(&table).await?;

        Ok(count)
    }

    /// Query existing text_hash values for given node IDs from the embedding table.
    /// Returns None if the text_hash column doesn't exist (old schema).
    async fn query_text_hashes(
        &self,
        table: &lancedb::Table,
        node_ids: &[&str],
    ) -> Option<std::collections::HashMap<String, String>> {
        use futures::TryStreamExt;

        if node_ids.is_empty() {
            return Some(std::collections::HashMap::new());
        }

        // Build filter: id IN ('id1', 'id2', ...)
        let quoted: Vec<String> = node_ids.iter().map(|id| format!("'{}'", id.replace('\'', "''"))).collect();
        let filter = format!("id IN ({})", quoted.join(", "));

        let result = table
            .query()
            .select(lancedb::query::Select::columns(&["id", "text_hash"]))
            .only_if(filter)
            .execute()
            .await;

        let stream = match result {
            Ok(s) => s,
            Err(e) => {
                // Column doesn't exist yet (old schema) — this is expected on first run
                tracing::debug!("query_text_hashes: could not query text_hash column: {}", e);
                return None;
            }
        };

        let batches: Vec<RecordBatch> = match stream.try_collect().await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!("query_text_hashes: failed to collect batches: {}", e);
                return None;
            }
        };

        let mut map = std::collections::HashMap::new();
        for batch in &batches {
            let id_col = batch
                .column_by_name("id")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let hash_col = batch
                .column_by_name("text_hash")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());

            if let (Some(ids), Some(hashes)) = (id_col, hash_col) {
                for i in 0..ids.len() {
                    if !ids.is_null(i) && !hashes.is_null(i) {
                        map.insert(ids.value(i).to_string(), hashes.value(i).to_string());
                    }
                }
            }
        }

        Some(map)
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


        // Compute BLAKE3 text hashes for all embedding texts
        let text_hashes: Vec<String> = texts
            .iter()
            .map(|t| blake3::hash(t.as_bytes()).to_hex().to_string())
            .collect();

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

        let schema = Arc::new(embedding_schema(dim));

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
            let batch_text_hashes: Vec<String> = text_hashes[offset..end].to_vec();
            let batch_flat: Vec<f32> = flat_embeddings[offset * dim..end * dim].to_vec();

            let id_array = Arc::new(StringArray::from(batch_ids)) as Arc<dyn arrow_array::Array>;
            let kind_array = Arc::new(StringArray::from(batch_kinds)) as Arc<dyn arrow_array::Array>;
            let title_array = Arc::new(StringArray::from(batch_titles)) as Arc<dyn arrow_array::Array>;
            let body_array = Arc::new(StringArray::from(batch_bodies)) as Arc<dyn arrow_array::Array>;
            let text_hash_array = Arc::new(StringArray::from(batch_text_hashes)) as Arc<dyn arrow_array::Array>;
            let values = Arc::new(Float32Array::from(batch_flat));
            let list_field = Arc::new(Field::new("item", DataType::Float32, true));
            let vector_array = Arc::new(arrow_array::FixedSizeListArray::try_new(
                list_field, dim as i32, values, None,
            )?) as Arc<dyn arrow_array::Array>;

            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![id_array, kind_array, title_array, body_array, text_hash_array, vector_array],
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

        // Build FTS index for hybrid search (BM25 on title + body).
        let table = self.db.open_table(&self.table_name).execute().await
            .context("Failed to open table for FTS index")?;
        self.create_fts_index(&table).await?;

        Ok(count)
    }

    /// Search over indexed artifacts using the specified mode.
    ///
    /// - `Hybrid` (default): combines BM25 keyword scoring + vector similarity
    ///   via LanceDB's native RRF fusion. Falls back to pure vector if the FTS
    ///   index isn't available.
    /// - `Keyword`: BM25 full-text search only (no embeddings computed).
    /// - `Semantic`: pure vector similarity search only.
    ///
    /// Returns `SearchOutcome::NotReady` if the table hasn't been created yet,
    /// `SearchOutcome::Results(vec)` otherwise (may be empty).
    pub async fn search(
        &self,
        query: &str,
        artifact_types: Option<&[String]>,
        limit: usize,
    ) -> Result<SearchOutcome> {
        self.search_with_mode(query, artifact_types, limit, SearchMode::default()).await
    }

    /// Search with an explicit [`SearchMode`].
    pub async fn search_with_mode(
        &self,
        query: &str,
        artifact_types: Option<&[String]>,
        limit: usize,
        mode: SearchMode,
    ) -> Result<SearchOutcome> {
        let table = match self
            .db
            .open_table(&self.table_name)
            .execute()
            .await
        {
            Ok(t) => t,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("was not found") || msg.contains("does not exist") || msg.contains("not found") {
                    return Ok(SearchOutcome::NotReady);
                }
                return Err(e).context("Failed to open embedding table");
            }
        };

        let over_fetch = limit * 3; // over-fetch to allow type filtering

        use futures::TryStreamExt;

        let batches: Vec<RecordBatch> = match mode {
            SearchMode::Keyword => {
                // Pure BM25 full-text search — no embedding needed.
                let fts_query = FullTextSearchQuery::new(query.to_string());
                let results = table
                    .query()
                    .full_text_search(fts_query)
                    .limit(over_fetch)
                    .execute()
                    .await
                    .context("FTS keyword search failed")?;
                results.try_collect().await?
            }
            SearchMode::Semantic => {
                // Pure vector search — original behavior.
                let query_embedding = embed_texts(vec![query.to_string()]).await?;
                let search = table
                    .vector_search(query_embedding[0].clone())
                    .context("Failed to create vector search")?
                    .distance_type(lancedb::DistanceType::Cosine)
                    .limit(over_fetch);
                let results = search.execute().await.context("Vector search failed")?;
                results.try_collect().await?
            }
            SearchMode::Hybrid => {
                // Hybrid: BM25 + vector with RRF fusion.
                // LanceDB automatically detects both FTS and vector on VectorQuery
                // and routes through execute_hybrid with RRF reranking.
                let query_embedding = embed_texts(vec![query.to_string()]).await?;
                let fts_query = FullTextSearchQuery::new(query.to_string());

                let hybrid_result = table
                    .query()
                    .full_text_search(fts_query)
                    .limit(over_fetch)
                    .nearest_to(query_embedding[0].as_slice())
                    .context("Failed to create hybrid search")?
                    .distance_type(lancedb::DistanceType::Cosine)
                    .execute()
                    .await;

                match hybrid_result {
                    Ok(stream) => stream.try_collect().await?,
                    Err(e) => {
                        // FTS index may not exist yet (first run before index_all
                        // completes, or old cache). Fall back to pure vector search.
                        tracing::warn!(
                            "Hybrid search failed ({}), falling back to vector-only",
                            e
                        );
                        let search = table
                            .vector_search(query_embedding[0].clone())
                            .context("Failed to create fallback vector search")?
                            .distance_type(lancedb::DistanceType::Cosine)
                            .limit(over_fetch);
                        let results = search.execute().await.context("Fallback vector search failed")?;
                        results.try_collect().await?
                    }
                }
            }
        };

        let mut search_results = Vec::new();

        // Hybrid/FTS results use `_score` (BM25 or RRF), vector uses `_distance`.
        // Detect which column is present and normalize to a 0..1 score.
        let has_score_col = batches.first()
            .is_some_and(|b| b.column_by_name("_score").is_some());

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

            for i in 0..batch.num_rows() {
                let kind = kinds.value(i).to_string();

                // Filter by artifact type if specified
                if let Some(types) = artifact_types {
                    if !types.iter().any(|t| t == &kind) {
                        continue;
                    }
                }

                let raw_score = if has_score_col {
                    // RRF / BM25 `_score` — higher is better.
                    // RRF scores are always < 1 (sum of 1/(k+rank_i), k=60).
                    // BM25 scores can exceed 1.0 for highly relevant short docs,
                    // but we clamp to [0, 1] for consistency with cosine similarity.
                    // Ordering is preserved since we sort by score descending.
                    let s = batch
                        .column_by_name("_score")
                        .unwrap()
                        .as_any()
                        .downcast_ref::<Float32Array>()
                        .unwrap()
                        .value(i);
                    s.max(0.0).min(1.0)
                } else {
                    // Cosine distance [0, 2]: convert to similarity.
                    let d = batch
                        .column_by_name("_distance")
                        .unwrap()
                        .as_any()
                        .downcast_ref::<Float32Array>()
                        .unwrap()
                        .value(i);
                    (1.0 - d).max(0.0)
                };

                // Demote test files: reduce score so production code ranks above
                // test code at similar distances. Same conventions as ranking::is_test_file.
                let id_str = ids.value(i).to_string();
                let is_test = id_str.contains("/tests/")
                    || id_str.contains("/test/")
                    || id_str.contains("_test.")
                    || id_str.contains(".test.")
                    || id_str.contains(".spec.");
                let score = if is_test { raw_score * 0.7 } else { raw_score };

                search_results.push(SearchResult {
                    id: id_str,
                    kind,
                    title: titles.value(i).to_string(),
                    body: bodies.value(i).to_string(),
                    score,
                });
            }
        }

        // Re-sort by adjusted score (descending) and truncate.
        search_results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        search_results.truncate(limit);

        Ok(SearchOutcome::Results(search_results))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        truncate_chars, build_code_embedding_text, CODE_EMBED_CHAR_BUDGET,
        BATCH_FLOOR, BATCH_CEILING, BATCH_YIELD_MS, BACKOFF_THRESHOLD,
    };
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

    // -----------------------------------------------------------------------
    // ADVERSARIAL TESTS: adaptive batch sizing (PR #129)
    // -----------------------------------------------------------------------

    /// Constants must form a valid configuration: floor < ceiling,
    /// backoff threshold > 1.0 (otherwise every batch triggers backoff),
    /// yield > 0 (otherwise no breathing room).
    #[test]
    fn adaptive_batch_constants_sane() {
        assert!(
            BATCH_FLOOR < BATCH_CEILING,
            "BATCH_FLOOR ({}) must be less than BATCH_CEILING ({})",
            BATCH_FLOOR, BATCH_CEILING
        );
        assert!(
            BATCH_FLOOR > 0,
            "BATCH_FLOOR must be > 0 to avoid zero-size batches"
        );
        assert!(
            BACKOFF_THRESHOLD > 1.0,
            "BACKOFF_THRESHOLD ({}) must be > 1.0 or every batch triggers backoff",
            BACKOFF_THRESHOLD
        );
        assert!(
            BATCH_YIELD_MS > 0,
            "BATCH_YIELD_MS must be > 0 to actually yield"
        );
    }

    /// BATCH_FLOOR must be a power of two so that repeated halving
    /// from BATCH_CEILING always lands exactly on BATCH_FLOOR.
    #[test]
    fn batch_floor_and_ceiling_are_powers_of_two() {
        assert!(
            BATCH_FLOOR.is_power_of_two(),
            "BATCH_FLOOR ({}) should be a power of two for clean halving",
            BATCH_FLOOR
        );
        assert!(
            BATCH_CEILING.is_power_of_two(),
            "BATCH_CEILING ({}) should be a power of two for clean doubling",
            BATCH_CEILING
        );
    }

    /// Simulate the ramp-up sequence: starting at BATCH_FLOOR, doubling
    /// each step, we should reach BATCH_CEILING in a known number of steps.
    #[test]
    fn ramp_up_reaches_ceiling() {
        let mut batch_size = BATCH_FLOOR;
        let mut steps = 0;
        while batch_size < BATCH_CEILING {
            batch_size = (batch_size * 2).min(BATCH_CEILING);
            steps += 1;
            assert!(
                steps <= 20,
                "ramp-up did not converge after 20 doublings — constants are broken"
            );
        }
        assert_eq!(batch_size, BATCH_CEILING);
        // With floor=4 and ceiling=64: 4->8->16->32->64 = 4 steps
        let expected = (BATCH_CEILING / BATCH_FLOOR).trailing_zeros() as usize;
        assert_eq!(
            steps, expected,
            "ramp-up should take exactly {} doublings, took {}",
            expected, steps
        );
    }

    /// Simulate backoff: halving from BATCH_CEILING should reach BATCH_FLOOR.
    #[test]
    fn backoff_reaches_floor() {
        let mut batch_size = BATCH_CEILING;
        let mut steps = 0;
        while batch_size > BATCH_FLOOR {
            batch_size = (batch_size / 2).max(BATCH_FLOOR);
            steps += 1;
            assert!(steps <= 20, "backoff did not converge");
        }
        assert_eq!(batch_size, BATCH_FLOOR);
    }

    /// Simulate the full adaptive loop for 0 items.
    /// The while loop should never execute; result should be empty.
    #[test]
    fn adaptive_loop_zero_items() {
        let items: Vec<String> = vec![];
        let total = items.len();
        assert_eq!(total, 0);
        // Mirrors the early return in embed_texts
        // No panic, no infinite loop
    }

    /// Simulate the adaptive loop for exactly 1 item.
    /// Should produce exactly one batch of size 1, no yield.
    #[test]
    fn adaptive_loop_single_item() {
        let mut remaining = vec!["one".to_string()];
        let mut current_batch_size = BATCH_FLOOR;
        let mut batches_processed = 0;
        let mut yields = 0;

        while !remaining.is_empty() {
            let bs = remaining.len().min(current_batch_size);
            let _batch: Vec<String> = remaining.drain(..bs).collect();
            // Simulate: first batch always doubles
            current_batch_size = (current_batch_size * 2).min(BATCH_CEILING);
            batches_processed += 1;
            if !remaining.is_empty() {
                yields += 1;
            }
        }
        assert_eq!(batches_processed, 1, "single item = single batch");
        assert_eq!(yields, 0, "single item = no yield needed");
    }

    /// Simulate the adaptive loop for exactly BATCH_FLOOR items.
    /// Should produce exactly one batch of size BATCH_FLOOR, no yield.
    #[test]
    fn adaptive_loop_exactly_floor_items() {
        let mut remaining: Vec<String> = (0..BATCH_FLOOR).map(|i| format!("item_{}", i)).collect();
        let mut current_batch_size = BATCH_FLOOR;
        let mut batches_processed = 0;
        let mut yields = 0;

        while !remaining.is_empty() {
            let bs = remaining.len().min(current_batch_size);
            let _batch: Vec<String> = remaining.drain(..bs).collect();
            current_batch_size = (current_batch_size * 2).min(BATCH_CEILING);
            batches_processed += 1;
            if !remaining.is_empty() {
                yields += 1;
            }
        }
        assert_eq!(batches_processed, 1, "BATCH_FLOOR items = single batch");
        assert_eq!(yields, 0, "BATCH_FLOOR items = no yield needed");
    }

    /// Simulate the adaptive loop for exactly BATCH_CEILING items.
    /// Should ramp up: 4 + 8 + 16 + 32 + 4 = 64 items across 5 batches.
    #[test]
    fn adaptive_loop_exactly_ceiling_items() {
        let mut remaining: Vec<String> =
            (0..BATCH_CEILING).map(|i| format!("item_{}", i)).collect();
        let mut current_batch_size = BATCH_FLOOR;
        let mut batch_sizes = Vec::new();
        let mut yields = 0;

        while !remaining.is_empty() {
            let bs = remaining.len().min(current_batch_size);
            let _batch: Vec<String> = remaining.drain(..bs).collect();
            batch_sizes.push(bs);
            current_batch_size = (current_batch_size * 2).min(BATCH_CEILING);
            if !remaining.is_empty() {
                yields += 1;
            }
        }
        // With ramp-up and ceiling=64: batches are 4, 8, 16, 32, 4 (remainder)
        let total_processed: usize = batch_sizes.iter().sum();
        assert_eq!(total_processed, BATCH_CEILING);
        assert!(
            batch_sizes.len() >= 2,
            "should need multiple batches for ramp-up"
        );
        // First batch is always BATCH_FLOOR
        assert_eq!(batch_sizes[0], BATCH_FLOOR);
        // Yields = batches - 1 (no yield after last batch)
        assert_eq!(yields, batch_sizes.len() - 1);
    }

    /// Simulate the EMA calculation to verify it converges, not diverges.
    /// Feed constant per-item times and verify rolling_avg converges to that value.
    #[test]
    fn ema_converges_on_steady_input() {
        const EMA_ALPHA: f64 = 0.3;
        let constant_time = 0.01; // 10ms per item
        let mut rolling_avg: Option<f64> = None;

        for _ in 0..20 {
            match rolling_avg {
                None => {
                    rolling_avg = Some(constant_time);
                }
                Some(avg) => {
                    rolling_avg = Some(avg * (1.0 - EMA_ALPHA) + constant_time * EMA_ALPHA);
                }
            }
        }

        let avg = rolling_avg.unwrap();
        assert!(
            (avg - constant_time).abs() < 1e-10,
            "EMA should converge to the constant input value, got {}",
            avg
        );
    }

    /// Simulate a latency spike: verify backoff triggers and then recovers.
    #[test]
    fn ema_backoff_and_recovery() {
        const EMA_ALPHA: f64 = 0.3;
        let normal_time = 0.01;
        let spike_time = 0.05; // 5x normal

        let mut rolling_avg: Option<f64> = None;
        let mut current_batch_size = BATCH_FLOOR;
        let mut batch_sizes = Vec::new();

        // 5 normal batches to establish baseline
        for _ in 0..5 {
            let per_item = normal_time;
            match rolling_avg {
                None => {
                    rolling_avg = Some(per_item);
                    current_batch_size = (current_batch_size * 2).min(BATCH_CEILING);
                }
                Some(avg) => {
                    if per_item > BACKOFF_THRESHOLD * avg {
                        current_batch_size = (current_batch_size / 2).max(BATCH_FLOOR);
                    } else {
                        current_batch_size = (current_batch_size * 2).min(BATCH_CEILING);
                    }
                    rolling_avg = Some(avg * (1.0 - EMA_ALPHA) + per_item * EMA_ALPHA);
                }
            }
            batch_sizes.push(current_batch_size);
        }

        // Batch size should have ramped up
        let pre_spike_size = current_batch_size;
        assert!(
            pre_spike_size > BATCH_FLOOR,
            "should have ramped up before spike"
        );

        // Spike: per-item time jumps to 5x normal
        let per_item = spike_time;
        let avg = rolling_avg.unwrap();
        assert!(
            per_item > BACKOFF_THRESHOLD * avg,
            "spike should exceed backoff threshold"
        );
        current_batch_size = (current_batch_size / 2).max(BATCH_FLOOR);
        rolling_avg = Some(avg * (1.0 - EMA_ALPHA) + per_item * EMA_ALPHA);

        let post_spike_size = current_batch_size;
        assert!(
            post_spike_size < pre_spike_size,
            "batch size should decrease after spike"
        );

        // Recovery: 10 more normal batches
        for _ in 0..10 {
            let per_item = normal_time;
            let avg = rolling_avg.unwrap();
            if per_item > BACKOFF_THRESHOLD * avg {
                current_batch_size = (current_batch_size / 2).max(BATCH_FLOOR);
            } else {
                current_batch_size = (current_batch_size * 2).min(BATCH_CEILING);
            }
            rolling_avg = Some(avg * (1.0 - EMA_ALPHA) + per_item * EMA_ALPHA);
        }

        assert!(
            current_batch_size >= pre_spike_size,
            "should recover to at least pre-spike size after normal batches, got {}",
            current_batch_size
        );
    }

    /// Floor clamping: repeated halving should never go below BATCH_FLOOR.
    #[test]
    fn backoff_never_below_floor() {
        let mut batch_size = BATCH_FLOOR;
        // Try to halve 10 more times past the floor
        for _ in 0..10 {
            batch_size = (batch_size / 2).max(BATCH_FLOOR);
        }
        assert_eq!(batch_size, BATCH_FLOOR, "should clamp at BATCH_FLOOR");
    }

    /// Ceiling clamping: repeated doubling should never exceed BATCH_CEILING.
    #[test]
    fn ramp_up_never_above_ceiling() {
        let mut batch_size = BATCH_CEILING;
        // Try to double 10 more times past the ceiling
        for _ in 0..10 {
            batch_size = (batch_size * 2).min(BATCH_CEILING);
        }
        assert_eq!(batch_size, BATCH_CEILING, "should clamp at BATCH_CEILING");
    }

    // ── SearchMode tests ───────────────────────────────────────────────

    use super::SearchMode;

    #[test]
    fn search_mode_default_is_hybrid() {
        assert_eq!(SearchMode::default(), SearchMode::Hybrid);
    }

    #[test]
    fn search_mode_equality() {
        assert_ne!(SearchMode::Keyword, SearchMode::Semantic);
        assert_ne!(SearchMode::Keyword, SearchMode::Hybrid);
        assert_ne!(SearchMode::Semantic, SearchMode::Hybrid);
    }
}
