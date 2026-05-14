use anyhow::{Context, Result};
use arrow_array::StringArray;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use futures_util::TryStreamExt;
use lancedb::index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery, QueryBase, Select};

// Number of documentation chunks to retrieve per user query
pub const TOP_K: usize = 12;

fn index_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("UBUNTU_HELP_INDEX_PATH") {
        return std::path::PathBuf::from(p);
    }
    std::path::PathBuf::from(concat!(env!("OUT_DIR"), "/index.lance"))
}

pub struct RagStore {
    pub table: lancedb::Table,
    embedder: TextEmbedding,
}

impl RagStore {
    pub async fn load() -> Result<Self> {
        let path = index_path();
        let path_str = path
            .to_str()
            .context("LanceDB index path is not valid UTF-8")?;
        let db = lancedb::connect(path_str).execute().await?;
        let table = db.open_table("docs").execute().await?;
        let embedder = tokio::task::spawn_blocking(|| {
            TextEmbedding::try_new(InitOptions::new(EmbeddingModel::BGESmallENV15))
                .context("failed to initialise embedding model")
        })
        .await
        .context("spawn_blocking panicked")??;
        Ok(Self { table, embedder })
    }

    /// Embeds `query` synchronously (CPU-bound). Call from `block_in_place` in async context.
    pub fn embed(&mut self, query: &str) -> Result<Vec<f32>> {
        self.embedder
            .embed(vec![query.to_string()], None)
            .context("failed to embed query")?
            .into_iter()
            .next()
            .context("embedder returned no vector for query")
    }

    /// Hybrid BM25 + vector search via LanceDB. Returns up to `top_k` (source, text) pairs.
    pub async fn search_with_vec(
        table: &lancedb::Table,
        query_text: &str,
        query_vec: Vec<f32>,
        top_k: usize,
    ) -> Result<Vec<(String, String)>> {
        // Empty table means a debug build with no indexed docs — return nothing gracefully.
        if table.count_rows(None).await.unwrap_or(0) == 0 {
            return Ok(vec![]);
        }

        let mut stream = table
            .query()
            .nearest_to(query_vec.as_slice())?
            .full_text_search(FullTextSearchQuery::new(query_text.to_string()))
            .limit(top_k)
            .select(Select::columns(&["source", "text"]))
            .execute()
            .await?;

        let mut results = Vec::new();
        while let Some(batch) = stream.try_next().await? {
            let sources = batch
                .column_by_name("source")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let texts = batch
                .column_by_name("text")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            for i in 0..batch.num_rows() {
                results.push((sources.value(i).to_string(), texts.value(i).to_string()));
            }
        }

        Ok(results)
    }
}
