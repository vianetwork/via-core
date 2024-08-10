use std::collections::{HashMap, VecDeque};

use crate::traits::{BitcoinInscriber, BitcoinOps, BitcoinSigner};
use anyhow::{Context, Ok, Result};
pub use bitcoin::Network as BitcoinNetwork;
use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, TxIn, TxOut, Witness};

use crate::client::BitcoinClient;
use crate::signer::KeyManager;

mod types;

const CTX_REQUIRED_CONFIRMATIONS: u32 = 1;

const COMMIT_TX_CHANGE_OUTPUT_INDEX: u32 = 0;
const COMMIT_TX_TAPSCRIPT_OUTPUT_INDEX: u32 = 1;
const REVEAL_TX_CHANGE_OUTPUT_INDEX: u32 = 0;
const REVEAL_TX_FEE_INPUT_INDEX: u32 = 0;
const REVEAL_TX_TAPSCRIPT_REVEAL_INDEX: u32 = 1;

struct CommitTxInputRes {
    commit_tx_inputs: Vec<TxIn>,
    unlocked_value: Amount,
    inputs_count: u32,
    utxo_amounts: Vec<Amount>,
}
struct Inscriber {
    client: Box<dyn BitcoinOps>,
    signer: Box<dyn BitcoinSigner>,
    context: types::InscriberContext,
}

// the upper layer call the inscriber in chainable way
// let snapshot = inscriber_instance
//    .inscribe(input)
//    .await?
//    .get_context_snapshot()
//    .await?;
//
//  persist(snapshot)
impl Inscriber {
    pub async fn new(
        rpc_url: &str,
        network: BitcoinNetwork,
        signer_private_key: &str,
        persisted_ctx: Option<types::InscriberContext>,
    ) -> Result<Self> {
        let client = Box::new(BitcoinClient::new(rpc_url, network).await?);
        let signer = Box::new(KeyManager::new(signer_private_key, network)?);
        let context: types::InscriberContext;

        match persisted_ctx {
            Some(ctx) => {
                context = ctx;
            }
            None => {
                context = types::InscriberContext::new();
            }
        }

        Ok(Self {
            client,
            signer,
            context,
        })
    }

    pub async fn inscribe(&mut self, input: types::InscriberInput) -> Result<()> {
        self.sync_context_with_blockchain().await;

        let commit_tx_input_info = self.prepare_commit_tx_input().await?;

        self.prepare_commit_tx_output().await;

        self.sign_commit_tx().await;

        self.prepare_reveal_tx_input().await;

        self.prepare_reveal_tx_output().await;

        self.sign_reveal_tx().await;

        self.broadcast_insription().await;

        self.insert_inscription_to_context().await;

        Ok(())
    }

    async fn sync_context_with_blockchain(&mut self) -> Result<()> {
        if self.context.fifo_queue.is_empty() {
            return Ok(());
        }

        let mut new_queue: VecDeque<types::InscriptionRequest> = VecDeque::new();

        let original_queue = self.context.fifo_queue.clone();

        let mut index = 0;
        while let Some(inscription) = self.context.fifo_queue.pop_front() {
            let txid_ref = &inscription.fee_payer_ctx.fee_payer_utxo_txid;
            let res = self
                .client
                .check_tx_confirmation(txid_ref, CTX_REQUIRED_CONFIRMATIONS)
                .await?;

            if index == 0 && !res {
                // add poped inscription back to the first of queue
                self.context.fifo_queue = original_queue;
                break;
            }
            if !res {
                new_queue.push_back(inscription);
            }

            index += 1;
        }

        self.context.fifo_queue = new_queue;

        Ok(())
    }

