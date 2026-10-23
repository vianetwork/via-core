// Frozen legacy HTTP schema. Never infer its transport fields from new DTO.
// jscpd:ignore-start Immutable old snapshot oracle for rolling-compatibility tests.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SigningSessionResponse {
    pub round_id: [u8; 32],
    pub content_hash: [u8; 32],
    pub authorized_content: Vec<u8>,
    pub nonces: BTreeMap<usize, BTreeMap<usize, String>>,
    pub signatures: BTreeMap<usize, BTreeMap<usize, String>>,
    pub session_op: Vec<u8>,
    pub required_signers: usize,
    pub received_nonces: BTreeMap<usize, usize>,
    pub received_partial_signatures: BTreeMap<usize, usize>,
    pub created_at: u64,
}

// jscpd:ignore-end
