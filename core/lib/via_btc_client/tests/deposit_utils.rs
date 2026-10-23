use std::{str::FromStr, sync::Arc};

use anyhow::{Context, Result};
use bitcoin::Amount;
use tracing::debug;
use via_btc_client::{
    client::BitcoinClient,
    inscriber::Inscriber,
    traits::BitcoinOps,
    types::{BitcoinAddress, BitcoinNetwork, InscriptionMessage, L1ToL2MessageInput, Recipient},
};
use via_test_utils::bitcoin_core::BitcoinCore;
use zksync_types::Address as EVMAddress;

pub mod config {
    use super::*;

    pub const NETWORK: BitcoinNetwork = BitcoinNetwork::Regtest;

    pub const BRIDGE_MUSIG2_ADDRESS: &str =
        "bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq";
    pub const TEST_RECEIVER_L2_ADDRESS: &str = "0x1234567890123456789012345678901234567890";

    pub const DEFAULT_DEPOSIT_AMOUNT_SATS: u64 = 100000;
    pub const DEFAULT_FUNDING_AMOUNT_BTC: f64 = 0.1;

}

pub struct DepositTestUtils;

impl DepositTestUtils {
    pub fn create_bitcoin_client(core: &BitcoinCore) -> Result<Arc<BitcoinClient>> {
        core.inscriber_client()
    }

    pub fn get_bridge_address() -> Result<BitcoinAddress> {
        let bridge_address = BitcoinAddress::from_str(config::BRIDGE_MUSIG2_ADDRESS)
            .expect("Invalid bridge address")
            .require_network(config::NETWORK)?;
        Ok(bridge_address)
    }

    pub fn get_test_receiver_l2_address() -> Result<EVMAddress> {
        let receiver_address = EVMAddress::from_str(config::TEST_RECEIVER_L2_ADDRESS)
            .expect("Invalid test receiver address");
        Ok(receiver_address)
    }

    pub async fn get_address_from_private_key(private_key: &str) -> Result<String> {
        let key = bitcoin::PrivateKey::from_wif(private_key)?;
        let public = bitcoin::CompressedPublicKey::from_private_key(
            &bitcoin::secp256k1::Secp256k1::new(),
            &key,
        )?;
        Ok(bitcoin::Address::p2wpkh(&public, config::NETWORK).to_string())
    }

    pub async fn fund_test_address(
        core: &BitcoinCore,
        test_address: &str,
        amount_btc: f64,
    ) -> Result<()> {
        let address = BitcoinAddress::from_str(test_address)?.require_network(config::NETWORK)?;
        core.fund(&address, Amount::from_btc(amount_btc)?)
    }

    pub async fn setup_funded_inscriber_with_key(
        core: &BitcoinCore,
        private_key: &str,
    ) -> Result<(Arc<BitcoinClient>, Inscriber)> {
        debug!("Setting up funded inscriber for testing with private key");

        let client = Self::create_bitcoin_client(core)?;

        let test_address = Self::get_address_from_private_key(private_key).await?;
        debug!("Test address derived from private key: {}", test_address);

        Self::fund_test_address(core, &test_address, config::DEFAULT_FUNDING_AMOUNT_BTC).await?;

        let context = None;
        let inscriber = Inscriber::new(client.clone(), private_key, context)
            .await
            .context("Failed to create Depositor Inscriber")?;

        Ok((client, inscriber))
    }

    pub fn create_test_l1_to_l2_message() -> Result<L1ToL2MessageInput> {
        let receiver_l2_address = Self::get_test_receiver_l2_address()?;
        Ok(L1ToL2MessageInput {
            receiver_l2_address,
            l2_contract_address: EVMAddress::zero(),
            call_data: vec![],
        })
    }

    pub fn create_test_recipient(amount_sats: u64) -> Result<Recipient> {
        let bridge_address = Self::get_bridge_address()?;
        Ok(Recipient {
            address: bridge_address,
            amount: Amount::from_sat(amount_sats),
        })
    }

    pub async fn perform_deposit_test_with_key(
        core: &BitcoinCore,
        amount_sats: Option<u64>,
        private_key: &str,
    ) -> Result<()> {
        let amount = amount_sats.unwrap_or(config::DEFAULT_DEPOSIT_AMOUNT_SATS);
        let client = Self::create_bitcoin_client(core)?;
        let mut inscriber = Inscriber::new(client.clone(), private_key, None)
            .await
            .context("Failed to create Depositor Inscriber")?;
        let balance = inscriber.get_balance().await.context("Failed to get balance")?;
        anyhow::ensure!(
            balance >= u128::from(amount),
            "Insufficient balance: {balance} < {amount}"
        );
        let input = Self::create_test_l1_to_l2_message()?;
        let recipient = Self::create_test_recipient(amount)?;
        let deposit = inscriber
            .inscribe_with_recipient(InscriptionMessage::L1ToL2Message(input), Some(recipient))
            .await?;
        core.mine(1)?;
        anyhow::ensure!(
            client.check_tx_confirmation(&deposit.final_reveal_tx.txid, 1).await?,
            "deposit was not confirmed by isolated Core"
        );
        Ok(())
    }

