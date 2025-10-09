use via_btc_client::types::{BitcoinTxid, L1ToL2Message};
use zksync_types::{
    l1::{via_l1::ViaL1Deposit, L1Tx},
    H256,
};

pub fn convert_txid_to_h256(txid: BitcoinTxid) -> H256 {
    let mut tx_id_bytes = txid.as_raw_hash()[..].to_vec();
    tx_id_bytes.reverse();
    H256::from_slice(&tx_id_bytes)
}

pub fn create_l1_tx_from_message(msg: &L1ToL2Message) -> anyhow::Result<Option<(L1Tx, H256)>> {
    let deposit = ViaL1Deposit {
        l2_receiver_address: msg.input.receiver_l2_address,
        amount: msg.amount.to_sat(),
        calldata: msg.input.call_data.clone(),
        l1_block_number: msg.common.block_height as u64,
        tx_index: msg
            .common
            .tx_index
            .ok_or_else(|| anyhow::anyhow!("deposit missing tx_index"))?,
        output_vout: msg
            .common
            .output_vout
            .ok_or_else(|| anyhow::anyhow!("deposit missing output_vout"))?,
    };

    if let Some(l1_tx) = deposit.l1_tx() {
        let tx_id = convert_txid_to_h256(msg.common.tx_id);
        return Ok(Some((l1_tx, tx_id)));
    }
    Ok(None)
}

pub struct NormalizedDeposit {
    pub tx_id: H256,
    pub receiver: zksync_types::ethabi::Address,
    pub value_sat: i64,
    pub calldata: Vec<u8>,
    pub canonical_tx_hash: H256,
    pub priority_id: i64,
}

pub fn normalize_deposit_from_message(
    msg: &L1ToL2Message,
) -> anyhow::Result<Option<NormalizedDeposit>> {
    if let Some((l1_tx, tx_id)) = create_l1_tx_from_message(msg)? {
        let deposit = ViaL1Deposit {
            l2_receiver_address: msg.input.receiver_l2_address,
            amount: msg.amount.to_sat(),
            calldata: msg.input.call_data.clone(),
            l1_block_number: msg.common.block_height as u64,
            tx_index: msg.common.tx_index.unwrap(),
            output_vout: msg.common.output_vout.unwrap(),
        };

        let receiver = deposit.l2_receiver_address;
        let amount = deposit.amount as i64;
        let calldata = deposit.calldata.clone();
        let priority_id = deposit.priority_id().0 as i64;

        Ok(Some(NormalizedDeposit {
            tx_id,
            receiver,
            value_sat: amount,
            calldata,
            canonical_tx_hash: l1_tx.common_data.canonical_tx_hash,
            priority_id,
        }))
    } else {
        Ok(None)
    }
}
