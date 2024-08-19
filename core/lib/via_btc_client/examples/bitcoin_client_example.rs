#![cfg(feature = "regtest")]

use anyhow::Result;
use bitcoin::{
    absolute::LockTime, address::NetworkUnchecked, transaction::Version, Address, Amount, Network,
    OutPoint, Sequence, Transaction, TxIn, TxOut, Witness,
};
use via_btc_client::{
    client::BitcoinClient, regtest::BitcoinRegtest, traits::BitcoinSigner, BitcoinOps,
};

#[tokio::main]
async fn main() -> Result<()> {
    let context = BitcoinRegtest::new()?;
    let client = BitcoinClient::new("http://localhost:18443", "regtest").await?;

    let miner_address = context.get_miner_address()?;
    println!(
        "Balance of miner {miner_address}: {:?} SAT",
        client.get_balance(&miner_address).await?
    );

    let address = context.get_address();
    let private_key = context.get_private_key();
    println!("Testing account:");
    println!("Private key: {:?}", private_key.to_wif());
    println!("Address: {:?}", address);
    println!("Balance: {:?} SAT", client.get_balance(&address).await?);

    let random_address = "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080"
        .parse::<Address<NetworkUnchecked>>()?
        .require_network(Network::Regtest)?;
    println!("\nSending simple transfer to {}...\n", random_address);

    let amount_to_send = Amount::from_btc(20.0)?.to_sat();
    let fee = Amount::from_btc(0.00001)?.to_sat();
    let mut total_input = 0;
    let mut inputs = Vec::new();

    println!("Getting UTXOs of test address...");
    let utxos = client.fetch_utxos(&address).await?;
    for (i, (utxo, txid, vout)) in utxos.into_iter().enumerate() {
        println!("#{i} utxo:\nvout:{vout}\n{:?}\ntxid:{txid}\n", utxo);
        inputs.push(TxIn {
            previous_output: OutPoint::new(txid, vout),
            script_sig: bitcoin::ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        });
        total_input += utxo.value.to_sat();
        if total_input > amount_to_send {
            break;
        }
    }

    let mut outputs = vec![TxOut {
        value: Amount::from_sat(amount_to_send),
        script_pubkey: random_address.script_pubkey(),
    }];
    if total_input.saturating_sub(amount_to_send) > Amount::from_btc(0.0002)?.to_sat() {
        outputs.push(TxOut {
            value: Amount::from_sat(total_input.saturating_sub(amount_to_send) - fee),
            script_pubkey: address.script_pubkey(),
        })
    }
    println!("Outputs: {:?}", outputs);
    let unsigned_tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: inputs,
        output: outputs,
    };

    println!("Signing tx...");
    let signer = BasicSigner::new(private_key.to_wif().as_str(), client.get_rpc_client())?;
    let mut signed_tx = unsigned_tx;
    for i in 0..signed_tx.input.len() {
        let witness = signer.sign_ecdsa(&signed_tx, i).await?;
        signed_tx.input[i].witness = witness;
    }

    let serialized_tx = bitcoin::consensus::encode::serialize(&signed_tx);
    let txid = client
        .broadcast_signed_transaction(&hex::encode(serialized_tx))
        .await?;
    println!("\nTransaction sent: {:?}", txid);

    println!("Waiting for transaction to be mined...");
    tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    let i = client.check_tx_confirmation(&txid, 1).await?;
    println!("Is {txid} confirmed (1 confirmation): {i}");

    println!("Getting UTXOs of test address...");
    let utxos = client.fetch_utxos(&address).await?;
    for (i, (utxo, txid, vout)) in utxos.into_iter().enumerate() {
        println!("#{i} utxo:\nvout:{vout}\n{:?}\ntxid:{txid}\n", utxo);
    }

    Ok(())
}
