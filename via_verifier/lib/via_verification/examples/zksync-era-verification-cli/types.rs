use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L1BatchRangeJson {
    pub result: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JSONL2SyncRPCResponse {
    pub result: L2SyncDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L2SyncDetails {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JSONL2RPCResponse {
    pub result: L1BatchJson,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L1BatchJson {
    #[serde(rename = "commitTxHash")]
    pub commit_tx_hash: String,
    #[serde(rename = "proveTxHash")]
    pub prove_tx_hash: Option<String>,
}
