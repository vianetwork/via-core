//! Isolated regtest processes use the provisioned, checksum-pinned executable, never PATH discovery.
// Adapt corepc's explicit executable/process owner; it terminates and waits on failed startup and Drop.
// https://git.rust-bitcoin.org/rust-bitcoin/corepc/src/commit/eca585605f6fbf79a1c8ce81ce86e2e4205cdf31/bitcoind/src/lib.rs

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Context;
use bitcoin::{
    absolute, block,
    consensus::encode::serialize_hex,
    hashes::{sha256, Hash},
    script::Builder,
    transaction, Address, Amount, Block, BlockHash, OutPoint, ScriptBuf, Sequence, Transaction,
    TxIn, TxMerkleNode, TxOut, Witness,
};
use bitcoind::{BitcoinD, Conf};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use via_btc_client::{client::BitcoinClient, types::NodeAuth};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

use crate::bitcoin_rpc::{BitcoinRpcServer, RpcError};

pub struct BitcoinCore {
    node: Arc<BitcoinD>,
    inscription_servers: Mutex<Vec<BitcoinRpcServer>>,
}

impl BitcoinCore {
    pub fn executable() -> anyhow::Result<PathBuf> {
        let path = PathBuf::from(
            std::env::var_os("VIA_TEST_BITCOIND")
                .context("VIA_TEST_BITCOIND must name the provisioned Core executable")?,
        );
        anyhow::ensure!(path.is_absolute(), "VIA_TEST_BITCOIND must be an absolute path");
        let pin: Value = serde_json::from_str(include_str!(
            "../../../../.github/test-fixtures/bitcoin-core.json"
        ))?;
        let expected = pin["executable_sha256"].as_str().context("missing Core executable pin")?;
        anyhow::ensure!(
            sha256::Hash::hash(&std::fs::read(&path)?).to_string() == expected,
            "Core executable checksum mismatch; provision the pinned archive"
        );
        Ok(path)
    }

    pub fn new() -> anyhow::Result<Self> {
        let mut conf = Conf::default();
        conf.args = vec![
            "-regtest",
            "-fallbackfee=0.0001",
            "-txindex=1",
            "-dbcache=32",
            "-persistmempool=0",
        ];
        conf.attempts = 2;
        let core = Self {
            node: Arc::new(BitcoinD::with_conf(Self::executable()?, &conf)?),
            inscription_servers: Mutex::new(Vec::new()),
        };
        let info: Value = core.rpc("getblockchaininfo", &[])?;
        anyhow::ensure!(info["chain"] == "regtest", "test Core must be isolated regtest");
        let info: Value = core.rpc("getnetworkinfo", &[])?;
        anyhow::ensure!(info["version"] == 280200, "test Core version must be 28.2");
        Ok(core)
    }

    pub fn rpc<T: DeserializeOwned>(&self, method: &str, params: &[Value]) -> anyhow::Result<T> {
        self.node
            .client
            .call(method, params)
            .with_context(|| format!("isolated Core {method} failed"))
    }

    /// The caller keeps this process owner alive while using its client.
    pub fn client(&self) -> anyhow::Result<Arc<BitcoinClient>> {
        Ok(Arc::new(BitcoinClient::new(
            &format!("http://{}", self.node.params.rpc_socket),
            NodeAuth::CookieFile(self.node.params.cookie_file.clone()),
            ViaBtcClientConfig::for_tests(),
        )?))
    }

    /// Only the fee estimate is fixed; transaction and broadcast RPCs still execute on this Core.
    // Core requires fee-history samples which an isolated fixture deliberately does not create.
    // https://github.com/bitcoin/bitcoin/blob/79d6e874e1dad6822b5d7e6993aa205d21ed2cbf/src/rpc/fees.cpp
    pub fn inscriber_client(&self) -> anyhow::Result<Arc<BitcoinClient>> {
        let node = self.node.clone();
        let server = BitcoinRpcServer::new_with_errors(
            Some(Duration::from_secs(10)),
            false,
            move |_, request| {
                if request["method"] == "estimatesmartfee" {
                    return Ok(json!({"feerate":0.00001,"blocks":1}));
                }
                let method = request["method"]
                    .as_str()
                    .ok_or_else(|| RpcError { code: -32600, message: "method required".into() })?;
                let params = request["params"]
                    .as_array()
                    .ok_or_else(|| RpcError { code: -32600, message: "params required".into() })?;
                node.client.call(method, params).map_err(|error| RpcError {
                    code: -32603,
                    message: format!("isolated inscription RPC {method}: {error}"),
                })
            },
        )?;
        let client = Arc::new(BitcoinClient::new(
            &server.url(""),
            NodeAuth::None,
            ViaBtcClientConfig::for_tests(),
        )?);
        self.inscription_servers.lock().unwrap().push(server);
        Ok(client)
    }

    pub fn mine(&self, count: u32) -> anyhow::Result<Vec<BlockHash>> {
        let address: String = self.rpc("getnewaddress", &[])?;
        self.rpc("generatetoaddress", &[json!(count), json!(address)])
    }

    pub fn fund(&self, address: &Address, amount: Amount) -> anyhow::Result<()> {
        let height: u32 = self.rpc("getblockcount", &[])?;
        if height < 101 {
            self.mine(101 - height)?;
        }
        let _: bitcoin::Txid =
            self.rpc("sendtoaddress", &[json!(address), json!(amount.to_btc())])?;
        self.mine(1)?;
        Ok(())
    }

