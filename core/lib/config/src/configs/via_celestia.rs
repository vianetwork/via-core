use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct ViaCelestiaConfig {
    pub api_node_url: String,
    pub auth_token: String,
}

impl ViaCelestiaConfig {
    /// Creates a config object suitable for use in unit tests.
    pub fn for_tests() -> ViaCelestiaConfig {
        Self {
            api_node_url: "ws://localhost:26658".into(),
            auth_token: "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJBbGxvdyI6WyJwdWJsaWMiLCJyZWFkIiwid3JpdGUiLCJhZG1pbiJdfQ.ut1X4u9XG5cbV0yaRAKfGp9xWVrz3NoEPGGRch13dFU".into(),
        }
    }
}
