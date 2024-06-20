use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct CelestiaConfig {
    pub api_node_url: String,
    pub api_private_key: String,
    pub auth_token: String,
}

impl CelestiaConfig {
    /// Creates a config object suitable for use in unit tests.
    pub fn for_tests() -> CelestiaConfig {
        Self {
            api_node_url: "ws://localhost:26658".into(),
            api_private_key: "0xf55baf7c0e4e33b1d78fbf52f069c426bc36cff1aceb9bc8f45d14c07f034d73"
                .into(),
            auth_token: "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJBbGxvdyI6WyJwdWJsaWMiLCJyZWFkIiwid3JpdGUiLCJhZG1pbiJdfQ.ut1X4u9XG5cbV0yaRAKfGp9xWVrz3NoEPGGRch13dFU".into(),
        }
    }
}
