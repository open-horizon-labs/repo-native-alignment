use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use arrow_array::{Float32Array, RecordBatch, RecordBatchIterator, StringArray, Int64Array};
use arrow_schema::{DataType, Field, Schema};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use lancedb::query::{ExecutableQuery, QueryBase};

use crate::git;
use crate::oh;

/// Number of recent commits to embed for temporal context.
const RECENT_COMMIT_LIMIT: usize = 500;
/// Number of PR merge commits to embed for structural context.
const PR_MERGE_LIMIT: usize = 250;

/// Truncate `s` to at most `max_chars` Unicode scalar values, returning a
/// valid UTF-8 slice. Safe even when a multibyte character straddles the
/// byte boundary (the original panic trigger).
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

fn new_model() -> Result<TextEmbedding> {
    TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallENV15)
            .with_show_download_progress(false),
    )
    .map_err(|e| anyhow::anyhow!("Failed to load embedding model: {}", e))
}

fn embed_texts(model: &mut TextEmbedding, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
    model
        .embed(texts, None)
        .map_err(|e| anyhow::anyhow!("Embedding failed: {}", e))
}

/// Content hash for embedding cache keying.
fn content_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex()[..32].to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Embed Cache — content-addressed vector cache surviving table drops
// ─────────────────────────────────────────────────────────────────────────────

const CACHE_TABLE: &str = "vectors";
const MAX_CACHE_ENTRIES: usize = 50_000;

/// Content-addressed embedding cache. Maps BLAKE3(text) → vector.
/// Stored in a separate LanceDB directory so it survives search table drops.
pub struct EmbedCache {
    db: lancedb::Connection,
}

fn cache_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("content_hash", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                384,
            ),
            false,
        ),
        Field::new("last_used", DataType::Int64, false),
        Field::new("created_at", DataType::Int64, false),
    ]))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

impl EmbedCache {
    pub async fn new(repo_root: &Path) -> Result<Self> {
        let db_path = repo_root.join(".oh").join(".cache").join("embed_cache");
        std::fs::create_dir_all(&db_path)?;
        let db = lancedb::connect(db_path.to_str().unwrap())
            .execute()
            .await
            .context("Failed to connect to embed cache LanceDB")?;
        Ok(Self { db })
    }

    /// Look up cached vectors by content hash.
    /// Returns a map of hash → vector for cache hits.
    async fn lookup(&self, hashes: &[String]) -> Result<HashMap<String, Vec<f32>>> {
        let table = match self.db.open_table(CACHE_TABLE).execute().await {
            Ok(t) => t,
            Err(_) => return Ok(HashMap::new()),
        };

        let mut hits = HashMap::new();
        // Query in chunks to avoid huge IN clauses
        for chunk in hashes.chunks(500) {
            let filter = chunk
                .iter()
                .map(|h| format!("'{}'", h))
                .collect::<Vec<_>>()
                .join(", ");
            let query = table
                .query()
                .only_if(format!("content_hash IN ({})", filter))
                .execute()
                .await;
            if let Ok(stream) = query {
                use futures::TryStreamExt;
                let batches: Vec<RecordBatch> = stream.try_collect().await.unwrap_or_default();
                for batch in &batches {
                    let hash_col = batch
                        .column_by_name("content_hash")
                        .unwrap()
                        .as_any()
                        .downcast_ref::<StringArray>()
                        .unwrap();
                    let vector_col = batch
                        .column_by_name("vector")
                        .unwrap()
                        .as_any()
                        .downcast_ref::<arrow_array::FixedSizeListArray>()
                        .unwrap();
                    for i in 0..batch.num_rows() {
                        let h = hash_col.value(i).to_string();
                        let arr = vector_col.value(i);
                        let values = arr
                            .as_any()
                            .downcast_ref::<Float32Array>()
                            .unwrap();
                        let vec: Vec<f32> = (0..values.len()).map(|j| values.value(j)).collect();
                        hits.insert(h, vec);
                    }
                }
            }
        }
        Ok(hits)
    }

