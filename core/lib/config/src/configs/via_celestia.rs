use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct ViaCelestiaConfig {
    /// Celestia url.
    pub api_node_url: String,

    /// Celestia blob limit
    pub blob_size_limit: usize,
}

impl ViaCelestiaConfig {
    /// Creates a config object suitable for use in unit tests.
    pub fn for_tests() -> ViaCelestiaConfig {
        Self {
            blob_size_limit: 1973786,
            api_node_url: "".into(),
        }
    }
}