    pub async fn perform_opreturn_deposit_test(
        core: &BitcoinCore,
        amount_sats: Option<u64>,
        private_key: &str,
    ) -> Result<String> {
        let amount = amount_sats.unwrap_or(config::DEFAULT_DEPOSIT_AMOUNT_SATS);

        debug!("Starting OP_RETURN deposit test");
        debug!("Amount: {} sats", amount);

        let client = Self::create_bitcoin_client(core)?;

        let bridge_address = Self::get_bridge_address()?;
        let receiver_l2_address = Self::get_test_receiver_l2_address()?;

        let txid = Self::create_opreturn_transaction(
            &client,
            private_key,
            amount,
            &bridge_address,
            &receiver_l2_address,
        )
        .await?;

        core.mine(1)?;
        debug!("OP_RETURN deposit successful!");
        debug!("Transaction ID: {}", txid);

        Ok(txid)
    }

    pub async fn create_opreturn_transaction(
        client: &Arc<BitcoinClient>,
        private_key: &str,
        amount_sats: u64,
        bridge_address: &BitcoinAddress,
        receiver_l2_address: &EVMAddress,
    ) -> Result<String> {
        use bitcoin::{
            absolute,
            consensus::encode::serialize_hex,
            secp256k1::{Message, Secp256k1},
            sighash::{EcdsaSighashType, SighashCache},
            transaction, Address, Amount, CompressedPublicKey, PrivateKey, ScriptBuf, Sequence,
            Transaction, TxIn, TxOut, Witness,
        };

        let secp = Secp256k1::new();
        let private_key = PrivateKey::from_wif(private_key)
            .map_err(|e| anyhow::anyhow!("Invalid private key: {}", e))?;
        let pk = private_key.inner.public_key(&secp);
        let compressed_pk = CompressedPublicKey::from_private_key(&secp, &private_key)
            .map_err(|e| anyhow::anyhow!("Failed to create compressed public key: {}", e))?;
        let address = Address::p2wpkh(&compressed_pk, config::NETWORK);

        let amount = Amount::from_sat(amount_sats);
        let fees = Amount::from_btc(0.0001)?;

        let all_utxos = client.fetch_utxos(&address).await?;
        let total_needed = amount + fees;
        let mut selected_utxos = Vec::new();
        let mut input_amount = Amount::from_sat(0);
        for (outpoint, txout) in all_utxos.into_iter() {
            selected_utxos.push((outpoint, txout));
            input_amount += selected_utxos.last().unwrap().1.value;
            if input_amount >= total_needed {
                break;
            }
        }

        if input_amount < total_needed {
            return Err(anyhow::anyhow!("Insufficient funds"));
        }

        let tx_inputs: Vec<TxIn> = selected_utxos
            .iter()
            .map(|(outpoint, _)| TxIn {
                previous_output: *outpoint,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            })
            .collect();

        let mut outputs = Vec::new();

        outputs.push(TxOut {
            value: amount,
            script_pubkey: bridge_address.script_pubkey(),
        });

        outputs.push(TxOut {
            value: Amount::from_sat(0),
            script_pubkey: ScriptBuf::new_op_return(receiver_l2_address.to_fixed_bytes()),
        });

        let change_amount = input_amount - total_needed;
        if change_amount > Amount::from_sat(0) {
            outputs.push(TxOut {
                value: change_amount,
                script_pubkey: address.script_pubkey(),
            });
        }

        let mut tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: tx_inputs,
            output: outputs,
        };

        let sighash_type = EcdsaSighashType::All;
        let mut cache = SighashCache::new(&mut tx);
        for (i, (_, utxo)) in selected_utxos.iter().enumerate() {
            let sighash = cache
                .p2wpkh_signature_hash(i, &utxo.script_pubkey, utxo.value, sighash_type)
                .map_err(|e| anyhow::anyhow!("Failed to create signature hash: {}", e))?;

            let msg = Message::from(sighash);
            let signature = secp.sign_ecdsa(&msg, &private_key.inner);

            let signature = bitcoin::ecdsa::Signature {
                signature,
                sighash_type,
            };

            cache
                .witness_mut(i)
                .ok_or_else(|| anyhow::anyhow!("Failed to get witness"))
                .map(|witness| *witness = Witness::p2wpkh(&signature, &pk))?;
        }

        let tx = cache.into_transaction();

        let tx_hex = serialize_hex(&tx);
        let txid = client.broadcast_signed_transaction(&tx_hex).await?;

        Ok(txid.to_string())
    }
}
