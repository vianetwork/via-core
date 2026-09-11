use bitcoin::{Address, ScriptBuf, Transaction};
use via_btc_ingestion::{MessageLocation, RejectionCode};
use zksync_types::via_wallet::SystemWallets;

use super::parser::{CarrierMetadata, CarrierParse, MessageParser};
use crate::types::FullInscriptionMessage;

/// A decoded Via message together with its exact carrier and signer identity.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionedMessage {
    pub message: FullInscriptionMessage,
    pub location: MessageLocation,
    pub signer_script: Option<ScriptBuf>,
}

/// A recognizable Via carrier whose payload cannot be decoded safely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionedMalformed {
    pub location: MessageLocation,
    pub signer_script: Option<ScriptBuf>,
    pub code: RejectionCode,
    pub detail: String,
}

/// A Via protocol marker that this parser version does not implement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionedUnsupported {
    pub location: MessageLocation,
    pub signer_script: Option<ScriptBuf>,
    pub kind: Vec<u8>,
}

/// Deterministic classification of one transaction input or output.
#[derive(Clone, Debug, PartialEq)]
pub enum PositionedParseOutcome {
    Valid(Box<PositionedMessage>),
    Malformed(PositionedMalformed),
    Unsupported(PositionedUnsupported),
    ContextRequired {
        location: MessageLocation,
        signer_script: Option<ScriptBuf>,
    },
    Irrelevant {
        location: MessageLocation,
    },
}

/// Parses every transaction carrier in input-first, output-second order.
#[derive(Clone, Debug)]
pub struct PositionedMessageParser {
    parser: MessageParser,
}

impl PositionedMessageParser {
    pub fn new(network: bitcoin::Network) -> Self {
        Self {
            parser: MessageParser::new(network),
        }
    }

    /// Classify all inputs and outputs without I/O or panics on malformed bytes.
    ///
    /// A witness inscription uses the nearest preceding P2WPKH input as its
    /// signer. An OP_RETURN message uses input zero when that input is P2WPKH;
    /// governance authorization still comes from tracked input roles rather
    /// than this informational signer.
    pub fn parse_transaction(
        &mut self,
        tx: &Transaction,
        tx_index: u32,
        block_height: u32,
        wallets: Option<&SystemWallets>,
    ) -> Vec<PositionedParseOutcome> {
        let tx_index = tx_index as usize;
        let bridge_vout = wallets.and_then(|wallets| {
            tx.output
                .iter()
                .position(|output| output.script_pubkey == wallets.bridge.script_pubkey())
        });
        let mut outcomes = Vec::with_capacity(tx.input.len() + tx.output.len());
        let mut preceding_signer = None;

        for (input_index, input) in tx.input.iter().enumerate() {
            if let Some(address) = self.parser.parse_p2wpkh(&input.witness) {
                preceding_signer = Some(address);
            }
            let location = MessageLocation::Input(input_index as u32);
            let metadata = CarrierMetadata {
                block_height,
                signer: preceding_signer.clone(),
                tx_index: Some(tx_index),
                output_vout: bridge_vout,
            };
            let parsed = self
                .parser
                .parse_system_carrier(input, tx, metadata, wallets);
            outcomes.push(classify(location, preceding_signer.as_ref(), parsed));
        }

        let output_signer = tx
            .input
            .first()
            .and_then(|input| self.parser.parse_p2wpkh(&input.witness));
        for output_index in 0..tx.output.len() {
            let location = MessageLocation::Output(output_index as u32);
            let parsed = self.parser.parse_output_carrier(
                tx,
                output_index,
                tx_index,
                block_height,
                wallets,
                output_signer.clone(),
            );
            outcomes.push(classify(location, output_signer.as_ref(), parsed));
        }

        outcomes
    }
}

