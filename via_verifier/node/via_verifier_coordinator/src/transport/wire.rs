use std::collections::BTreeMap;

use axum::{extract::Query, http::Uri};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_with::{base64::Base64, serde_as};

use super::bounded_json;
use crate::{
    auth::{digest, Binding, Envelope, MAX_BODY},
    types::{AdmittedContent, SigningSessionResponse},
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportVersion {
    #[default]
    Legacy,
    Compact,
}

impl TransportVersion {
    // Without offer, oldest supported writer answers. Input decoding does not depend on it.
    // Unlike unsigned Accept header, Via's offer sits in signed target.
    // Writer selection follows Kubernetes media negotiation: https://github.com/kubernetes/apiserver/blob/62c14eb1e5291f23d56d4610bba00c41b6a3b771/pkg/endpoints/handlers/negotiation/negotiate.go
    pub(crate) fn negotiate(uri: &Uri, writes_enabled: bool) -> anyhow::Result<Self> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Offer {
            transport_version: Option<u8>,
        }
        // Axum's typed Query extractor, unchanged: https://github.com/tokio-rs/axum/blob/9983bc1da460becd3a0f08c513d610411e84dd43/axum/src/extract/query.rs
        let Query(offer) = Query::<Offer>::try_from_uri(uri)?;
        match offer.transport_version {
            None => Ok(Self::Legacy),
            Some(2) if writes_enabled => Ok(Self::Compact),
            Some(2) => Ok(Self::Legacy),
            Some(version) => anyhow::bail!("Unsupported transport version {version}"),
        }
    }
}

// HTTP-only mirror of Binding. Serde's remote derive leaves Binding's bincode serializer untouched.
// via-withdrawal-http-v1 authentication domain stays unchanged too.
// Base64 uses serde_with's adapter unchanged, fixed-array length check included: https://github.com/jonasbb/serde_with/blob/8c59954436b139b238dc23830946f04a3e331862/serde_with/src/base64.rs
#[serde_as]
#[derive(Serialize, Deserialize)]
#[serde(remote = "Binding", deny_unknown_fields)]
struct CompactBinding {
    version: u8,
    sequencer_version: String,
    principal: String,
    audience: String,
    method: String,
    target: String,
    #[serde_as(as = "Base64")]
    round: [u8; 32],
    #[serde_as(as = "Base64")]
    content: [u8; 32],
    #[serde_as(as = "Base64")]
    challenge: [u8; 32],
    timestamp: i64,
    status: u16,
}

#[serde_as]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactEnvelope {
    #[serde(with = "CompactBinding")]
    binding: Binding,
    #[serde_as(as = "Base64")]
    body: Vec<u8>,
    signature: String,
}

// Every field is borrowed: duplicate or unknown envelope fields fail before typed decoding.
// No untagged Content buffer is allocated for large legacy array.
// Borrowed RawValue pattern, nearly verbatim from serde_json: https://github.com/serde-rs/json/blob/0903de449c52c1b4a2271e909b7afb18909dc379/src/raw.rs
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeProbe<'a> {
    #[serde(borrow, rename = "binding")]
    _binding: &'a RawValue,
    #[serde(borrow)]
    body: &'a RawValue,
    #[serde(borrow, rename = "signature")]
    _signature: &'a RawValue,
}

pub(crate) fn decode_envelope(bytes: &[u8]) -> anyhow::Result<(Envelope, TransportVersion)> {
    anyhow::ensure!(bytes.len() <= MAX_BODY, "Oversized authentication body");
    let probe: EnvelopeProbe<'_> = serde_json::from_slice(bytes)?;
    match probe.body.get().as_bytes().first() {
        Some(b'[') => Ok((serde_json::from_slice(bytes)?, TransportVersion::Legacy)),
        Some(b'"') => {
            let compact: CompactEnvelope = serde_json::from_slice(bytes)?;
            Ok((
                Envelope {
                    binding: compact.binding,
                    body: compact.body,
                    signature: compact.signature,
                },
                TransportVersion::Compact,
            ))
        }
        _ => anyhow::bail!("Expected numeric-array or Base64 envelope body"),
    }
}

