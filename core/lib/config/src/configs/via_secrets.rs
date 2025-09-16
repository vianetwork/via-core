use std::fmt::Debug;

use bitcoincore_rpc::Auth;
use serde::Deserialize;
use zksync_basic_types::url::SensitiveUrl;

use super::Secrets;

#[derive(Clone, Deserialize, PartialEq)]
pub struct ViaL1Secrets {
    /// URL of the Bitcoin node RPC.
    pub rpc_url: SensitiveUrl,

    /// Username for the Bitcoin node RPC.
    pub rpc_user: String,

    /// Password for the Bitcoin node RPC.
    pub rpc_password: String,
}

impl ViaL1Secrets {
    pub fn auth_node(&self) -> Auth {
        Auth::UserPass(self.rpc_user.clone(), self.rpc_password.clone())
    }
}

impl Debug for ViaL1Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViaL1Secrets")
            .field("rpc_url", &self.rpc_url)
            .field("rpc_user", &"********")
            .field("rpc_password", &"********")
            .finish()
    }
}

#[derive(Clone, Deserialize, PartialEq)]
pub struct ViaDASecrets {
    /// URL for the Celestia node RPC.
    pub api_node_url: SensitiveUrl,

    /// AUTH token for the Celestia node RPC.
    pub auth_token: String,
}

impl Debug for ViaDASecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViaDASecrets")
            .field("api_node_url", &self.api_node_url)
            .field("auth_token", &"********")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViaSecrets {
    pub base_secrets: Secrets,
    pub via_l1: Option<ViaL1Secrets>,
    pub via_da: Option<ViaDASecrets>,
}

impl Default for ViaSecrets {
    fn default() -> Self {
        Self {
            base_secrets: Secrets {
                consensus: None,
                database: None,
                l1: None,
                data_availability: None,
            },
            via_l1: None,
            via_da: None,
        }
    }
}
