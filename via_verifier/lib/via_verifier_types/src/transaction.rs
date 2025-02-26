use bincode::{deserialize, serialize};
use bitcoin::{Amount, OutPoint, Transaction, TxOut, Txid};
use serde::{Deserialize, Serialize};
use via_btc_client::traits::Serializable;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsignedBridgeTx {
    pub tx: Transaction,
    pub txid: Txid,
    pub utxos: Vec<(OutPoint, TxOut)>,
    pub change_amount: Amount,
}

impl Serializable for UnsignedBridgeTx {
    fn to_bytes(&self) -> Vec<u8> {
        serialize(self).expect("error serialize the UnsignedBridgeTx")
    }

    fn from_bytes(bytes: &[u8]) -> Self
    where
        Self: Sized,
    {
        deserialize(bytes).expect("error deserialize the UnsignedBridgeTx")
    }
}
