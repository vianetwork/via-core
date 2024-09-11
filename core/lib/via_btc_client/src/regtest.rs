use std::{
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use anyhow::Result;
use bitcoin::{address::NetworkUnchecked, Address, Network, PrivateKey};

const COMPOSE_FILE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/docker-compose-btc.yml"
);
const CLI_CONTAINER_NAME: &str = "resources-bitcoin-cli-1";

pub struct BitcoinRegtest {
    private_key: PrivateKey,
    address: Address,
}

impl BitcoinRegtest {
    pub fn new() -> Result<Self> {
        let regtest = Self {
            address: "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56"
                .parse::<Address<NetworkUnchecked>>()?
                .require_network(Network::Regtest)?,
            private_key: PrivateKey::from_wif(
                "cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R",
            )?,
        };
        regtest.setup()?;
        Ok(regtest)
    }

    fn setup(&self) -> Result<()> {
        self.run()?;
        thread::sleep(Duration::from_secs(10));
        Ok(())
    }

    fn run(&self) -> Result<()> {
        Command::new("docker")
            .args(["compose", "-f", COMPOSE_FILE_PATH, "up", "-d"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        Ok(())
    }

    pub fn get_miner_address(&self) -> Result<Address> {
        let output = Command::new("docker")
            .args(["logs", CLI_CONTAINER_NAME])
            .output()?;
        let stdout_utf8 = std::str::from_utf8(&output.stdout)?;
        if let Some(line) = stdout_utf8
            .lines()
            .find(|line| line.starts_with("Alice's address:"))
        {
            match line
                .split_once(": ")
                .map(|(_, addr)| addr.trim().to_string())
            {
                Some(address) => Ok(address
                    .parse::<Address<NetworkUnchecked>>()?
                    .require_network(Network::Regtest)?),
                None => Err(anyhow::anyhow!("Error while getting miner address")),
            }
        } else {
            Err(anyhow::anyhow!("Error while getting miner address"))
        }
    }

    fn stop(&self) -> Result<()> {
        Command::new("docker")
            .args(["compose", "-f", COMPOSE_FILE_PATH, "down", "--volumes"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        Ok(())
    }

    pub fn get_url(&self) -> String {
        "http://127.0.0.1:18443".to_string()
    }

    pub fn get_address(&self) -> &Address {
        &self.address
    }

    pub fn get_private_key(&self) -> &PrivateKey {
        &self.private_key
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
    use bitcoin::CompressedPublicKey;
    use bitcoincore_rpc::Auth;
    use secp256k1::Secp256k1;

    use super::*;
    use crate::{client::BitcoinClient, traits::BitcoinOps};

    #[tokio::test]
    async fn test_bitcoin_regtest() {
        let regtest = BitcoinRegtest::new().expect("Failed to create BitcoinRegtest");
        let client = BitcoinClient::new(
            &regtest.get_url(),
            Network::Regtest,
            Auth::UserPass("rpcuser".to_string(), "rpcpassword".to_string()),
        )
        .expect("Failed create rpc client");

        let block_count = client
            .fetch_block_height()
            .await
            .expect("Failed to get block count");
        assert!(block_count > 100);

        let address = regtest.get_address();
        let private_key = regtest.get_private_key();

        let secp = Secp256k1::new();
        let compressed_public_key = CompressedPublicKey::from_private_key(&secp, private_key)
            .expect("Failed to generate address from test private_key");
        let derived_address = Address::p2wpkh(&compressed_public_key, Network::Regtest);
        assert_eq!(*address, derived_address, "Address mismatch!");

        let balance = client
            .get_balance(address)
            .await
            .expect("Failed to get balance of test address");
        assert!(balance > 300000);
    }
}
