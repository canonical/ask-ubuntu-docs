use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

static TOML_SRC: &str = include_str!("../product-prompts.toml");

#[derive(Deserialize)]
struct ProductEntry {
    prompt: String,
    /// URL prefix for RAG score boosting. Empty string means no dedicated docs yet.
    docs: String,
}

fn map() -> &'static HashMap<String, ProductEntry> {
    static MAP: OnceLock<HashMap<String, ProductEntry>> = OnceLock::new();
    MAP.get_or_init(|| toml::from_str(TOML_SRC).expect("product-prompts.toml is malformed"))
}

/// System-prompt addition for the given product label (e.g. `"Desktop"`).
/// Returns an empty string if the product is not found.
pub fn get_prompt(product: &str) -> &'static str {
    map().get(product).map(|e| e.prompt.as_str()).unwrap_or("")
}

/// RAG source-URL prefix for the given product, used to boost relevant chunks.
/// Returns `None` if the product has no dedicated documentation indexed yet.
pub fn source_prefix(product: &str) -> Option<&'static str> {
    map()
        .get(product)
        .map(|e| e.docs.as_str())
        .filter(|s| !s.is_empty())
}