    /// Port Core's named fixture: build on the current tip without submitting any block yet.
    // https://github.com/bitcoin/bitcoin/blob/79d6e874e1dad6822b5d7e6993aa205d21ed2cbf/test/functional/test_framework/blocktools.py
    pub fn create_empty_fork(&self, fork_length: u32) -> anyhow::Result<Vec<Block>> {
        let mut parent: BlockHash = self.rpc("getbestblockhash", &[])?;
        let mut height: u32 = self.rpc("getblockcount", &[])?;
        let tip: Value = self.rpc("getblock", &[json!(parent)])?;
        anyhow::ensure!(tip["height"] == height, "Core tip changed while preparing fork");
        let mut time = u32::try_from(tip["time"].as_u64().context("Core tip has no timestamp")?)?
            .checked_add(1)
            .context("fork timestamp overflow")?;
        let bits =
            u32::from_str_radix(tip["bits"].as_str().context("Core tip has no target")?, 16)?;
        let mut blocks = Vec::new();
        for _ in 0..fork_length {
            height = height.checked_add(1).context("fork height overflow")?;
            let mut script = Builder::new().push_int(i64::from(height));
            if height <= 16 {
                script = script.push_int(0);
            }
            let coinbase = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::from_consensus(height - 1),
                input: vec![TxIn {
                    previous_output: OutPoint::null(),
                    script_sig: script.into_script(),
                    sequence: Sequence::ENABLE_LOCKTIME_NO_RBF,
                    witness: Witness::default(),
                }],
                output: vec![TxOut {
                    value: Amount::from_sat(
                        5_000_000_000u64.checked_shr(height / 150).unwrap_or(0),
                    ),
                    script_pubkey: ScriptBuf::builder().push_int(1).into_script(),
                }],
            };
            let mut block = Block {
                header: block::Header {
                    version: block::Version::from_consensus(0x2000_0000),
                    prev_blockhash: parent,
                    merkle_root: TxMerkleNode::from_raw_hash(coinbase.compute_txid().to_raw_hash()),
                    time,
                    bits: bitcoin::CompactTarget::from_consensus(bits),
                    nonce: 0,
                },
                txdata: vec![coinbase],
            };
            while !block.header.target().is_met_by(block.block_hash()) {
                block.header.nonce =
                    block.header.nonce.checked_add(1).context("regtest nonce exhausted")?;
            }
            parent = block.block_hash();
            blocks.push(block);
            time = time.checked_add(1).context("fork timestamp overflow")?;
        }
        Ok(blocks)
    }

    pub fn submit_in_order(&self, blocks: &[Block]) -> anyhow::Result<()> {
        for block in blocks {
            let rejection: Option<String> =
                self.rpc("submitblock", &[json!(serialize_hex(block))])?;
            // Equal-work side branches may be stored before Core validates them as its best chain.
            // https://github.com/bitcoin/bitcoin/blob/79d6e874e1dad6822b5d7e6993aa205d21ed2cbf/test/functional/rpc_preciousblock.py#L20-L26
            anyhow::ensure!(
                rejection.is_none() || rejection.as_deref() == Some("inconclusive"),
                "Core rejected prepared block: {}",
                rejection.unwrap_or_default()
            );
            let stored: String = self.rpc("getblock", &[json!(block.block_hash()), json!(0)])?;
            anyhow::ensure!(
                stored == serialize_hex(block),
                "Core did not retain the submitted block"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_startup_is_bounded_and_removes_its_temporary_datadir() -> anyhow::Result<()> {
        let directory = bitcoind::tempfile::tempdir()?;
        let mut conf = Conf::default();
        conf.tmpdir = Some(directory.path().to_owned());
        conf.args = vec!["-regtest", "-via-invalid-startup-option"];
        conf.attempts = 1;
        let start = std::time::Instant::now();
        assert!(BitcoinD::with_conf(BitcoinCore::executable()?, &conf).is_err());
        assert!(start.elapsed() < Duration::from_secs(30));
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn natural_fork_is_prepared_without_mutating_core_then_submitted_in_order() -> anyhow::Result<()>
    {
        let core = BitcoinCore::new()?;
        core.mine(5)?;
        let original: BlockHash = core.rpc("getbestblockhash", &[])?;
        let fork = core.create_empty_fork(3)?;
        assert_eq!(core.rpc::<BlockHash>("getbestblockhash", &[])?, original);
        assert_eq!(fork[0].header.prev_blockhash, original);
        assert_eq!(fork[1].header.prev_blockhash, fork[0].block_hash());
        assert_eq!(fork[2].header.prev_blockhash, fork[1].block_hash());
        core.mine(1)?;
        core.submit_in_order(&fork)?;
        assert_eq!(core.rpc::<BlockHash>("getbestblockhash", &[])?, fork[2].block_hash());
        assert_eq!(core.rpc::<u32>("getblockcount", &[])?, 8);
        Ok(())
    }

    #[tokio::test]
    async fn invalidation_reports_noncanonical_without_a_replacement_block() -> anyhow::Result<()> {
        let core = BitcoinCore::new()?;
        let tip = core.mine(3)?[2];
        let client = core.client()?;
        assert!(matches!(
            client.header_by_hash(&tip).await?.unwrap().membership,
            via_btc_client::types::HeaderMembership::Canonical(_)
        ));
        let _: Value = core.rpc("invalidateblock", &[json!(tip)])?;
        assert_eq!(core.rpc::<u32>("getblockcount", &[])?, 2);
        assert_eq!(
            client.header_by_hash(&tip).await?.unwrap().membership,
            via_btc_client::types::HeaderMembership::Noncanonical
        );
        Ok(())
    }
}