    /// Store newly computed vectors in the cache.
    async fn store(&self, entries: Vec<(String, Vec<f32>)>) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let now = unix_now();
        let hashes: Vec<String> = entries.iter().map(|(h, _)| h.clone()).collect();
        let flat_vectors: Vec<f32> = entries.iter().flat_map(|(_, v)| v.iter().copied()).collect();
        let last_used: Vec<i64> = vec![now; entries.len()];
        let created_at: Vec<i64> = vec![now; entries.len()];

        let schema = cache_schema();
        let hash_array = Arc::new(StringArray::from(hashes)) as Arc<dyn arrow_array::Array>;
        let values = Arc::new(Float32Array::from(flat_vectors));
        let list_field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array = Arc::new(arrow_array::FixedSizeListArray::try_new(
            list_field, 384, values, None,
        )?) as Arc<dyn arrow_array::Array>;
        let last_used_array = Arc::new(Int64Array::from(last_used)) as Arc<dyn arrow_array::Array>;
        let created_at_array = Arc::new(Int64Array::from(created_at)) as Arc<dyn arrow_array::Array>;

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![hash_array, vector_array, last_used_array, created_at_array],
        )?;
        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema.clone());

        match self.db.open_table(CACHE_TABLE).execute().await {
            Ok(table) => {
                let mut merge = table.merge_insert(&["content_hash"]);
                merge
                    .when_matched_update_all(None)
                    .when_not_matched_insert_all();
                merge
                    .execute(Box::new(batches))
                    .await
                    .context("Failed to upsert embed cache")?;
            }
            Err(_) => {
                self.db
                    .create_table(CACHE_TABLE, Box::new(batches))
                    .execute()
                    .await
                    .context("Failed to create embed cache table")?;
            }
        }
        Ok(())
    }

    /// Evict least-recently-used entries if cache exceeds max size.
    async fn evict(&self) -> Result<usize> {
        let table = match self.db.open_table(CACHE_TABLE).execute().await {
            Ok(t) => t,
            Err(_) => return Ok(0),
        };

        let count = table
            .count_rows(None)
            .await
            .unwrap_or(0);

        if count <= MAX_CACHE_ENTRIES {
            return Ok(0);
        }

        // Find the last_used cutoff: we want to keep MAX_CACHE_ENTRIES entries
        // Delete the oldest (count - MAX_CACHE_ENTRIES) entries
        let to_evict = count - MAX_CACHE_ENTRIES;

        // Get the cutoff timestamp by querying sorted by last_used ascending
        let stream = table
            .query()
            .select(lancedb::query::Select::columns(&["last_used"]))
            .only_if("true".to_string())
            .limit(to_evict)
            .execute()
            .await;

        if let Ok(stream) = stream {
            use futures::TryStreamExt;
            let batches: Vec<RecordBatch> = stream.try_collect().await.unwrap_or_default();
            if let Some(last_batch) = batches.last() {
                let ts_col = last_batch
                    .column_by_name("last_used")
                    .unwrap()
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap();
                if last_batch.num_rows() > 0 {
                    // Find max last_used in the eviction set
                    let cutoff = (0..last_batch.num_rows())
                        .map(|i| ts_col.value(i))
                        .max()
                        .unwrap_or(0);
                    let _ = table
                        .delete(&format!("last_used <= {}", cutoff))
                        .await;
                    return Ok(to_evict);
                }
            }
        }
        Ok(0)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Embedding Index — semantic search over artifacts, commits, and code
// ─────────────────────────────────────────────────────────────────────────────

/// The embedding index: wraps LanceDB with fastembed for semantic search over .oh/ artifacts.
pub struct EmbeddingIndex {
    db: lancedb::Connection,
    table_name: String,
    model: Mutex<TextEmbedding>,
    cache: EmbedCache,
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
        } else if self.kind == "commit" || self.kind == "merge" {
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

/// Artifacts table schema (search index).
fn artifacts_schema(dim: i32) -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("title", DataType::Utf8, false),
        Field::new("body", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                dim,
            ),
            false,
        ),
    ]))
}

