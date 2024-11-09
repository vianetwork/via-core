use async_trait::async_trait;
use bitcoin::{
    hashes::Hash,
    key::UntweakedPublicKey,
    secp256k1::{
        ecdsa::Signature as ECDSASignature, schnorr::Signature as SchnorrSignature, All, Keypair,
        Message, PublicKey, Secp256k1,
    },
    Address, Block, BlockHash, CompressedPublicKey, Network, OutPoint, PrivateKey, ScriptBuf,
    Transaction, TxOut, Txid,
};
use std::sync::Arc;

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
}

impl MockBitcoinOpsConfig {
    pub fn set_block_height(&mut self, block_height: u128) {
        self.block_height = block_height;
    }

    pub fn set_tx_confirmation(&mut self, tx_confirmation: bool) {
        self.tx_confirmation = tx_confirmation;
    }
}

#[derive(Debug, Default)]
pub struct MockBitcoinOps {
    pub balance: u128,
    pub utxos: Vec<(OutPoint, TxOut)>,
    pub fee_rate: u64,
    pub block_height: u128,
    pub tx_confirmation: bool,
    pub transaction: Option<Transaction>,
    pub block: Option<Block>,
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