fn classify(
    location: MessageLocation,
    signer: Option<&Address>,
    parsed: CarrierParse<FullInscriptionMessage>,
) -> PositionedParseOutcome {
    let signer_script = signer.map(Address::script_pubkey);
    match parsed {
        CarrierParse::Valid(message) => {
            PositionedParseOutcome::Valid(Box::new(PositionedMessage {
                message,
                location,
                signer_script,
            }))
        }
        CarrierParse::Malformed(detail) => PositionedParseOutcome::Malformed(PositionedMalformed {
            location,
            signer_script,
            code: RejectionCode::MalformedMessage,
            detail,
        }),
        CarrierParse::Unsupported(kind) => {
            PositionedParseOutcome::Unsupported(PositionedUnsupported {
                location,
                signer_script,
                kind,
            })
        }
        CarrierParse::ContextRequired => PositionedParseOutcome::ContextRequired {
            location,
            signer_script,
        },
        CarrierParse::Irrelevant => PositionedParseOutcome::Irrelevant { location },
    }
}

#[cfg(test)]
mod tests {
    use std::{panic::AssertUnwindSafe, str::FromStr};

    use bitcoin::{
        absolute::LockTime,
        hashes::Hash,
        key::UntweakedPublicKey,
        opcodes::{all::OP_RETURN, OP_FALSE, OP_TRUE},
        script::{Builder, PushBytesBuf},
        secp256k1::{Keypair, Secp256k1, SecretKey},
        taproot::{LeafVersion, TaprootBuilder},
        transaction::Version,
        Address, Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid,
        Witness,
    };

    use super::*;
    use crate::types::{self, FullInscriptionMessage};

    fn signer(seed: u8) -> (Address, Witness, UntweakedPublicKey) {
        let secp = Secp256k1::new();
        let secret = SecretKey::from_slice(&[seed; 32]).unwrap();
        let keypair = Keypair::from_secret_key(&secp, &secret);
        let public_key = keypair.public_key();
        let compressed = bitcoin::CompressedPublicKey(public_key);
        let address = Address::p2wpkh(&compressed, Network::Regtest);
        let witness = Witness::from_slice(&[vec![0; 71], public_key.serialize().to_vec()]);
        (address, witness, keypair.x_only_public_key().0)
    }

    fn input(index: u32, witness: Witness) -> TxIn {
        TxIn {
            previous_output: OutPoint {
                txid: Txid::from_byte_array([index as u8 + 1; 32]),
                vout: index,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness,
        }
    }

    fn attestation_witness(internal_key: UntweakedPublicKey, reference: Txid) -> Witness {
        let marker =
            PushBytesBuf::try_from(types::VIA_INSCRIPTION_PROTOCOL.as_bytes().to_vec()).unwrap();
        let kind =
            PushBytesBuf::try_from(types::VALIDATOR_ATTESTATION_MSG.as_bytes().to_vec()).unwrap();
        let reference = PushBytesBuf::try_from(reference.as_byte_array().to_vec()).unwrap();
        let script = Builder::new()
            .push_slice(internal_key.serialize())
            .push_opcode(bitcoin::opcodes::all::OP_CHECKSIG)
            .push_opcode(OP_FALSE)
            .push_opcode(bitcoin::opcodes::all::OP_IF)
            .push_slice(marker)
            .push_slice(kind)
            .push_slice(reference)
            .push_opcode(OP_TRUE)
            .push_opcode(bitcoin::opcodes::all::OP_ENDIF)
            .into_script();
        let secp = Secp256k1::new();
        let spend_info = TaprootBuilder::new()
            .add_leaf(0, script.clone())
            .unwrap()
            .finalize(&secp, internal_key)
            .unwrap();
        let control = spend_info
            .control_block(&(script.clone(), LeafVersion::TapScript))
            .unwrap()
            .serialize();
        Witness::from_slice(&[vec![0; 64], script.into_bytes(), control])
    }

    fn transaction(inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: inputs,
            output: outputs,
        }
    }

    fn wallets() -> SystemWallets {
        SystemWallets {
            sequencer: Address::from_str("bcrt1qw2mvkvm6alfhe86yf328kgvr7mupdx4vln7kpv")
                .unwrap()
                .assume_checked(),
            bridge: Address::from_str(
                "bcrt1pcx974cg2w66cqhx67zadf85t8k4sd2wp68l8x8agd3aj4tuegsgsz97amg",
            )
            .unwrap()
            .assume_checked(),
            governance: Address::from_str(
                "bcrt1q92gkfme6k9dkpagrkwt76etkaq29hvf02w5m38f6shs4ddpw7hzqp347zm",
            )
            .unwrap()
            .assume_checked(),
            verifiers: vec![],
        }
    }

