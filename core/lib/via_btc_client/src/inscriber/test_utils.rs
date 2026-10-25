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

#[derive(Debug, Clone)]
pub struct MockBitcoinOps {
    pub network: Network,
    pub balance: u128,
    pub utxos: Vec<(OutPoint, TxOut)>,
    pub fee_rate: u64,
    pub block_height: u64,
    pub tx_confirmation: bool,
    pub transaction: Option<Transaction>,
    pub block: Option<Block>,
    pub headers: Vec<crate::types::BlockHeaderInfo>,
    pub fee_history: Vec<u64>,
}

impl Default for MockBitcoinOps {
    fn default() -> Self {
        Self {
            network: Network::Regtest,
            balance: 0,
            utxos: Vec::new(),
            fee_rate: 0,
            block_height: 0,
            tx_confirmation: false,
            transaction: None,
            block: None,
            headers: Vec::new(),
            fee_history: Vec::new(),
        }
    }
}

impl MockBitcoinOps {
    pub fn set_block_height(&mut self, block_height: u64) {
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

#[async_trait]
impl BitcoinOps for MockBitcoinOps {
    async fn best_block_hash(&self) -> BitcoinClientResult<BlockHash> {
        self.headers
            .iter()
            .filter(|h| matches!(h.membership, crate::types::HeaderMembership::Canonical(_)))
            .max_by_key(|h| h.height)
            .map(|h| h.hash)
            .ok_or_else(|| types::BitcoinError::Rpc("No selected header fixture".into()))
    }
    async fn header_by_hash(
        &self,
        hash: &BlockHash,
    ) -> BitcoinClientResult<Option<crate::types::BlockHeaderInfo>> {
        Ok(self.headers.iter().find(|h| h.hash == *hash).copied())
    }
    async fn canonical_header_at(
        &self,
        height: u32,
    ) -> BitcoinClientResult<crate::types::BlockHeaderInfo> {
        self.headers
            .iter()
            .find(|h| {
                h.height == height
                    && matches!(h.membership, crate::types::HeaderMembership::Canonical(_))
            })
            .copied()
            .ok_or_else(|| types::BitcoinError::Rpc("No canonical header fixture".into()))
    }
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

    async fn fetch_block_height(&self) -> BitcoinClientResult<u64> {
        BitcoinClientResult::Ok(self.block_height)
    }

    async fn get_fee_rate(&self, _conf_target: u16) -> BitcoinClientResult<u64> {
        BitcoinClientResult::Ok(self.fee_rate)
    }

    fn get_network(&self) -> Network {
        self.network
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
        let sk = PrivateKey::new(
            bitcoin::secp256k1::SecretKey::from_slice(&[1; 32]).unwrap(),
            Network::Regtest,
        );
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
        Self::default()
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

pub fn get_mock_inscriber_and_conditions(config: MockBitcoinOps) -> Inscriber {
    let client = config;
    let signer = MockBitcoinSigner::new();
    let context = InscriberContext::default();

    Inscriber {
        client: Arc::new(client),
        signer: Arc::new(signer),
        context,
    }
}

#[cfg(any(test, feature = "test-utils"))]
mockall::mock! {
    pub BitcoinOpsExpectations {}
    #[async_trait]
    impl BitcoinOps for BitcoinOpsExpectations {
        async fn best_block_hash(&self) -> Result<bitcoin::BlockHash, crate::types::BitcoinError>;
        async fn header_by_hash(&self, hash: &bitcoin::BlockHash) -> Result<Option<crate::types::BlockHeaderInfo>, crate::types::BitcoinError>;
        async fn canonical_header_at(&self, height: u32) -> Result<crate::types::BlockHeaderInfo, crate::types::BitcoinError>;

        async fn get_transaction(&self, txid: &Txid) -> crate::types::BitcoinClientResult<Transaction>;
        async fn fetch_block(&self, block_height: u128) -> crate::types::BitcoinClientResult<Block>;
        async fn fetch_block_by_hash(&self, block_hash: &BlockHash) -> crate::types::BitcoinClientResult<Block>;
        async fn get_balance(&self, address: &Address) -> crate::types::BitcoinClientResult<u128>;
        async fn broadcast_signed_transaction(&self, signed_transaction: &str) -> crate::types::BitcoinClientResult<Txid>;
        async fn fetch_utxos(&self, address: &Address) -> crate::types::BitcoinClientResult<Vec<(OutPoint, TxOut)>>;
        async fn check_tx_confirmation(&self, txid: &Txid, conf_num: u32) -> crate::types::BitcoinClientResult<bool>;
        async fn fetch_block_height(&self) -> crate::types::BitcoinClientResult<u64>;
        async fn get_fee_rate(&self, conf_target: u16) -> crate::types::BitcoinClientResult<u64>;
        fn get_network(&self) -> bitcoin::Network;
        async fn get_block_stats(&self, height: u64) -> crate::types::BitcoinClientResult<GetBlockStatsResult>;
        async fn get_fee_history(
            &self,
            from_block_height: usize,
            to_block_height: usize,
        ) -> crate::types::BitcoinClientResult<Vec<u64>>;
    }
}
