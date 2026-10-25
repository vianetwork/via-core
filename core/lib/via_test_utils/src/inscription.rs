use bitcoin::{script::PushBytesBuf, secp256k1::XOnlyPublicKey, ScriptBuf};

/// Builds the inscription script shared by the Bitcoin test fixtures, with the fields in the given order.
/// The caller builds the transaction inputs and witness.
pub fn inscription_script(internal: XOnlyPublicKey, kind: &[u8], fields: &[Vec<u8>]) -> ScriptBuf {
    let push = |bytes: &[u8]| PushBytesBuf::try_from(bytes.to_vec()).unwrap();
    let mut script = bitcoin::script::Builder::new()
        .push_slice(internal.serialize())
        .push_opcode(bitcoin::opcodes::all::OP_CHECKSIG)
        .push_opcode(bitcoin::opcodes::OP_FALSE)
        .push_opcode(bitcoin::opcodes::all::OP_IF)
        .push_slice(b"via_inscription_protocol")
        .push_slice(push(kind));
    for field in fields {
        script = script.push_slice(push(field));
    }
    script
        .push_opcode(bitcoin::opcodes::all::OP_ENDIF)
        .into_script()
}

pub fn unsigned_proof(
    pubdata: via_btc_client::indexer::replay::Included<
        via_btc_client::types::L1BatchDAReferenceInput,
    >,
    origin: via_btc_client::indexer::replay::MessageOrigin,
    reference: via_btc_client::types::ProofDAReferenceInput,
) -> via_btc_client::indexer::replay::Included<via_btc_client::indexer::replay::ProofInput> {
    use via_btc_client::indexer::replay::{Included, Payload, ProofInput};
    Included {
        origin,
        payload: Payload {
            input: ProofInput { proof: reference, pubdata },
            public_key: Vec::new(),
            signature: Vec::new(),
            sender_script: None,
        },
    }
}
