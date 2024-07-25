use inquire::ui::{Attributes, Color, RenderConfig, StyleSheet, Styled};
use inquire::Text;
use std::str::FromStr;

use bitcoin::hashes::Hash;
use bitcoin::key::Keypair;
use bitcoin::key::{TapTweak, TweakedKeypair, UntweakedPublicKey};
use bitcoin::locktime::absolute;
use bitcoin::opcodes::{all, OP_FALSE};
use bitcoin::script::{Builder as ScriptBuilder, PushBytesBuf};
use bitcoin::secp256k1::{rand, Message, Secp256k1, SecretKey, Signing, Verification};
use bitcoin::sighash::{EcdsaSighashType, Prevouts, SighashCache, TapSighashType};

use bitcoin::taproot::{ControlBlock, LeafVersion, TaprootBuilder};
use bitcoin::{
    transaction, Address, Amount, CompressedPublicKey, Network, OutPoint, PrivateKey, ScriptBuf,
    Sequence, TapLeafHash, Transaction, TxIn, TxOut, Txid, WPubkeyHash, Witness,
};
use bitcoincore_rpc::RawTx;

use reqwest;
use serde_json::Value;

#[tokio::main]
async fn main() {
    let secp = Secp256k1::new();

    greeting();

    // get user input (private key(wif), the data to inscribe)
    let (sk, wpkh, sender_address, keypair, inscription_data) = get_user_input(&secp);
    let (internal_key, _parity) = keypair.x_only_public_key();

    println!("calling api to fetch all utxos for the given address...");
    let _utxos = get_utxos(&sender_address).await;
}

async fn get_utxos(addr: &Address) -> Vec<(OutPoint, TxOut)> {
    // call blockcypher api to get all utxos for the given address
    // https://api.blockcypher.com/v1/btc/test3/addrs/tb1qvxglm3jqsawtct65drunhe6uvat2k58dhfugqu/full?limit=200

    let url = format!(
        "https://api.blockcypher.com/v1/btc/test3/addrs/{}/full?limit=200",
        addr
    );
    let res = reqwest::get(&url).await.unwrap().text().await.unwrap();

    // Convert the response string to JSON
    let res_json: Value = serde_json::from_str(&res).unwrap();

    let balance = res_json.get("final_balance").unwrap().as_u64().unwrap();

    println!("your address balance is {:?} sats", balance);

    let txs = res_json.get("txs").unwrap().as_array().unwrap();

    println!("found {} transactions", txs.len());

    let mut utxos: Vec<(OutPoint, TxOut)> = vec![];

    for tx in txs {
        let txid = tx.get("hash").unwrap().as_str().unwrap();
        let txid = Txid::from_str(txid).unwrap();

        let vouts = tx.get("outputs").unwrap().as_array().unwrap();

        for (vout_index, vout) in vouts.iter().enumerate() {
            let mut isValid = true;

            if vout.get("spent_by").is_some() {
                isValid = false;
            }

            if vout.get("script_type").unwrap().as_str().unwrap() != "pay-to-witness-pubkey-hash" {
                println!("skipping non-p2wpkh output ...");
                isValid = false;
            }

            if !isValid {
                continue;
            }

            let out_point = OutPoint {
                txid,
                vout: vout_index as u32,
            };

            let value = vout.get("value").unwrap().as_u64().unwrap();
            let tx_out = TxOut {
                value: Amount::from_sat(value),
                script_pubkey: ScriptBuf::from_hex(vout.get("script").unwrap().as_str().unwrap())
                    .unwrap(),
            };

            utxos.push((out_point, tx_out));
        }
    }

    return utxos;
}

fn get_user_input<C: Signing>(
    secp: &Secp256k1<C>,
) -> (SecretKey, WPubkeyHash, Address, Keypair, String) {
    let mut render_config = RenderConfig::default();
    render_config.prompt_prefix = Styled::new(">").with_fg(Color::LightGreen);
    render_config.prompt = StyleSheet::new().with_fg(Color::LightMagenta);

    let user_wif_prv = Text::new("Enter your private key (WIF): ")
        .with_render_config(render_config)
        .prompt()
        .unwrap();

    let user_wif_prv = user_wif_prv.trim();

    let private_key = PrivateKey::from_wif(user_wif_prv).expect("Invalid Private Key WIF format");
    let sk = private_key.inner;

    let pk = bitcoin::PublicKey::new(sk.public_key(secp));
    let wpkh = pk.wpubkey_hash().expect("key is compressed");

    let compressed_pk = CompressedPublicKey::from_private_key(secp, &private_key).unwrap();
    let address = Address::p2wpkh(&compressed_pk, Network::Testnet);

    let keypair = Keypair::from_secret_key(secp, &sk);

    println!("Your address: {}", address);

    let multiline_content = r#"
    Please check the printed address above and make sure it is correct.
    if it's not press ctrl+c to exit and try again.

    Enter the data you want to inscribe (string or hexstring): 
    "#;
    let data = Text::new(multiline_content)
        .with_render_config(render_config)
        .prompt()
        .unwrap();

    let trimmed_data = data.trim().to_string();

    (sk, wpkh, address, keypair, trimmed_data)
}

fn greeting() {
    let content = r#"
    
    Welcome! 
    
    This is an CLI application that walks you through 
    inscribing arbitrary data into the Bitcoin testnet.

    **Please before continuing make sure you have done the following:**
    
    1- Install electrum wallet (https://electrum.org/#download)
    And run it in testnet mode with using the following command:
    Linux: electrum --testnet
    Mac: /Applications/Electrum.app/Contents/MacOS/run_electrum --testnet

    2- create a p2wpkh wallet (this is the default wallet type in electrum).
    
    3- get some testnet coins.
    
    Faucet Links:
        https://bitcoinfaucet.uo1.net/
        https://coinfaucet.eu/en/btc-testnet/
    
    when you are ready, press enter to continue...
    "#;

    let mut render_config = RenderConfig::default();
    render_config.prompt_prefix =
        Styled::new("***********************************************************")
            .with_fg(Color::LightRed);
    render_config.prompt = StyleSheet::new().with_fg(Color::Grey);

    let res = Text::new(content)
        .with_render_config(render_config)
        .prompt();

    match res {
        Ok(_) => {}
        Err(e) => println!("Error: {}", e),
    }
}
