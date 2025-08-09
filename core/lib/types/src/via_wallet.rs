use std::{collections::HashMap, str::FromStr};

use bitcoin::{hashes::Hash, Address, Txid};
use serde::{Deserialize, Serialize};

use crate::via_bootstrap::BootstrapState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalletRole {
    Sequencer,
    Verifier,
    Bridge,
    Gov,
}

impl WalletRole {
    pub fn all_roles() -> &'static [WalletRole] {
        &[
            WalletRole::Sequencer,
            WalletRole::Verifier,
            WalletRole::Bridge,
            WalletRole::Gov,
        ]
    }
}

impl std::fmt::Display for WalletRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            WalletRole::Sequencer => "Sequencer",
            WalletRole::Verifier => "Verifier",
            WalletRole::Bridge => "Bridge",
            WalletRole::Gov => "Gov",
        };
        write!(f, "{}", s)
    }
}

impl FromStr for WalletRole {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "sequencer" => Ok(WalletRole::Sequencer),
            "verifier" => Ok(WalletRole::Verifier),
            "bridge" => Ok(WalletRole::Bridge),
            "gov" => Ok(WalletRole::Gov),
            _ => Err(anyhow::anyhow!("Unknown role: {}", s)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct WalletInfo {
    pub addresses: Vec<Address>,
    pub txid: Txid,
}

impl Default for WalletInfo {
    fn default() -> Self {
        Self {
            addresses: vec![],
            txid: Txid::all_zeros(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SystemWallets {
    pub sequencer: Address,
    pub verifiers: Vec<Address>,
    pub governance: Address,
    pub bridge: Address,
}

impl SystemWallets {
    pub fn is_valid_bridge_address(&self, bridge_address: Address) -> anyhow::Result<()> {
        if !self.verifiers.contains(&bridge_address) {
            anyhow::bail!(
                "bridge address mismatch, expected one of {:?}, found {}",
                &self.bridge,
                bridge_address
            );
        }
        Ok(())
    }

    pub fn is_valid_verifier_address(&self, verifier: Address) -> anyhow::Result<()> {
        if !self.verifiers.contains(&verifier) {
            anyhow::bail!(
                "Verifier address not found in the verifiers set, expected one of {:?}, found {}",
                &self.verifiers,
                verifier
            );
        }
        Ok(())
    }

    pub fn is_valid_sequencer_address(&self, sequencer: Address) -> anyhow::Result<()> {
        if self.sequencer != sequencer {
            anyhow::bail!(
                "Sequencer address mismatch, expected {}, found {}",
                self.sequencer.to_string(),
                sequencer.to_string()
            );
        }
        Ok(())
    }
}
impl TryFrom<HashMap<String, String>> for SystemWallets {
    type Error = anyhow::Error;

    fn try_from(role_address: HashMap<String, String>) -> Result<Self, Self::Error> {
        let sequencer = parse_str_wallet(WalletRole::Sequencer, &role_address)?;
        let bridge = parse_str_wallet(WalletRole::Bridge, &role_address)?;
        let governance = parse_str_wallet(WalletRole::Gov, &role_address)?;
        let verifiers = parse_str_wallets(WalletRole::Verifier, &role_address)?;

        Ok(SystemWallets {
            sequencer,
            bridge,
            governance,
            verifiers,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct SystemWalletsDetails(pub HashMap<WalletRole, WalletInfo>);

impl TryFrom<&BootstrapState> for SystemWalletsDetails {
    type Error = anyhow::Error;

    fn try_from(state: &BootstrapState) -> Result<Self, Self::Error> {
        let mut map = HashMap::new();

        let wallets = state
            .wallets
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Wallets missing"))?;

        if let Some(txid) = &state.sequencer_proposal_tx_id {
            map.insert(
                WalletRole::Sequencer,
                WalletInfo {
                    addresses: vec![wallets.sequencer.clone()],
                    txid: *txid,
                },
            );
        }

        if let Some(txid) = &state.bootstrap_tx_id {
            map.insert(
                WalletRole::Bridge,
                WalletInfo {
                    addresses: vec![wallets.bridge.clone()],
                    txid: *txid,
                },
            );

            map.insert(
                WalletRole::Gov,
                WalletInfo {
                    addresses: vec![wallets.governance.clone()],
                    txid: *txid,
                },
            );

            map.insert(
                WalletRole::Verifier,
                WalletInfo {
                    addresses: wallets.verifiers.clone(),
                    txid: *txid,
                },
            );
        }

        Ok(SystemWalletsDetails(map))
    }
}

impl TryFrom<SystemWallets> for SystemWalletsDetails {
    type Error = anyhow::Error;

    fn try_from(wallets: SystemWallets) -> Result<Self, Self::Error> {
        let mut system_wallet_map = SystemWalletsDetails::default();

        system_wallet_map.0.insert(
            WalletRole::Sequencer,
            WalletInfo {
                addresses: vec![wallets.sequencer],
                txid: Txid::all_zeros(),
            },
        );

        system_wallet_map.0.insert(
            WalletRole::Bridge,
            WalletInfo {
                addresses: vec![wallets.bridge],
                txid: Txid::all_zeros(),
            },
        );

        system_wallet_map.0.insert(
            WalletRole::Verifier,
            WalletInfo {
                addresses: wallets.verifiers,
                txid: Txid::all_zeros(),
            },
        );

        system_wallet_map.0.insert(
            WalletRole::Gov,
            WalletInfo {
                addresses: vec![wallets.governance],
                txid: Txid::all_zeros(),
            },
        );

        Ok(system_wallet_map)
    }
}

fn parse_str_wallet(
    role: WalletRole,
    role_address: &HashMap<String, String>,
) -> anyhow::Result<Address> {
    let address_str = role_address
        .get(&role.to_string())
        .ok_or_else(|| anyhow::anyhow!("Missing address for role {}", role))?;

    Address::from_str(address_str.trim())
        .map(|a| a.assume_checked())
        .map_err(|e| anyhow::anyhow!("Invalid address '{}' for role {}: {}", address_str, role, e))
}

fn parse_str_wallets(
    role: WalletRole,
    role_address: &HashMap<String, String>,
) -> anyhow::Result<Vec<Address>> {
    let addresses_str = role_address
        .get(&role.to_string())
        .ok_or_else(|| anyhow::anyhow!("Missing addresses for role {}", role))?;

    let mut addresses = Vec::new();

    for address_str in addresses_str.split(',') {
        let addr = Address::from_str(address_str.trim())
            .map(|a| a.assume_checked())
            .map_err(|e| {
                anyhow::anyhow!("Invalid address '{}' for role {}: {}", address_str, role, e)
            })?;
        addresses.push(addr);
    }

    Ok(addresses)
}
