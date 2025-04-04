use core::fmt;

use zksync_basic_types::{H160, H256};

use super::wallets::{AddressWallet, StateKeeper, TokenMultiplierSetter, Wallet};

#[derive(Default, Clone, PartialEq)]
pub struct ViaWallet {
    pub private_key: String,
}

impl fmt::Debug for ViaWallet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Secret")
            .field("private_key", &"******")
            .finish()
    }
}

impl ViaWallet {
    pub fn new(private_key: String) -> Self {
        Self { private_key }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViaWallets {
    pub state_keeper: Option<StateKeeper>,
    pub token_multiplier_setter: Option<TokenMultiplierSetter>,

    /// Via wallets
    pub btc_sender: Option<ViaWallet>,
    pub vote_operator: Option<ViaWallet>,
}

impl ViaWallets {
    pub fn for_tests() -> ViaWallets {
        ViaWallets {
            state_keeper: Some(StateKeeper {
                fee_account: AddressWallet::from_address(H160::repeat_byte(0x3)),
            }),
            token_multiplier_setter: Some(TokenMultiplierSetter {
                wallet: Wallet::from_private_key_bytes(H256::repeat_byte(0x4), None).unwrap(),
            }),
            btc_sender: Some(ViaWallet::new(String::from("pk"))),
            vote_operator: Some(ViaWallet::new(String::from("pk"))),
        }
    }
}
