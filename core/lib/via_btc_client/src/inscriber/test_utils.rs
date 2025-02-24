use std::sync::Arc;

use async_trait::async_trait;
use bitcoin::{
    hashes::Hash,
    key::UntweakedPublicKey,
    secp256k1::{
        ecdsa::Signature as ECDSASignature, schnorr::Signature as SchnorrSignature, All, Keypair,
        Message, PublicKey, Secp256k1,
    },
    Address, Amount, Block, BlockHash, CompressedPublicKey, Network, OutPoint, PrivateKey,
    ScriptBuf, Transaction, TxOut, Txid,
};
use bitcoincore_rpc::json::{FeeRatePercentiles, GetBlockStatsResult};

use super::Inscriber;
use crate::{
    traits::{BitcoinOps, BitcoinSigner},
    types::{self, BitcoinClientResult, InscriberContext},
};

#[derive(Debug, Default, Clone)]
pub struct MockBitcoinOpsConfig {
    pub balance: u128,
    pub utxos: Vec<(OutPoint, TxOut)>,
    pub fee_rate: u64,
    pub block_height: u128,
    pub tx_confirmation: bool,
    pub transaction: Option<Transaction>,
    pub block: Option<Block>,
    pub fee_history: Vec<u64>,
}

impl MockBitcoinOpsConfig {
    pub fn set_block_height(&mut self, block_height: u128) {
        self.block_height = block_height;
    }

    pub fn set_tx_confirmation(&mut self, tx_confirmation: bool) {
        self.tx_confirmation = tx_confirmation;
    }

    pub fn set_fee_history(&mut self, fees: Vec<u64>) {
        self.fee_history = fees;
    }

    pub fn set_utxos(&mut self, utxos: Vec<(OutPoint, TxOut)>) {
        self.utxos = utxos;
    }
}

#[derive(Debug, Default, Clone)]
pub struct MockBitcoinOps {
    pub balance: u128,
    pub utxos: Vec<(OutPoint, TxOut)>,
    pub fee_rate: u64,
    pub block_height: u128,
    pub tx_confirmation: bool,
    pub transaction: Option<Transaction>,
    pub block: Option<Block>,
    pub fee_history: Vec<u64>,
}

impl MockBitcoinOps {
    pub fn new(config: MockBitcoinOpsConfig) -> Self {
        Self {
            balance: config.balance,
            utxos: config.utxos,
            fee_rate: config.fee_rate,
            block_height: config.block_height,
            tx_confirmation: config.tx_confirmation,
            transaction: config.transaction,
            block: config.block,
            fee_history: config.fee_history,
        }
    }
}

#[async_trait]
impl BitcoinOps for MockBitcoinOps {
    async fn get_balance(&self, _address: &Address) -> BitcoinClientResult<u128> {
        BitcoinClientResult::Ok(self.balance)
    }

    async fn broadcast_signed_transaction(
        &self,
        _signed_transaction: &str,
    ) -> BitcoinClientResult<Txid> {
        BitcoinClientResult::Ok(Txid::from_slice(&[0u8; 32]).unwrap())
    }

    async fn fetch_utxos(&self, _address: &Address) -> BitcoinClientResult<Vec<(OutPoint, TxOut)>> {
        let default_utxos = vec![(
            OutPoint {
                txid: Txid::all_zeros(),
                vout: 0,
            },
            TxOut {
                value: Amount::from_btc(1.0).unwrap(),
                script_pubkey: _address.script_pubkey(),
            },
        )];
        if self.utxos.is_empty() {
            return BitcoinClientResult::Ok(default_utxos);
        }
        BitcoinClientResult::Ok(self.utxos.clone())
    }

    async fn check_tx_confirmation(
        &self,
        _txid: &Txid,
        _conf_num: u32,
    ) -> BitcoinClientResult<bool> {
        BitcoinClientResult::Ok(self.tx_confirmation)
    }

    async fn fetch_block_height(&self) -> BitcoinClientResult<u128> {
        BitcoinClientResult::Ok(self.block_height)
    }

    async fn get_fee_rate(&self, _conf_target: u16) -> BitcoinClientResult<u64> {
        BitcoinClientResult::Ok(self.fee_rate)
    }

    fn get_network(&self) -> Network {
        Network::Bitcoin
    }

    async fn fetch_block(&self, _block_height: u128) -> BitcoinClientResult<Block> {
        BitcoinClientResult::Ok(self.block.clone().expect("Block not set"))
    }

    async fn get_transaction(&self, _txid: &Txid) -> BitcoinClientResult<Transaction> {
        BitcoinClientResult::Ok(self.transaction.clone().expect("No transaction found"))
    }

    async fn fetch_block_by_hash(&self, _block_hash: &BlockHash) -> BitcoinClientResult<Block> {
        BitcoinClientResult::Ok(self.block.clone().expect("No block found"))
    }

    async fn get_fee_history(&self, _: usize, _: usize) -> BitcoinClientResult<Vec<u64>> {
        BitcoinClientResult::Ok(self.fee_history.clone())
    }

