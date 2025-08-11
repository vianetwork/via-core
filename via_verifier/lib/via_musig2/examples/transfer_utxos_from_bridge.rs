use std::{collections::HashMap, fs::File, str::FromStr};

use anyhow::{anyhow, Context, Result};
use bitcoin::{
    absolute::LockTime,
    consensus::{self, encode::serialize_hex},
    hashes::Hash,
    hex::DisplayHex,
    secp256k1::{self, Keypair, Secp256k1, SecretKey},
    sighash::{Prevouts, SighashCache},
    taproot::LeafVersion,
    transaction::Version,
    Address, Amount, Network, OutPoint, PrivateKey, ScriptBuf, Sequence, TapLeafHash,
    TapSighashType, Transaction, TxIn, TxOut, Txid, Witness,
};
use bitcoincore_rpc::Auth;
use clap::Parser;
use serde::{Deserialize, Serialize};
use via_btc_client::{client::BitcoinClient, traits::BitcoinOps};
use zksync_config::configs::via_btc_client::ViaBtcClientConfig;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Args {
    #[arg(long)]
    private_key: Option<String>,

    #[arg(long)]
    signers_public_keys: Vec<String>,

    #[arg(long, default_value = "my_wallet.json")]
    wallet_path: String,

    #[arg(long, default_value = "utxos.json")]
    utxos_path: Option<String>,

    #[arg(long)]
    from_address: Option<String>,

    #[arg(long)]
    to_address: Option<String>,

    #[arg(long, default_value = "500")]
    fee: u64,

    #[arg(long, default_value = "regtest")]
    network: String,

    #[arg(long, default_value = "gov_bridge_tx.json")]
    output: String,

    #[arg(long, default_value = "prepare")]
    action: String,

    #[arg(long, default_value = "http://0.0.0.0:18443")]
    rpc_url: String,

    #[arg(long, default_value = "rpcuser")]
    rpc_username: String,

    #[arg(long, default_value = "rpcpassword")]
    rpc_password: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
enum Action {
    Prepare,
    Sign,
    Finalize,
    Broadcast,
}

impl Action {
    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "prepare" => Ok(Action::Prepare),
            "sign" => Ok(Action::Sign),
            "finalize" => Ok(Action::Finalize),
            "broadcast" => Ok(Action::Broadcast),
            _ => Err(anyhow!("Invalid action: {s}")),
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
struct UTXO {
    txid: String,
    vout: u32,
    value: u64,
}

impl UTXO {
    fn to_parts(&self, from: &Address) -> (OutPoint, TxOut) {
        (
            OutPoint {
                txid: Txid::from_str(&self.txid).expect("Invalid txid format"),
                vout: self.vout,
            },
            TxOut {
                value: Amount::from_sat(self.value),
                script_pubkey: from.script_pubkey(),
            },
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct WalletData {
    public_keys: Vec<String>,
    aggregated_internal_key: String,
    governance_script_hex: String,
    taproot_output_key: String,
    merkle_root: Option<String>,
    taproot_address: String,
    control_block: String,
    threshold: usize,
    total_governance_keys: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct OutputData {
    messages: Vec<String>,
    signatures: Option<HashMap<String, Vec<String>>>,
    tx: String,
    signed_tx: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    match Action::from_str(&args.action)? {
        Action::Prepare => do_prepare(&args).await?,
        Action::Sign => do_sign(&args)?,
        Action::Finalize => do_finalize(&args)?,
        Action::Broadcast => do_broadcast(&args).await?,
    }
    Ok(())
}

async fn do_prepare(args: &Args) -> Result<()> {
    let wallet: WalletData = read_json(&args.wallet_path)?;
    let network = Network::from_str(&args.network)?;
    let from_address = Address::from_str(
        &args
            .from_address
            .clone()
            .ok_or_else(|| anyhow!("Missing from address"))?,
    )?
    .require_network(network.clone())?;
    let to_address = Address::from_str(
        &args
            .to_address
            .clone()
            .ok_or(anyhow!("Missing to address"))?,
    )?
    .require_network(network.clone())?;

    let utxos: Vec<(OutPoint, TxOut)> = read_json::<Vec<UTXO>>(
        &args
            .utxos_path
            .clone()
            .ok_or(anyhow!("Missing utxos path"))?,
    )?
    .into_iter()
    .map(|u| u.to_parts(&from_address))
    .collect();

    let tx = build_tx(utxos.clone(), Amount::from_sat(args.fee), to_address);

    let messages = compute_inputs_sig_hashes(
        tx.clone(),
        utxos.clone(),
        ScriptBuf::from_hex(&wallet.governance_script_hex)?,
    )
    .await?
    .iter()
    .map(|m| hex::encode(m.as_ref()))
    .collect();

    let output_data = OutputData {
        messages,
        signatures: None,
        tx: serialize_hex(&tx),
        signed_tx: None,
    };
    write_json(&args.output, &output_data)
}

fn do_sign(args: &Args) -> Result<()> {
    let mut output: OutputData = read_json(&args.output)?;
    let messages = output
        .messages
        .iter()
        .map(|m| {
            let bytes = hex::decode(m)?;
            secp256k1::Message::from_digest_slice(&bytes).map_err(|e| anyhow!(e))
        })
        .collect::<Result<Vec<_>>>()?;

    let pk_wif = args
        .private_key
        .clone()
        .ok_or_else(|| anyhow!("Missing private key"))?;
    let secp = Secp256k1::new();
    let sk = PrivateKey::from_wif(&pk_wif)?;
    let keypair =
        Keypair::from_secret_key(&secp, &SecretKey::from_slice(&sk.inner.secret_bytes())?);

    let signer_signatures = sign_tx(keypair, messages)?
        .iter()
        .map(|sig| sig.to_hex_string(bitcoin::hex::Case::Lower))
        .collect::<Vec<_>>();

    output
        .signatures
        .get_or_insert_with(HashMap::new)
        .insert(keypair.public_key().to_string(), signer_signatures);

    write_json(&args.output, &output)
}

fn do_finalize(args: &Args) -> Result<()> {
    let mut output: OutputData = read_json(&args.output)?;
    let wallet: WalletData = read_json(&args.wallet_path)?;
    let utxos: Vec<UTXO> = read_json(
        &args
            .utxos_path
            .clone()
            .ok_or(anyhow!("Missing utxos path"))?,
    )?;

    let signatures = output
        .signatures
        .clone()
        .ok_or_else(|| anyhow!("Missing signatures"))?;

    let witnesses = build_witnesses(
        args.signers_public_keys.clone(),
        utxos.len(),
        hex::decode(&wallet.governance_script_hex)?,
        hex::decode(&wallet.control_block)?,
        signatures,
    )?;

    let tx: Transaction = consensus::deserialize(&hex::decode(&output.tx)?)?;
    let signed_tx = finalize_tx(tx, witnesses)?;
    output.signed_tx = Some(serialize_hex(&signed_tx));

    write_json(&args.output, &output)
}

async fn do_broadcast(args: &Args) -> Result<()> {
    let output: OutputData = read_json(&args.output)?;

    let auth = Auth::UserPass(args.rpc_username.clone(), args.rpc_password.clone());
    let config = ViaBtcClientConfig {
        network: args.network.clone(),
        external_apis: vec![],
        fee_strategies: vec![],
        use_rpc_for_fee_rate: None,
    };

    let btc_client = BitcoinClient::new(&args.rpc_url.clone(), auth, config).unwrap();
    let Some(signed_tx) = output.signed_tx else {
        anyhow::bail!("signed signature missing");
    };
    let txid = btc_client.broadcast_signed_transaction(&signed_tx).await?;

    println!("Txid: {:?}", txid.to_string());
    Ok(())
}

// ---------- Helpers ----------

fn read_json<T: for<'de> Deserialize<'de>>(path: &str) -> Result<T> {
    let file = File::open(path).with_context(|| format!("Opening {path}"))?;
    Ok(serde_json::from_reader(file)?)
}

fn write_json<T: Serialize>(path: &str, value: &T) -> Result<()> {
    let file = File::create(path).with_context(|| format!("Creating {path}"))?;
    Ok(serde_json::to_writer_pretty(file, value)?)
}

fn build_tx(utxos: Vec<(OutPoint, TxOut)>, fee: Amount, to: Address) -> Transaction {
    let total_amount = utxos.iter().map(|u| u.1.value).sum::<Amount>() - fee;
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: utxos
            .into_iter()
            .map(|(op, _)| TxIn {
                previous_output: op,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::default(),
            })
            .collect(),
        output: vec![TxOut {
            value: total_amount,
            script_pubkey: to.script_pubkey(),
        }],
    }
}

async fn compute_inputs_sig_hashes(
    tx: Transaction,
    utxos: Vec<(OutPoint, TxOut)>,
    multisig_script: ScriptBuf,
) -> Result<Vec<secp256k1::Message>> {
    let leaf_hash = TapLeafHash::from_script(&multisig_script, LeafVersion::TapScript);
    let prevouts: Vec<_> = utxos.into_iter().map(|(_, txout)| txout).collect();
    let mut sighash_cache = SighashCache::new(&tx);

    (0..prevouts.len())
        .map(|i| {
            let sighash = sighash_cache.taproot_script_spend_signature_hash(
                i,
                &Prevouts::All(&prevouts),
                leaf_hash,
                TapSighashType::All,
            )?;
            Ok(secp256k1::Message::from_digest_slice(
                &sighash.as_raw_hash().as_byte_array().to_vec(),
            )?)
        })
        .collect()
}

fn sign_tx(kp: Keypair, messages: Vec<secp256k1::Message>) -> Result<Vec<Vec<u8>>> {
    let secp = Secp256k1::new();
    messages
        .into_iter()
        .map(|msg| {
            let mut sig = secp.sign_schnorr(&msg, &kp).as_ref().to_vec();
            sig.push(TapSighashType::All as u8);
            Ok(sig)
        })
        .collect()
}

fn build_witnesses(
    signers_public_keys: Vec<String>,
    total_utxos: usize,
    multisig_script: Vec<u8>,
    control_block: Vec<u8>,
    signatures_per_utxo: HashMap<String, Vec<String>>,
) -> anyhow::Result<Vec<Witness>> {
    (0..total_utxos)
        .map(|utxo_idx| {
            let mut witness = Witness::new();

            for public_key in signers_public_keys.iter().rev() {
                let sig = signatures_per_utxo
                    .get(public_key)
                    .and_then(|sigs| sigs.get(utxo_idx))
                    .map(|s| hex::decode(s))
                    .transpose()?;

                witness.push(sig.unwrap_or_default());
            }

            witness.push(&multisig_script);
            witness.push(&control_block);
            Ok(witness)
        })
        .collect::<anyhow::Result<Vec<_>>>()
}

fn finalize_tx(mut tx: Transaction, witnesses: Vec<Witness>) -> Result<Transaction> {
    for (i, wit) in witnesses.into_iter().enumerate() {
        tx.input[i].witness = wit;
    }
    Ok(tx)
}
