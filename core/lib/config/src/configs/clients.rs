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
            api_node_url: "localhost:12345".into(),
            api_private_key: "0xf55baf7c0e4e33b1d78fbf52f069c426bc36cff1aceb9bc8f45d14c07f034d73"
                .into(),
        }
    }
}
