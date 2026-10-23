use std::collections::BTreeMap;

use anyhow::Context;
use jsonrpsee::core::server::BoundedWriter;
use serde::Serialize;
use via_btc_client::traits::Serializable;

use crate::{
    auth::{Binding, Envelope, MAX_BODY},
    types::{SessionOperation, SigningSession, SigningSessionResponse},
    utils::{encode_nonce, encode_signature},
};

/// Supported empty-body envelope size, including delimiters, binding and signature.
/// Admission reserves this much for any accepted request's worst-width reply.
pub(crate) const MAX_ENVELOPE_OVERHEAD: usize = 65_536;

// Adopt the workspace-pinned public writer; count the final JSON, including its wrapper.
// https://github.com/paritytech/jsonrpsee/blob/c6344f9ee57229ca182956c408b0f84d49ba56ea/core/src/server/helpers.rs
pub(crate) fn bounded_json<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> serde_json::Result<Vec<u8>> {
    let mut writer = BoundedWriter::new(limit);
    serde_json::to_writer(&mut &mut writer, value)?;
    Ok(writer.into_bytes())
}

fn empty_snapshot(session: &SigningSession, required_signers: usize) -> SigningSessionResponse {
    SigningSessionResponse {
        round_id: session.round_id,
        content_hash: session.content_hash,
        authorized_content: session.authorized_content.clone(),
        nonces: BTreeMap::new(),
        signatures: BTreeMap::new(),
        session_op: session.session_op.as_ref().map(Serializable::to_bytes).unwrap_or_default(),
        required_signers,
        received_nonces: BTreeMap::new(),
        received_partial_signatures: BTreeMap::new(),
        created_at: session.created_at,
    }
}

pub(crate) fn snapshot(
    session: &SigningSession,
    required_signers: usize,
) -> anyhow::Result<SigningSessionResponse> {
    let mut response = empty_snapshot(session, required_signers);
    for (input, values) in &session.received_nonces {
        response.nonces.insert(
            *input,
            values
                .iter()
                .map(|(signer, nonce)| Ok((*signer, encode_nonce(*signer, nonce.clone())?.nonce)))
                .collect::<anyhow::Result<_>>()?,
        );
        response.received_nonces.insert(*input, values.len());
    }
    for (input, values) in &session.received_sigs {
        response.signatures.insert(
            *input,
            values
                .iter()
                .map(|(signer, signature)| {
                    Ok((*signer, encode_signature(*signer, *signature)?.signature))
                })
                .collect::<anyhow::Result<_>>()?,
        );
        response.received_partial_signatures.insert(*input, values.len());
    }
    Ok(response)
}

fn index_digits(count: usize) -> Option<usize> {
    if count == 0 {
        return Some(0);
    }
    let (mut result, mut start, mut width) = (1usize, 1usize, 1usize);
    while start < count {
        let end = start.saturating_mul(10).min(count);
        result = result.checked_add((end - start).checked_mul(width)?)?;
        start = end;
        width += 1;
    }
    Some(result)
}

fn matrix_size(inputs: usize, signers: usize, value_width: usize) -> Option<usize> {
    let row = 5usize
        .checked_add(signers.checked_mul(value_width.checked_add(6)?)?)?
        .checked_add(index_digits(signers)?)?;
    1usize.checked_add(index_digits(inputs)?)?.checked_add(inputs.checked_mul(row)?)
}

fn counts_size(inputs: usize, signers: usize) -> Option<usize> {
    let width = signers.to_string().len();
    1usize.checked_add(index_digits(inputs)?)?.checked_add(inputs.checked_mul(4 + width)?)
}

// Own Via schema arithmetic, using sBTC's signed-wrapper budgeting pattern. The empty
// snapshot above is the actual serving schema; only four future maps are calculated.
// Public nonce/share encodings remain 88/44 characters, and tests use real values.
// https://github.com/stacks-sbtc/sbtc/blob/fb469a32a48f47d2a6e46e13e72c944ab2c96755/signer/src/bitcoin/packaging.rs
fn future_inner_bytes(session: &SigningSession, signers: usize) -> anyhow::Result<usize> {
    let empty = bounded_json(&empty_snapshot(session, signers), MAX_BODY)?.len();
    let Some(SessionOperation::Withdrawal(_, messages, _)) = &session.session_op else {
        return Ok(empty);
    };
    let inputs = messages.len();
    anyhow::ensure!(inputs > 0 && signers > 0, "Empty signing dimensions");
    let size = || {
        let maps = matrix_size(inputs, signers, 88)?
            .checked_add(matrix_size(inputs, signers, 44)?)?
            .checked_add(2usize.checked_mul(counts_size(inputs, signers)?)?)?;
        empty.checked_add(maps.checked_sub(8)?)
    };
    size().context("Signing transcript size overflow")
}

pub(crate) fn check_round_size(session: &SigningSession, signers: usize) -> anyhow::Result<()> {
    let inner = future_inner_bytes(session, signers)?;
    // Each numeric byte needs at most three digits and one comma. H includes [].
    let final_bound = inner
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_add(MAX_ENVELOPE_OVERHEAD))
        .context("Signing envelope size overflow")?;
    anyhow::ensure!(
        final_bound <= MAX_BODY,
        "Full signing envelope bound {final_bound} exceeds {MAX_BODY}"
    );
    Ok(())
}

pub(crate) fn response_binding(
    request: &Binding,
    coordinator: &str,
    status: u16,
    timestamp: i64,
) -> Binding {
    let mut binding = request.clone();
    binding.audience = request.principal.clone();
    binding.principal = coordinator.to_owned();
    binding.status = status;
    binding.timestamp = timestamp;
    binding
}

pub(crate) fn check_metadata(binding: &Binding, coordinator: &str) -> anyhow::Result<()> {
    for binding in [binding.clone(), response_binding(binding, coordinator, u16::MAX, i64::MIN)] {
        let envelope = Envelope { binding, body: Vec::new(), signature: "A".repeat(88) };
        bounded_json(&envelope, MAX_ENVELOPE_OVERHEAD)
            .context("Unsupported authentication envelope metadata")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