    #[test]
    fn multi_input_attestations_keep_distinct_signers() {
        let (signer_a, signer_a_witness, internal_a) = signer(1);
        let (signer_b, signer_b_witness, internal_b) = signer(2);
        let reference_a = Txid::from_byte_array([0xA1; 32]);
        let reference_b = Txid::from_byte_array([0xB2; 32]);
        let tx = transaction(
            vec![
                input(0, signer_a_witness),
                input(1, attestation_witness(internal_a, reference_a)),
                input(2, signer_b_witness),
                input(3, attestation_witness(internal_b, reference_b)),
            ],
            vec![],
        );
        let outcomes =
            PositionedMessageParser::new(Network::Regtest).parse_transaction(&tx, 9, 100, None);
        let valid: Vec<_> = outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                PositionedParseOutcome::Valid(message) => Some(message),
                _ => None,
            })
            .collect();

        assert_eq!(valid.len(), 2);
        assert_eq!(valid[0].location, MessageLocation::Input(1));
        assert_eq!(valid[0].signer_script, Some(signer_a.script_pubkey()));
        assert_eq!(valid[1].location, MessageLocation::Input(3));
        assert_eq!(valid[1].signer_script, Some(signer_b.script_pubkey()));
        let references: Vec<_> = valid
            .iter()
            .map(|positioned| match &positioned.message {
                FullInscriptionMessage::ValidatorAttestation(message) => {
                    message.input.reference_txid
                }
                other => panic!("unexpected message: {other:?}"),
            })
            .collect();
        assert_eq!(references, vec![reference_a, reference_b]);
    }

    #[test]
    fn legacy_parser_keeps_transaction_wide_last_signer_behavior() {
        let (_, signer_a_witness, internal_a) = signer(7);
        let (signer_b, signer_b_witness, internal_b) = signer(8);
        let tx = transaction(
            vec![
                input(0, signer_a_witness),
                input(
                    1,
                    attestation_witness(internal_a, Txid::from_byte_array([0x71; 32])),
                ),
                input(2, signer_b_witness),
                input(
                    3,
                    attestation_witness(internal_b, Txid::from_byte_array([0x82; 32])),
                ),
            ],
            vec![],
        );
        let messages =
            MessageParser::new(Network::Regtest).parse_system_transaction(&tx, 100, None);

        assert_eq!(messages.len(), 2);
        assert!(messages.iter().all(|message| match message {
            FullInscriptionMessage::ValidatorAttestation(attestation) =>
                attestation.common.p2wpkh_address.as_ref() == Some(&signer_b),
            _ => false,
        }));
    }

    #[test]
    fn op_return_reports_nonzero_output_index_and_input_zero_signer() {
        let (signer_address, signer_witness, _) = signer(3);
        let replacement = signer(4).0;
        let payload = format!("VIA_PROTOCOL:SEQ:{}", replacement).into_bytes();
        let tx = transaction(
            vec![input(0, signer_witness)],
            vec![
                TxOut {
                    value: Amount::from_sat(1),
                    script_pubkey: signer_address.script_pubkey(),
                },
                TxOut {
                    value: Amount::ZERO,
                    script_pubkey: Builder::new()
                        .push_opcode(OP_RETURN)
                        .push_slice(PushBytesBuf::try_from(payload).unwrap())
                        .into_script(),
                },
            ],
        );
        let outcomes =
            PositionedMessageParser::new(Network::Regtest).parse_transaction(&tx, 7, 100, None);
        let positioned = outcomes
            .iter()
            .find_map(|outcome| match outcome {
                PositionedParseOutcome::Valid(message) => Some(message),
                _ => None,
            })
            .unwrap();

        assert_eq!(positioned.location, MessageLocation::Output(1));
        assert_eq!(
            positioned.signer_script,
            Some(signer_address.script_pubkey())
        );
        assert!(matches!(
            positioned.message,
            FullInscriptionMessage::UpdateSequencer(_)
        ));
    }

    #[test]
    fn malformed_payload_corpus_is_total() {
        let wallets = wallets();
        let bridge_output = TxOut {
            value: Amount::from_sat(50),
            script_pubkey: wallets.bridge.script_pubkey(),
        };
        let mut corpus = vec![
            vec![],
            vec![0],
            b"VIA_WI".to_vec(),
            b"VIA_PROTOCOL:SEQ:".to_vec(),
        ];
        for len in 1..96 {
            corpus.push(vec![len as u8; len]);
        }

        for (case_index, payload) in corpus.into_iter().enumerate() {
            let op_return = if payload.is_empty() {
                Builder::new().push_opcode(OP_RETURN).into_script()
            } else {
                Builder::new()
                    .push_opcode(OP_RETURN)
                    .push_slice(PushBytesBuf::try_from(payload).unwrap())
                    .into_script()
            };
            let tx = transaction(
                vec![input(0, Witness::from_slice(&[vec![1], vec![2], vec![3]]))],
                vec![
                    bridge_output.clone(),
                    TxOut {
                        value: Amount::ZERO,
                        script_pubkey: op_return,
                    },
                ],
            );
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                PositionedMessageParser::new(Network::Regtest).parse_transaction(
                    &tx,
                    0,
                    0,
                    Some(&wallets),
                )
            }));
            assert!(result.is_ok());
            if case_index < 4 {
                assert!(matches!(
                    &result.unwrap()[2],
                    PositionedParseOutcome::Malformed(PositionedMalformed {
                        location: MessageLocation::Output(1),
                        code: RejectionCode::MalformedMessage,
                        ..
                    })
                ));
            }
        }
    }

    #[test]
    fn recognized_truncated_witness_is_typed_malformed() {
        let (signer, signer_witness, internal_key) = signer(6);
        let mut witness = attestation_witness(internal_key, Txid::from_byte_array([0x66; 32]));
        let truncated = Builder::new()
            .push_slice(internal_key.serialize())
            .push_opcode(bitcoin::opcodes::all::OP_CHECKSIG)
            .push_opcode(OP_FALSE)
            .push_opcode(bitcoin::opcodes::all::OP_IF)
            .push_slice(
                PushBytesBuf::try_from(types::VIA_INSCRIPTION_PROTOCOL.as_bytes().to_vec())
                    .unwrap(),
            )
            .push_slice(
                PushBytesBuf::try_from(types::VALIDATOR_ATTESTATION_MSG.as_bytes().to_vec())
                    .unwrap(),
            )
            .push_opcode(bitcoin::opcodes::all::OP_ENDIF)
            .into_script();
        witness = Witness::from_slice(&[
            witness[0].to_vec(),
            truncated.into_bytes(),
            witness[2].to_vec(),
        ]);
        let tx = transaction(vec![input(0, signer_witness), input(1, witness)], vec![]);
        let outcomes =
            PositionedMessageParser::new(Network::Regtest).parse_transaction(&tx, 0, 0, None);

        assert!(matches!(
            &outcomes[1],
            PositionedParseOutcome::Malformed(PositionedMalformed {
                location: MessageLocation::Input(1),
                signer_script: Some(script),
                code: RejectionCode::MalformedMessage,
                ..
            }) if script == &signer.script_pubkey()
        ));
    }

    #[test]
    fn positioned_results_are_deterministic() {
        let (signer, signer_witness, internal) = signer(5);
        let reference = Txid::from_byte_array([0x55; 32]);
        let tx = transaction(
            vec![
                input(0, signer_witness),
                input(1, attestation_witness(internal, reference)),
            ],
            vec![TxOut {
                value: Amount::from_sat(1),
                script_pubkey: signer.script_pubkey(),
            }],
        );
        let first =
            PositionedMessageParser::new(Network::Regtest).parse_transaction(&tx, 3, 44, None);
        let second =
            PositionedMessageParser::new(Network::Regtest).parse_transaction(&tx, 3, 44, None);
        assert_eq!(first, second);
    }
}
