use std::{
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use anyhow::Result;
use bitcoin::{address::NetworkUnchecked, Address, Network, PrivateKey};

const COMPOSE_FILE_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/docker-compose-btc.yml");

pub struct BitcoinRegtest {
    compose_file: PathBuf,
    alice_private_key: Option<PrivateKey>,
    alice_address: Option<Address>,
}

impl BitcoinRegtest {
    pub fn new() -> Result<Self> {
        let compose_file = PathBuf::from(COMPOSE_FILE_PATH);
        let mut regtest = Self {
            compose_file,
            alice_private_key: None,
            alice_address: None,
        };
        regtest.setup()?;
        Ok(regtest)
    }

    fn setup(&mut self) -> Result<()> {
        self.run()?;
        thread::sleep(Duration::from_secs(10));
        let (address, private_key) = self.get_alice_info()?;
        self.alice_address = Some(address);
        self.alice_private_key = Some(private_key);
        Ok(())
    }

    fn run(&self) -> Result<()> {
        Command::new("docker")
            .args([
                "compose",
                "-f",
                self.compose_file.to_str().unwrap(),
                "up",
                "-d",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        Command::new("docker")
            .args([
                "compose",
                "-f",
                self.compose_file.to_str().unwrap(),
                "down",
                "--volumes",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        Ok(())
    }

    fn get_alice_info(&self) -> Result<(Address, PrivateKey)> {
        let output = Command::new("docker")
            .args(["logs", "tests-bitcoin-cli-1"])
            .output()?;

        let logs = String::from_utf8_lossy(&output.stdout);

        let address_str = logs
            .lines()
            .find(|line| line.starts_with("Alice's address:"))
            .and_then(|line| line.split(':').nth(1))
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("Alice's address not found in logs"))?;

        let private_key_str = logs
            .lines()
            .find(|line| line.starts_with("Alice's private key:"))
            .and_then(|line| line.split(':').nth(1))
            .map(str::trim)
            .ok_or_else(|| anyhow::anyhow!("Alice's private key not found in logs"))?;

        let address = address_str
            .parse::<Address<NetworkUnchecked>>()?
            .require_network(Network::Regtest)?;
        let private_key = PrivateKey::from_wif(private_key_str)?;

        Ok((address, private_key))
    }

    pub fn get_url(&self) -> String {
        "http://127.0.0.1:18443".to_string()
    }

    pub fn alice_address(&self) -> Result<&Address> {
        self.alice_address
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Alice's address not set"))
    }

    pub fn alice_private_key(&self) -> Result<&PrivateKey> {
        self.alice_private_key
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Alice's private key not set"))
    }
}

impl Drop for BitcoinRegtest {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            eprintln!("Failed to stop Bitcoin regtest: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use bitcoincore_rpc::{Auth, Client, RpcApi};

    use super::*;

    #[test]
    fn test_bitcoin_regtest() {
        let regtest = BitcoinRegtest::new().expect("Failed to create BitcoinRegtest");

        let url = regtest.get_url();
        let rpc = Client::new(
            &url,
            Auth::UserPass("rpcuser".to_string(), "rpcpassword".to_string()),
        )
        .expect("Failed to create RPC client");

        let balance = rpc.get_balance(None, None).expect("Failed to get balance");
        assert!(balance.to_btc() > 0.0);

        let block_count = rpc.get_block_count().expect("Failed to get block count");
        assert!(block_count > 100);

        let wallet_info = rpc.get_wallet_info().expect("Failed to get wallet info");
        assert_eq!(wallet_info.wallet_name, "Alice");

        println!(
            "Alice's private key: {}",
            regtest.alice_private_key().unwrap()
        );
        println!("Alice's address: {}", regtest.alice_address().unwrap());
    }
}