/// Build a RecordBatch from parallel vectors of ids, kinds, titles, bodies, and embeddings.
fn build_batch(
    ids: Vec<String>,
    kinds: Vec<String>,
    titles: Vec<String>,
    bodies: Vec<String>,
    embeddings: Vec<Vec<f32>>,
) -> Result<(RecordBatch, Arc<Schema>)> {
    let dim = embeddings.first().map(|v| v.len()).unwrap_or(384) as i32;
    let flat: Vec<f32> = embeddings.into_iter().flatten().collect();

    let schema = artifacts_schema(dim);
    let id_array = Arc::new(StringArray::from(ids)) as Arc<dyn arrow_array::Array>;
    let kind_array = Arc::new(StringArray::from(kinds)) as Arc<dyn arrow_array::Array>;
    let title_array = Arc::new(StringArray::from(titles)) as Arc<dyn arrow_array::Array>;
    let body_array = Arc::new(StringArray::from(bodies)) as Arc<dyn arrow_array::Array>;
    let values = Arc::new(Float32Array::from(flat));
    let list_field = Arc::new(Field::new("item", DataType::Float32, true));
    let vector_array = Arc::new(arrow_array::FixedSizeListArray::try_new(
        list_field, dim, values, None,
    )?) as Arc<dyn arrow_array::Array>;

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![id_array, kind_array, title_array, body_array, vector_array],
    )?;
    Ok((batch, schema))
}

/// Dedup synthetic string literal nodes by value for embedding.
/// Returns deduped nodes with locations metadata. Graph keeps per-file nodes.
fn dedup_synthetic_consts(symbols: &[crate::graph::Node]) -> (Vec<crate::graph::Node>, Vec<crate::graph::Node>) {
    let mut real_nodes = Vec::new();
    let mut deduped_map: HashMap<String, (crate::graph::Node, Vec<String>)> = HashMap::new();

    for node in symbols {
        let is_synthetic = node
            .metadata
            .get("synthetic")
            .map(|v| v == "true")
            .unwrap_or(false);

        if !is_synthetic {
            real_nodes.push(node.clone());
            continue;
        }

        let value = node
            .metadata
            .get("value")
            .cloned()
            .unwrap_or_else(|| node.id.name.clone());

        let location = format!("{}:{}", node.id.file.display(), node.line_start);

        deduped_map
            .entry(value.clone())
            .and_modify(|(_, locations)| locations.push(location.clone()))
            .or_insert_with(|| {
                let mut n = node.clone();
                n.id.file = PathBuf::from("__strings__");
                n.id.name = value.clone();
                n.id.root = node.id.root.clone();
                (n, vec![location])
            });
    }

    let deduped_strings: Vec<crate::graph::Node> = deduped_map
        .into_values()
        .map(|(mut node, locations)| {
            node.metadata
                .insert("occurrences".to_string(), locations.len().to_string());
            node.metadata
                .insert("locations".to_string(), locations.join(", "));
            node
        })
        .collect();

    (real_nodes, deduped_strings)
}

impl EmbeddingIndex {
    /// Create or open the embedding index with shared model instance.
    pub async fn new(repo_root: &Path) -> Result<Self> {
        let db_path = repo_root.join(".oh").join(".cache").join("embeddings");
        std::fs::create_dir_all(&db_path)?;

        let db = lancedb::connect(db_path.to_str().unwrap())
            .execute()
            .await
            .context("Failed to connect to LanceDB")?;

        let model = new_model()?;
        let cache = EmbedCache::new(repo_root).await?;

        Ok(Self {
            db,
            table_name: "artifacts".to_string(),
            model: Mutex::new(model),
            cache,
        })
    }

