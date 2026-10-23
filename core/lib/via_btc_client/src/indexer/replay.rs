//! Stable semantic content excludes inclusion coordinates and signatures that may change on re-mining.

use serde_json::{json, Value};

use crate::types::FullInscriptionMessage;

/// Each domain persists this content with its own evidence identity and inclusion records.
/// Idea: TigerBeetle's immutable replay comparison, pinned at
/// <https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig#L3971-L4033>.
pub fn semantic_payload(message: &FullInscriptionMessage) -> serde_json::Result<Value> {
    use FullInscriptionMessage as M;
    let (kind, input) = match message {
        M::L1BatchDAReference(m) => ("batch", serde_json::to_value(&m.input)?),
        M::ProofDAReference(m) => ("proof", serde_json::to_value(&m.input)?),
        M::ValidatorAttestation(m) => ("vote", serde_json::to_value(&m.input)?),
        M::SystemBootstrapping(m) => ("bootstrap", serde_json::to_value(&m.input)?),
        M::L1ToL2Message(m) => (
            "deposit",
            json!({ "amount": m.amount.to_sat(), "input": m.input, "outputs": m.tx_outputs }),
        ),
        M::SystemContractUpgradeProposal(m) => {
            ("upgrade_proposal", serde_json::to_value(&m.input)?)
        }
        M::BridgeWithdrawal(m) => (
            "withdrawal",
            json!({
                "version": m.input.version.clone() as u8,
                "inputs": m.input.transaction.input.iter().map(|input| json!({"previous_output":input.previous_output,"sequence":input.sequence,"script_sig":input.script_sig})).collect::<Vec<_>>(),
                "outputs": m.input.transaction.output,
                "prevouts": m.input.prevouts,
                "bridge_script_pubkey": m.input.bridge_script_pubkey,
                "lock_time": m.input.transaction.lock_time,
                "transaction_version": m.input.transaction.version,
            }),
        ),
        M::UpdateBridgeProposal(m) => ("bridge_proposal", serde_json::to_value(&m.input)?),
        M::UpdateGovernance(m) => ("governance", serde_json::to_value(&m.input)?),
        M::UpdateSequencer(m) => ("sequencer", serde_json::to_value(&m.input)?),
        M::SystemContractUpgrade(m) => ("upgrade", serde_json::to_value(&m.input)?),
        M::UpdateBridge(m) => ("bridge", serde_json::to_value(&m.input)?),
    };
    let common = message.common();
    Ok(json!({
        "kind": kind,
        "public_key": common.encoded_public_key.as_bytes(),
        "sender": common.p2wpkh_address.as_ref().map(ToString::to_string),
        "input": input,
    }))
}

/// Object fields compare lexically, recursively, without hashing or ignoring unknown fields.
/// A field path is diagnostic only; the entire semantic value must agree.
pub fn compare_payload(previous: &Value, incoming: &Value) -> Result<(), String> {
    match (previous, incoming) {
        (Value::Object(previous), Value::Object(incoming)) => {
            let keys: std::collections::BTreeSet<_> =
                previous.keys().chain(incoming.keys()).collect();
            for key in keys {
                match (previous.get(key), incoming.get(key)) {
                    (Some(old), Some(new)) => compare_payload(old, new).map_err(|field| {
                        if field.is_empty() {
                            key.clone()
                        } else {
                            format!("{key}_{field}")
                        }
                    })?,
                    _ => return Err(key.clone()),
                }
            }
            Ok(())
        }
        _ => via_reorg::replay::first_difference([("", Some(previous), Some(incoming))])
            .map_or(Ok(()), |field| Err(field.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_checks_full_payload_and_reports_first_immutable_field() {
        let original = json!({"kind":"deposit", "input":{"amount":5,"receiver":"a"}});
        assert_eq!(compare_payload(&original, &original), Ok(()));
        assert_eq!(
            compare_payload(
                &original,
                &json!({"kind":"proof", "input":{"amount":6,"receiver":"b"}})
            ),
            Err("input_amount".into())
        );
        assert_eq!(
            compare_payload(
                &original,
                &json!({"kind":"deposit", "input":{"amount":5,"receiver":"b"}})
            ),
            Err("input_receiver".into())
        );
        assert!(compare_payload(&original, &json!({"kind":"deposit"})).is_err());
        assert!(compare_payload(
            &original,
            &json!({"kind":"deposit", "input":{"amount":5,"receiver":"a"},"extra":0})
        )
        .is_err());
    }
}