pub(crate) fn encode_envelope(
    envelope: Envelope,
    version: TransportVersion,
    limit: usize,
) -> serde_json::Result<Vec<u8>> {
    match version {
        TransportVersion::Legacy => bounded_json(&envelope, limit),
        TransportVersion::Compact => bounded_json(
            &CompactEnvelope {
                binding: envelope.binding,
                body: envelope.body,
                signature: envelope.signature,
            },
            limit,
        ),
    }
}

#[serde_as]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactSnapshot {
    transport_version: u8,
    #[serde_as(as = "Base64")]
    round_id: [u8; 32],
    #[serde_as(as = "Base64")]
    content_hash: [u8; 32],
    #[serde_as(as = "Base64")]
    authorized_content: Vec<u8>,
    nonces: BTreeMap<usize, BTreeMap<usize, String>>,
    signatures: BTreeMap<usize, BTreeMap<usize, String>>,
    required_signers: usize,
    received_nonces: BTreeMap<usize, usize>,
    received_partial_signatures: BTreeMap<usize, usize>,
    created_at: u64,
}

pub(crate) fn encode_snapshot(
    response: SigningSessionResponse,
    version: TransportVersion,
) -> serde_json::Result<Vec<u8>> {
    match version {
        TransportVersion::Legacy => bounded_json(&response, MAX_BODY),
        TransportVersion::Compact => bounded_json(
            &CompactSnapshot {
                transport_version: 2,
                round_id: response.round_id,
                content_hash: response.content_hash,
                authorized_content: response.authorized_content,
                nonces: response.nonces,
                signatures: response.signatures,
                required_signers: response.required_signers,
                received_nonces: response.received_nonces,
                received_partial_signatures: response.received_partial_signatures,
                created_at: response.created_at,
            },
            MAX_BODY,
        ),
    }
}

/// Call only after authenticating the exact body.
/// Both formats normalize to the one admitted proposal. The legacy duplicate is never authority.
pub(crate) fn decode_snapshot(
    bytes: &[u8],
    version: TransportVersion,
) -> anyhow::Result<SigningSessionResponse> {
    let mut response = match version {
        TransportVersion::Legacy => serde_json::from_slice::<SigningSessionResponse>(bytes)?,
        TransportVersion::Compact => {
            let compact: CompactSnapshot = serde_json::from_slice(bytes)?;
            anyhow::ensure!(compact.transport_version == 2, "Unsupported compact snapshot version");
            SigningSessionResponse {
                transport: version,
                round_id: compact.round_id,
                content_hash: compact.content_hash,
                authorized_content: compact.authorized_content,
                session_op: Vec::new(),
                nonces: compact.nonces,
                signatures: compact.signatures,
                required_signers: compact.required_signers,
                received_nonces: compact.received_nonces,
                received_partial_signatures: compact.received_partial_signatures,
                created_at: compact.created_at,
            }
        }
    };
    response.transport = version;
    if response.authorized_content.is_empty() {
        anyhow::ensure!(
            response.round_id == [0; 32]
                && response.content_hash == [0; 32]
                && response.session_op.is_empty(),
            "Invalid empty signing snapshot"
        );
        return Ok(response);
    }
    anyhow::ensure!(
        response.round_id != [0; 32]
            && digest(&response.authorized_content) == response.content_hash,
        "Invalid proposal commitment"
    );
    let admitted: AdmittedContent = bincode::deserialize(&response.authorized_content)?;
    if version == TransportVersion::Legacy {
        anyhow::ensure!(response.session_op == admitted.proposal, "Conflicting legacy proposal");
    }
    response.session_op = admitted.proposal;
    Ok(response)
}