    /// Embed texts using the shared model, leveraging the BLAKE3 cache.
    /// Returns vectors in the same order as the input texts.
    async fn embed_with_cache(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        // 1. Hash all texts
        let hashes: Vec<String> = texts.iter().map(|t| content_hash(t)).collect();

        // 2. Batch lookup in cache
        let hits = self.cache.lookup(&hashes).await?;
        let cache_hits = hits.len();

        // 3. Identify misses
        let miss_indices: Vec<usize> = (0..texts.len())
            .filter(|i| !hits.contains_key(&hashes[*i]))
            .collect();

        // 4. Embed only cache misses
        let new_vectors = if !miss_indices.is_empty() {
            let miss_texts: Vec<String> = miss_indices.iter().map(|&i| texts[i].clone()).collect();
            tracing::info!(
                "Embedding {} texts ({} cache hits, {} misses)",
                texts.len(),
                cache_hits,
                miss_indices.len()
            );
            let mut model = self.model.lock().unwrap();
            embed_texts(&mut model, miss_texts)?
        } else {
            tracing::info!(
                "All {} texts resolved from embed cache",
                texts.len()
            );
            vec![]
        };

        // 5. Store new vectors in cache
        let cache_entries: Vec<(String, Vec<f32>)> = miss_indices
            .iter()
            .zip(new_vectors.iter())
            .map(|(&i, v)| (hashes[i].clone(), v.clone()))
            .collect();
        self.cache.store(cache_entries).await?;

        // 6. Merge hits + new vectors in original order
        let mut new_iter = new_vectors.into_iter();
        let result: Vec<Vec<f32>> = (0..texts.len())
            .map(|i| {
                if let Some(v) = hits.get(&hashes[i]) {
                    v.clone()
                } else {
                    new_iter.next().unwrap()
                }
            })
            .collect();

        Ok(result)
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
        let embeddings = self.embed_with_cache(&texts).await?;

        let (batch, schema) = build_batch(ids, kinds, titles, bodies, embeddings)?;

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
        let artifacts = oh::load_oh_artifacts(repo_root)?;

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
        if let Ok(commits) = git::load_commits(repo_root, RECENT_COMMIT_LIMIT) {
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
        }

        // Index PR merge commits for structural context (what shipped)
        let mut seen_merge_shas = std::collections::HashSet::new();
        if let Ok((merge_nodes, _edges)) =
            git::pr_merges::extract_pr_merges(repo_root, Some(PR_MERGE_LIMIT))
        {
            for node in &merge_nodes {
                let merge_sha = node
                    .metadata
                    .get("merge_sha")
                    .cloned()
                    .unwrap_or_default();
                let short = merge_sha.get(..7).unwrap_or(&merge_sha).to_string();
                // Skip if already covered by recent commits
                if seen_merge_shas.contains(&short) {
                    continue;
                }
                seen_merge_shas.insert(short.clone());

                let branch = node.metadata.get("branch_name").cloned().unwrap_or_default();
                let files = node.metadata.get("files_changed").cloned().unwrap_or_default();
                let body = format!(
                    "{}\n\nBranch: {}\nFiles: {}",
                    node.body, branch, files
                );
                let title = node.signature.clone();

                ids.push(format!("merge:{}", short));
                kinds.push("merge".to_string());
                titles.push(title);
                bodies.push(body.clone());
                texts.push(body);
            }
        }

        // Dedup synthetic string literal nodes by value before embedding
        let (real_symbols, deduped_strings) = dedup_synthetic_consts(symbols);

        // Index real code symbols and markdown sections
        for node in real_symbols.iter().chain(deduped_strings.iter()) {
            let kind_str = match &node.id.kind {
                crate::graph::NodeKind::Other(s) => s.clone(),
                k => format!("{}", k),
            };

            // Build searchable text from signature + body + metadata
            let mut text = String::new();
            text.push_str(&node.id.name);
            text.push(' ');
            text.push_str(&node.signature);
            text.push(' ');
            // Include doc comments / body for semantic matching
            // Truncate body to avoid huge embeddings
            let body_snippet = truncate_chars(&node.body, 500);
            text.push_str(body_snippet);
            // Include LSP-enriched metadata (type info, hover docs, resolved types)
            // so that semantic search can find nodes by type-level concepts.
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

        if texts.is_empty() {
            return Ok(0);
        }

        let count = texts.len();
        let deduped_string_count = deduped_strings.len();

        tracing::info!(
            "Embedding pipeline: {} items ({} artifacts, {} commits, {} merges, {} symbols, {} deduped strings)",
            count,
            artifacts.len(),
            std::cmp::min(RECENT_COMMIT_LIMIT, count),
            seen_merge_shas.len(),
            real_symbols.len(),
            deduped_string_count,
        );

        // Compute embeddings via cache
        let embeddings = self.embed_with_cache(&texts).await?;

        let (batch, schema) = build_batch(ids, kinds, titles, bodies, embeddings)?;

        // Drop existing table if it exists, then create new
        let _ = self.db.drop_table(&self.table_name, &[]).await;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        self.db
            .create_table(&self.table_name, Box::new(batches))
            .execute()
            .await
            .context("Failed to create LanceDB table")?;

        // Evict old cache entries if over threshold
        if let Ok(evicted) = self.cache.evict().await {
            if evicted > 0 {
                tracing::info!("Evicted {} stale entries from embed cache", evicted);
            }
        }

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

        // Embed the query using shared model
        let query_embedding = {
            let mut model = self.model.lock().unwrap();
            embed_texts(&mut model, vec![query.to_string()])?
        };

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
    use super::*;

    #[test]
    fn test_truncate_chars_ascii() {
        let s = "a".repeat(600);
        assert_eq!(truncate_chars(&s, 500).len(), 500);
    }

    #[test]
    fn test_truncate_chars_multibyte_boundary() {
        let mut s = "a".repeat(498);
        s.push('\u{2014}'); // em dash, bytes 498..501
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
    fn test_content_hash_deterministic() {
        let h1 = content_hash("application/json");
        let h2 = content_hash("application/json");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 32);
    }

    #[test]
    fn test_content_hash_different_inputs() {
        let h1 = content_hash("application/json");
        let h2 = content_hash("text/html");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_dedup_synthetic_consts() {
        use crate::graph::{Node, NodeId, NodeKind, ExtractionSource};
        use std::collections::BTreeMap;

        let make_synthetic = |file: &str, value: &str, line: usize| -> Node {
            let mut metadata = BTreeMap::new();
            metadata.insert("value".to_string(), value.to_string());
            metadata.insert("synthetic".to_string(), "true".to_string());
            Node {
                id: NodeId {
                    root: "test".to_string(),
                    file: PathBuf::from(file),
                    name: value.to_string(),
                    kind: NodeKind::Const,
                },
                language: "rust".to_string(),
                line_start: line,
                line_end: line,
                signature: format!("\"{}\"", value),
                body: String::new(),
                metadata,
                source: ExtractionSource::TreeSitter,
            }
        };

        let make_real = |name: &str| -> Node {
            Node {
                id: NodeId {
                    root: "test".to_string(),
                    file: PathBuf::from("src/lib.rs"),
                    name: name.to_string(),
                    kind: NodeKind::Function,
                },
                language: "rust".to_string(),
                line_start: 1,
                line_end: 10,
                signature: format!("fn {}()", name),
                body: "// body".to_string(),
                metadata: BTreeMap::new(),
                source: ExtractionSource::TreeSitter,
            }
        };

        let nodes = vec![
            make_real("foo"),
            make_synthetic("src/a.rs", "application/json", 10),
            make_synthetic("src/b.rs", "application/json", 20),
            make_synthetic("src/c.rs", "text/html", 30),
            make_real("bar"),
        ];

        let (real, deduped) = dedup_synthetic_consts(&nodes);

        assert_eq!(real.len(), 2, "should keep 2 real nodes");
        assert_eq!(deduped.len(), 2, "should dedup to 2 unique string values");

        // Find the application/json deduped node
        let json_node = deduped.iter().find(|n| n.id.name == "application/json").unwrap();
        assert_eq!(json_node.metadata.get("occurrences").unwrap(), "2");
        assert!(json_node.metadata.get("locations").unwrap().contains("src/a.rs:10"));
        assert!(json_node.metadata.get("locations").unwrap().contains("src/b.rs:20"));
        assert_eq!(json_node.id.file, PathBuf::from("__strings__"));
    }
}
