use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

// The binary index produced by build.rs is embedded directly into the executable.
// This means no external files or servers are needed at runtime to do vector search.
static INDEX_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/index.bin"));

// Number of documentation chunks to retrieve per user query
pub const TOP_K: usize = 12;
// Cosine similarity multiplier applied to chunks from the selected product's docs.
// Boosts on-topic results without completely excluding other sources.
const PRODUCT_BOOST: f32 = 1.1;

struct IndexEntry {
    source: String,
    text: String,
    // Pre-computed embedding vector for this chunk (384 floats for BGE-small)
    vector: Vec<f32>,
}

// Holds the in-memory document index and the embedding model used to embed queries
pub struct RagStore {
    entries: Vec<IndexEntry>,
    embedder: TextEmbedding,
}

impl RagStore {
    // Parses the embedded index bytes and initialises the embedding model.
    // This is a blocking operation; callers in async context should use block_in_place.
    pub fn load() -> Result<Self> {
        let (_, entries) =
            parse_index(INDEX_BYTES).context("failed to parse embedded RAG index")?;
        let embedder =
            TextEmbedding::try_new(InitOptions::new(EmbeddingModel::BGESmallENV15))
                .context("failed to initialise embedding model")?;
        Ok(Self { entries, embedder })
    }

    /// Embeds `query` and returns the top-k most similar (source, text) pairs.
    /// If `product_prefix` is `Some(prefix)`, chunks whose source URL contains
    /// that prefix have their cosine similarity multiplied by `PRODUCT_BOOST`
    /// before ranking, so on-topic results surface first without hard-filtering.
    pub fn search(&mut self, query: &str, top_k: usize, product_prefix: Option<&str>) -> Result<Vec<(String, String)>> {
        let query_vec = self
            .embedder
            .embed(vec![query.to_string()], None)
            .context("failed to embed query")?
            .into_iter()
            .next()
            .context("embedder returned no vector for query")?;

        // Score every entry; apply product boost where applicable
        let mut scored: Vec<(f32, usize)> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let mut score = cosine_similarity(&query_vec, &e.vector);
                if let Some(prefix) = product_prefix {
                    if e.source.contains(prefix) {
                        score *= PRODUCT_BOOST;
                    }
                }
                (score, i)
            })
            .collect();

        // Sort descending: highest similarity first
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        Ok(scored
            .into_iter()
            .take(top_k)
            .map(|(_, i)| (self.entries[i].source.clone(), self.entries[i].text.clone()))
            .collect())
    }
}

// Returns the cosine similarity between two equal-length vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { 0.0 } else { dot / (norm_a * norm_b) }
}

// Parses the binary index format written by build.rs.
// Format: dim(u64le) + n_chunks(u64le) + [(src_len, src, txt_len, txt, dim×f32), …]
fn parse_index(mut data: &[u8]) -> Result<(usize, Vec<IndexEntry>)> {
    let dim = read_u64(&mut data)? as usize;
    let n = read_u64(&mut data)? as usize;
    let mut entries = Vec::with_capacity(n);
    for _ in 0..n {
        let source = read_string(&mut data)?;
        let text = read_string(&mut data)?;
        let mut vector = Vec::with_capacity(dim);
        for _ in 0..dim {
            let bytes = data.get(..4).context("index truncated while reading vector")?;
            vector.push(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
            data = &data[4..];
        }
        entries.push(IndexEntry { source, text, vector });
    }
    Ok((dim, entries))
}

fn read_u64(data: &mut &[u8]) -> Result<u64> {
    let bytes = data.get(..8).context("index truncated while reading u64")?;
    let val = u64::from_le_bytes(bytes.try_into().unwrap());
    *data = &data[8..];
    Ok(val)
}

fn read_string(data: &mut &[u8]) -> Result<String> {
    let len = read_u64(data)? as usize;
    let bytes = data.get(..len).context("index truncated while reading string")?;
    let s = std::str::from_utf8(bytes).context("invalid UTF-8 in index")?.to_string();
    *data = &data[len..];
    Ok(s)
}
