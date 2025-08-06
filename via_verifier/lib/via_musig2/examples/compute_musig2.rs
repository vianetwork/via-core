use std::{fs::File, io::Write, str::FromStr};

use bitcoin::{
    blockdata::{opcodes::all::*, script::Builder},
    secp256k1::Secp256k1,
    Address, Network, PublicKey, ScriptBuf, XOnlyPublicKey,
};
use clap::Parser;
use musig2::KeyAggContext;
use serde::Serialize;

#[derive(Parser, Debug)]
#[command(
    author,
    version = "0.0.1",
    about = "Compute a musig 2 wallet with key hash as primary spending and the script path for governance spending hash"
)]
struct Args {
    /// Comma separated signer public keys (compressed hex, 33 bytes)
    #[arg(long)]
    signers: String,

    /// Comma separated governance x-only public keys (hex, N keys)
    #[arg(long)]
    governance_keys: String,

    /// Governance threshold M (e.g. 2 for 2-of-N)
    #[arg(long)]
    threshold: usize,

    /// Bitcoin network (regtest, testnet, mainnet, signet)
    #[arg(long, default_value = "regtest")]
    network: String,

    /// Output JSON file path
    #[arg(long, default_value = "wallet_info.json")]
    output: String,
}

#[derive(Serialize)]
struct ExportData {
    aggregated_internal_key: String,
    governance_script_hex: String,
    taproot_output_key: String,
    merkle_root: Option<String>,
    taproot_address: String,
    threshold: usize,
    total_governance_keys: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let secp = Secp256k1::verification_only();

    // --- Parse signer full pubkeys (MuSig2 aggregation) ---
    let signer_hex: Vec<&str> = args.signers.split(',').collect();
    let mut musig_pubkeys = Vec::new();

    for hex in signer_hex {
        // let bytes = Vec::from_hex(hex.trim())?;
        let pk = musig2::secp256k1::PublicKey::from_str(&hex.trim())?;
        musig_pubkeys.push(pk);
    }

    if musig_pubkeys.is_empty() {
        anyhow::bail!("Must provide at least one signer key");
    }

    // Aggregate MuSig2 key
    let mut musig_key_agg_cache = KeyAggContext::new(musig_pubkeys)?;
    let agg_pubkey = musig_key_agg_cache.aggregated_pubkey::<secp256k1_musig2::PublicKey>();
    let (xonly_agg_key, _) = agg_pubkey.x_only_public_key();
    let internal_key = XOnlyPublicKey::from_slice(&xonly_agg_key.serialize())?;

    // --- Governance pubkeys (x-only) ---
    let gov_hex: Vec<&str> = args.governance_keys.split(',').collect();
    let mut gov_xonly = Vec::new();
    for hex in gov_hex {
        let xonly = PublicKey::from_str(&hex.trim())?
            .inner
            .x_only_public_key()
            .0;
        gov_xonly.push(xonly);
    }

    let n = gov_xonly.len();
    let m = args.threshold;

    if n == 0 {
        anyhow::bail!("Must provide at least one governance key");
    }
    if m == 0 || m > n {
        anyhow::bail!(format!(
            "Invalid threshold: {} (must be between 1 and N={})",
            m, n
        ));
    }

    // --- Build generic M-of-N Taproot Schnorr multisig script ---
    let mut builder = Builder::new();

    for (i, key) in gov_xonly.iter().enumerate() {
        if i == 0 {
            builder = builder.push_x_only_key(key).push_opcode(OP_CHECKSIG);
        } else {
            builder = builder.push_x_only_key(key).push_opcode(OP_CHECKSIGADD);
        }
    }

    let multisig_script = builder
        .push_int(m as i64)
        .push_opcode(OP_NUMEQUAL)
        .into_script();

    let spend_info = bitcoin::taproot::TaprootBuilder::new()
        .add_leaf(0, ScriptBuf::from(multisig_script.clone()))?
        .finalize(&secp, internal_key)
        .unwrap();

    let taproot_output_key = spend_info.output_key();
    let net = match args.network.as_str() {
        "mainnet" => Network::Bitcoin,
        "testnet" => Network::Testnet,
        "signet" => Network::Signet,
        _ => Network::Regtest,
    };
    let taproot_address = Address::p2tr_tweaked(taproot_output_key, net);

    // --- Export JSON file ---
    let data = ExportData {
        aggregated_internal_key: internal_key.to_string(),
        governance_script_hex: multisig_script.to_hex_string(),
        taproot_output_key: taproot_output_key.to_string(),
        merkle_root: spend_info.merkle_root().map(|h| h.to_string()),
        taproot_address: taproot_address.to_string(),
        threshold: m,
        total_governance_keys: n,
    };

    let json = serde_json::to_string_pretty(&data)?;
    let mut file = File::create(&args.output)?;
    file.write_all(json.as_bytes())?;

    println!("Exported wallet info to {}", args.output);

    Ok(())
}