    async fn get_block_stats(&self, height: u64) -> BitcoinClientResult<GetBlockStatsResult> {
        BitcoinClientResult::Ok(GetBlockStatsResult {
            avg_fee: Amount::ZERO,
            avg_fee_rate: Amount::ZERO,
            avg_tx_size: 0,
            block_hash: BlockHash::all_zeros(),
            fee_rate_percentiles: FeeRatePercentiles {
                fr_10th: Amount::ZERO,
                fr_25th: Amount::ZERO,
                fr_50th: Amount::ZERO,
                fr_75th: Amount::ZERO,
                fr_90th: Amount::ZERO,
            },
            height,
            ins: 0,
            max_fee: Amount::ZERO,
            max_fee_rate: Amount::ZERO,
            max_tx_size: 0,
            median_fee: Amount::ZERO,
            median_time: 0,
            median_tx_size: 0,
            min_fee: Amount::ZERO,
            min_fee_rate: Amount::ZERO,
            min_tx_size: 0,
            outs: 0,
            subsidy: Amount::ZERO,
            sw_total_size: 0,
            sw_total_weight: 0,
            sw_txs: 0,
            time: 0,
            total_out: Amount::ZERO,
            total_size: 0,
            total_weight: 0,
            total_fee: Amount::ZERO,
            txs: 0,
            utxo_increase: 0,
            utxo_size_inc: 0,
        })
    }
}

#[derive(Debug, Clone)]
pub struct MockBitcoinSigner {
    pub ecdsa_signature: ECDSASignature,
    pub schnorr_signature: SchnorrSignature,
    pub address: Address,
    pub script: ScriptBuf,
    pub secp: Secp256k1<All>,
    pub internal_key: UntweakedPublicKey,
    pub public_key: PublicKey,
}

impl Default for MockBitcoinSigner {
    fn default() -> Self {
        let secp = Secp256k1::new();
        let sk = PrivateKey::generate(Network::Regtest);
        let keypair = Keypair::from_secret_key(&secp, &sk.inner);
        let compressed_pk = CompressedPublicKey::from_private_key(&secp, &sk)
            .expect("Failed to generate compressed public key");
        let address = Address::p2wpkh(&compressed_pk, Network::Regtest);
        let internal_key = keypair.x_only_public_key().0;
        let script = address.script_pubkey();

        Self {
            ecdsa_signature: ECDSASignature::from_compact(&[0; 64]).unwrap(),
            schnorr_signature: SchnorrSignature::from_slice(&[0; 64]).unwrap(),
            address,
            script,
            secp,
            internal_key,
            public_key: compressed_pk.0,
        }
    }
}

impl MockBitcoinSigner {
    pub fn new() -> Self {
        let secp = Secp256k1::new();
        let sk = PrivateKey::generate(Network::Regtest);
        let keypair = Keypair::from_secret_key(&secp, &sk.inner);
        let compressed_pk = CompressedPublicKey::from_private_key(&secp, &sk)
            .expect("Failed to generate compressed public key");
        let address = Address::p2wpkh(&compressed_pk, Network::Regtest);
        let internal_key = keypair.x_only_public_key().0;
        let script = address.script_pubkey();

        Self {
            ecdsa_signature: ECDSASignature::from_compact(&[0; 64]).unwrap(),
            schnorr_signature: SchnorrSignature::from_slice(&[0; 64]).unwrap(),
            address,
            script,
            secp,
            internal_key,
            public_key: compressed_pk.0,
        }
    }
}

#[async_trait::async_trait]
impl BitcoinSigner for MockBitcoinSigner {
    fn sign_ecdsa(&self, _: Message) -> types::BitcoinSignerResult<ECDSASignature> {
        BitcoinClientResult::Ok(self.ecdsa_signature)
    }

    fn sign_schnorr(&self, _: Message) -> types::BitcoinSignerResult<SchnorrSignature> {
        BitcoinClientResult::Ok(self.schnorr_signature)
    }

    fn get_p2wpkh_address(&self) -> types::BitcoinSignerResult<Address> {
        BitcoinClientResult::Ok(self.address.clone())
    }

    fn get_p2wpkh_script_pubkey(&self) -> &ScriptBuf {
        &self.script
    }

    fn get_secp_ref(&self) -> &Secp256k1<All> {
        &self.secp
    }

    fn get_internal_key(&self) -> types::BitcoinSignerResult<UntweakedPublicKey> {
        BitcoinClientResult::Ok(self.internal_key)
    }

    fn get_public_key(&self) -> PublicKey {
        self.public_key
    }
}

pub fn get_mock_inscriber_and_conditions(config: MockBitcoinOpsConfig) -> Inscriber {
    let client = MockBitcoinOps::new(config);
    let signer = MockBitcoinSigner::new();
    let context = InscriberContext::default();

    Inscriber {
        client: Arc::new(client),
        signer: Arc::new(signer),
        context,
    }
}
