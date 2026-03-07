use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{Float32Array, RecordBatch, RecordBatchIterator, StringArray};
use arrow_schema::{DataType, Field, Schema};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use lancedb::query::{ExecutableQuery, QueryBase};

use crate::git;
use crate::oh;

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

        let db = lancedb::connect(db_path.to_str().unwrap())
            .execute()
            .await
            .context("Failed to connect to LanceDB")?;

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

        // Also index all git commits
        if let Ok(commits) = git::load_commits(repo_root, usize::MAX) {
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

        // Also index code symbols and markdown sections from the graph
        for node in symbols {
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
            let body_snippet = if node.body.len() > 500 {
                &node.body[..500]
            } else {
                &node.body
            };
            text.push_str(body_snippet);

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

        // Compute embeddings
        let mut model = new_model()?;

        let embeddings = embed_texts(&mut model, texts)?;
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

        // Drop existing table if it exists, then create new
        let _ = self.db.drop_table(&self.table_name, &[]).await;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        self.db
            .create_table(&self.table_name, Box::new(batches))
            .execute()
            .await
            .context("Failed to create LanceDB table")?;

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
        let mut model = new_model()?;
        let query_embedding = embed_texts(&mut model, vec![query.to_string()])?;

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
