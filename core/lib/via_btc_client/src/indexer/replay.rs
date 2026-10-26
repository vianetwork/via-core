//! Immutable parsed content is separate from the inclusion that can change on a remine.

use serde::{Deserialize, Serialize};
use zksync_types::{
    l1::{priority_id::ViaPriorityOpId, via_l1::ViaL1Deposit, L1Tx},
    Address, PriorityOpId, H256,
};

use crate::types::{CommonFields, FullInscriptionMessage, L1ToL2Message, L1ToL2MessageInput};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageOrigin {
    pub height: u32,
    pub hash: bitcoin::BlockHash,
    pub transaction_index: u32,
    pub message_index: u32,
    pub txid: bitcoin::Txid,
}

impl TryFrom<&CommonFields> for MessageOrigin {
    type Error = anyhow::Error;
    fn try_from(common: &CommonFields) -> anyhow::Result<Self> {
        use anyhow::Context;
        Ok(Self {
            height: common.block_height,
            hash: common.block_hash.context("message has no scanned block hash")?,
            transaction_index: common
                .tx_index
                .context("message has no transaction index")?
                .try_into()?,
            message_index: common.message_index.try_into()?,
            txid: common.tx_id,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProofInput {
    pub proof: crate::types::ProofDAReferenceInput,
    /// Retained replay may rebind inclusion positions without changing either immutable payload.
    /// The proof row's captured header ID changes with that rebind, excluding stale completion.
    pub pubdata: Included<crate::types::L1BatchDAReferenceInput>,
}

/// Included pairs message position with immutable payload.
/// Remine after reorg rewrites origin and leaves payload untouched and comparable.
/// Idea only from NBXplorer schema, which keeps blks_txs memberships apart from transaction content: https://github.com/btcpayserver/NBXplorer/blob/27585a7a83b11a96adad9facf092c0d07d327ac2/NBXplorer/DBScripts/FullSchema.sql
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Included<T> {
    pub origin: MessageOrigin,
    pub payload: Payload<T>,
}

impl<T: PartialEq> Included<T> {
    pub fn compare(&self, saved: &Self) -> Result<(), ReplayConflict> {
        compare_fields([
            ("height", self.origin.height == saved.origin.height),
            ("block_hash", self.origin.hash == saved.origin.hash),
            ("transaction_index", self.origin.transaction_index == saved.origin.transaction_index),
            ("message_index", self.origin.message_index == saved.origin.message_index),
            ("txid", self.origin.txid == saved.origin.txid),
        ])?;
        self.payload.compare(&saved.payload)
    }

    pub fn new(common: &CommonFields, input: T) -> anyhow::Result<Self> {
        Ok(Self { origin: MessageOrigin::try_from(common)?, payload: Payload::new(common, input) })
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Bitcoin replay conflict in {field}")]
pub struct ReplayConflict {
    pub field: &'static str,
}

pub fn compare_fields(
    fields: impl IntoIterator<Item = (&'static str, bool)>,
) -> Result<(), ReplayConflict> {
    match fields.into_iter().find(|(_, equal)| !equal) {
        Some((field, _)) => Err(ReplayConflict { field }),
        None => Ok(()),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Payload<T> {
    pub input: T,
    pub public_key: Vec<u8>,
    pub signature: Vec<u8>,
    pub sender_script: Option<Vec<u8>>,
}

impl<T: PartialEq> Payload<T> {
    pub fn new(common: &CommonFields, input: T) -> Self {
        Self {
            input,
            public_key: common.encoded_public_key.as_bytes().to_vec(),
            signature: common.schnorr_signature.to_vec(),
            sender_script: common
                .p2wpkh_address
                .as_ref()
                .map(|address| address.script_pubkey().into_bytes()),
        }
    }

    /// compare reports first differing field between replayed and stored payload, after cheap full-equality check.
    /// Conflict names one field, so operator sees which input changed.
    /// Idea only from TigerBeetle create_transfer_exists, which names first differing field on duplicate transfer id: https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig
    pub fn compare(&self, saved: &Self) -> Result<(), ReplayConflict> {
        if self == saved {
            return Ok(());
        }
        compare_fields([
            ("input", self.input == saved.input),
            ("public_key", self.public_key == saved.public_key),
            ("signature", self.signature == saved.signature),
            ("sender_script", self.sender_script == saved.sender_script),
        ])
    }
}

pub type DepositPayload = Payload<(L1ToL2MessageInput, u64)>;

pub enum DepositReplay {
    Equal,
    NeedsRetainedCheck,
}

/// compare_deposit_replay accepts identical replay, rebinds origin of attached execution, rejects changed payload under same outpoint.
/// Same key with different content is conflict, never overwrite, so stored history stays trustworthy.
/// Idea only from Stripe idempotency keys via rocket-rides-atomic, which answers 409 when stored request params differ: https://github.com/brandur/rocket-rides-atomic/blob/94b370ddbdc8d08fb263f07c221bab42190d48e6/api.rb
pub fn compare_deposit_replay(
    incoming: &DepositPayload,
    source_hash: &str,
    saved_hash: Option<&str>,
    saved_payload: Option<serde_json::Value>,
    attached: bool,
) -> anyhow::Result<DepositReplay> {
    anyhow::ensure!(
        attached || saved_hash == Some(source_hash),
        ReplayConflict { field: "deposit_inclusion" }
    );
    if let Some(saved) = saved_payload {
        incoming.compare(&serde_json::from_value(saved)?)?;
        Ok(DepositReplay::Equal)
    } else {
        anyhow::ensure!(attached, ReplayConflict { field: "unreconciled_deposit" });
        Ok(DepositReplay::NeedsRetainedCheck)
    }
}

impl TryFrom<&L1ToL2Message> for ViaL1Deposit {
    type Error = anyhow::Error;

    fn try_from(message: &L1ToL2Message) -> anyhow::Result<Self> {
        use anyhow::Context;
        Ok(Self {
            l2_receiver_address: message.input.receiver_l2_address,
            amount: message.amount.to_sat(),
            calldata: message.input.call_data.clone(),
            l1_block_number: u64::from(message.common.block_height),
            tx_index: message.common.tx_index.context("deposit missing tx_index")?,
            output_vout: message.common.output_vout.context("deposit missing output_vout")?,
        })
    }
}

/// Bitcoin's displayed txid byte order is the persisted H256 order at Via's boundary.
pub fn transaction_hash(txid: bitcoin::Txid) -> H256 {
    use bitcoin::hashes::Hash;
    let mut bytes = txid.to_byte_array();
    bytes.reverse();
    H256(bytes)
}

pub struct ScannedDeposit {
    pub operation: L1Tx,
    pub txid: H256,
    pub origin: DepositOrigin,
    pub payload: DepositPayload,
}

impl ScannedDeposit {
    pub fn from_messages(messages: Vec<FullInscriptionMessage>) -> anyhow::Result<Vec<Self>> {
        let mut deposits = Vec::new();
        for message in messages {
            let FullInscriptionMessage::L1ToL2Message(message) = message else { continue };
            let deposit = ViaL1Deposit::try_from(&message)?;
            let Some(operation) = deposit.l1_tx() else {
                tracing::warn!("Invalid deposit, l1 tx_id {}", message.common.tx_id);
                continue;
            };
            deposits.push(Self {
                operation,
                txid: transaction_hash(message.common.tx_id),
                origin: DepositOrigin::try_from(&message.common)?,
                payload: Payload::new(&message.common, (message.input, message.amount.to_sat())),
            });
        }
        Ok(deposits)
    }
}

pub struct DepositOrigin {
    pub height: u32,
    pub hash: bitcoin::BlockHash,
    pub vout: u32,
}
impl TryFrom<&CommonFields> for DepositOrigin {
    type Error = anyhow::Error;
    fn try_from(common: &CommonFields) -> anyhow::Result<Self> {
        use anyhow::Context;
        let message = MessageOrigin::try_from(common)?;
        Ok(Self {
            height: message.height,
            hash: message.hash,
            vout: common.output_vout.context("deposit has no output position")?.try_into()?,
        })
    }
}

pub fn retained_deposit_operation(
    priority_id: PriorityOpId,
    receiver: Address,
    amount: u64,
    canonical_hash: H256,
) -> Result<L1Tx, ReplayConflict> {
    let position = ViaPriorityOpId(priority_id.0);
    let operation = ViaL1Deposit {
        l2_receiver_address: receiver,
        amount,
        calldata: Vec::new(),
        l1_block_number: position.block_number(),
        tx_index: position.tx_index() as usize,
        output_vout: position.vout() as usize,
    }
    .l1_tx()
    .ok_or(ReplayConflict { field: "retained_execution" })?;
    if operation.common_data.canonical_tx_hash != canonical_hash {
        return Err(ReplayConflict { field: "retained_execution" });
    }
    Ok(operation)
}
