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
    pub fee: Amount,
    pub fee_rate: u64,
}

impl UnsignedBridgeTx {
    pub fn get_fee_per_user(&self) -> Amount {
        let withdrawals_count = self.tx.output.len() as u64 - 2;
        if withdrawals_count == 0 {
            return self.fee;
        }
        Amount::from_sat(self.fee.to_sat() / withdrawals_count)
    }

    pub fn is_empty(&self) -> bool {
        self.tx.output.len() as u64 - 2 == 0
    }
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
