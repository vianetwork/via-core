use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct CelestiaConfig {
    pub api_node_url: String,
    pub api_private_key: String,
}

impl CelestiaConfig {
    /// Creates a config object suitable for use in unit tests.
    pub fn for_tests() -> CelestiaConfig {
        Self {
            api_node_url: "todo".into(),
            api_private_key: "todo".into(),
        }
    }
}