    async fn prepare_commit_tx_input(&self) -> Result<CommitTxInputRes> {
        let mut commit_tx_inputs: Vec<TxIn> = Vec::new();
        let mut unlocked_value: Amount = Amount::ZERO;
        let mut inputs_count: u32 = 0;
        let mut utxo_amounts: Vec<Amount> = Vec::new();

        let address_ref = &self.signer.get_p2wpkh_address()?;
        let mut utxos = self.client.fetch_utxos(address_ref).await?;

        /*
            adjust utxos list based on unconfirmed utxos in context

            !!! Only Service should send transaction with this address otherwise it will cause a problem in this code !!!
        */

        let context_queue_len = self.context.fifo_queue.len();

        let mut spent_utxos: HashMap<OutPoint, bool> = HashMap::new();

        for i in 0..context_queue_len {
            let is_head = i == 0;

            if let Some(inscription_req) = self.context.fifo_queue.get(i) {
                let commit_input = inscription_req.commit_tx_input.clone();
                let commit_input_count = commit_input.txids.len();

                for j in 0..commit_input_count {
                    let txid = commit_input.txids.get(j);
                    let vout = commit_input.vouts.get(j);

                    if let (Some(txid), Some(vout)) = (txid, vout) {
                        let outpoint = OutPoint {
                            txid: *txid,
                            vout: *vout,
                        };

                        spent_utxos.insert(outpoint, true);
                    }
                }

                let reveal_fee_payer_input = inscription_req.inscriber_output.commit_txid.clone();

                let reveal_fee_payer_input = OutPoint {
                    txid: reveal_fee_payer_input,
                    vout: REVEAL_TX_FEE_INPUT_INDEX,
                };

                spent_utxos.insert(reveal_fee_payer_input, true);

                if !is_head {
                    let reveal_tx_change_output =
                        inscription_req.inscriber_output.reveal_txid.clone();

                    let reveal_tx_change_output = OutPoint {
                        txid: reveal_tx_change_output,
                        vout: REVEAL_TX_CHANGE_OUTPUT_INDEX,
                    };

                    spent_utxos.insert(reveal_tx_change_output, true);
                }
            }
        }

        // iterate over utxos and filter out spent utxos and non p2wpkh utxos

        utxos.retain(|utxo| {
            let is_spent = spent_utxos.contains_key(&utxo.0);
            let is_p2wpkh = self.is_p2wpkh(&utxo.1.script_pubkey);

            !is_spent && is_p2wpkh
        });

        // add context available utxos to final utxos
        if context_queue_len > 0 {
            if let Some (head_inscription) = self.context.fifo_queue.get(0) {
                let reveal_change_output = head_inscription.inscriber_output.reveal_txid.clone();

                let reveal_change_output = OutPoint {
                    txid: reveal_change_output,
                    vout: head_inscription.fee_payer_ctx.fee_payer_utxo_vout,
                };


                let reveal_txout = TxOut {
                    value: head_inscription.fee_payer_ctx.fee_payer_utxo_value,
                    script_pubkey: self.signer.get_p2wpkh_script_pubkey().clone(),
                };

                utxos.push((reveal_change_output, reveal_txout));

            }
        }

        for (outpoint, txout) in utxos {
            let txin = TxIn {
                previous_output: outpoint,
                script_sig: ScriptBuf::default(), // For a p2wpkh script_sig is empty.
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::default(), // Get filled in after signing.
            };

            commit_tx_inputs.push(txin);
            unlocked_value += txout.value;
            inputs_count += 1;
            utxo_amounts.push(txout.value);
        }

        let res = CommitTxInputRes {
            commit_tx_inputs,
            unlocked_value,
            inputs_count,
            utxo_amounts,
        };

        Ok(res)
    }

    fn is_p2wpkh(&self, script_pubkey: &ScriptBuf) -> bool {
        let p2wpkh_script = self.signer.get_p2wpkh_script_pubkey();
        
        script_pubkey == p2wpkh_script
    }

    async fn prepare_commit_tx_output(&self) {
        todo!();
    }

    async fn sign_commit_tx(&self) {
        todo!();
    }

    async fn prepare_reveal_tx_input(&self) {
        todo!();
    }

    async fn prepare_reveal_tx_output(&self) {
        todo!();
    }

    async fn sign_reveal_tx(&self) {
        todo!();
    }

    async fn broadcast_insription(&self) {
        todo!();
    }

    async fn insert_inscription_to_context(&self) {
        todo!();
    }

    pub async fn get_context_snapshot(&self) {
        todo!();
    }

    pub async fn recreate_context_from_snapshot() {
        todo!();
    }

    async fn rebroadcast_whole_context(&self) {
        todo!();
    }
}
