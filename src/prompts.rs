use std::collections::HashMap;
use std::sync::OnceLock;

static TOML_SRC: &str = include_str!("../prompts.toml");

/// Return the system-prompt addition for the given product label (e.g. `"Desktop"`).
/// Returns an empty string if the product is not found.
pub fn get(product: &str) -> &'static str {
    static MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        toml::from_str(TOML_SRC).expect("prompts.toml is malformed")
    });
    map.get(product).map(String::as_str).unwrap_or("")
}
