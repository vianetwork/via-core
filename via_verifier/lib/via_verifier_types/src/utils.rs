use bitcoin::Txid;
use zksync_types::H256;

pub fn convert_txid_to_h256(txid: Txid) -> H256 {
    let mut tx_id_bytes = txid.as_raw_hash()[..].to_vec();
    tx_id_bytes.reverse();
    H256::from_slice(&tx_id_bytes)
}
