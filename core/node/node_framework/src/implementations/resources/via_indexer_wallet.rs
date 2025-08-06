use std::sync::Arc;

use zksync_types::via_wallet::SystemWallets;

use crate::Resource;

#[derive(Debug, Clone)]
pub struct ViaSystemWalletsResource(pub Arc<SystemWallets>);

impl Resource for ViaSystemWalletsResource {
    fn name() -> String {
        "via_system_wallets_resource".into()
    }
}

impl From<SystemWallets> for ViaSystemWalletsResource {
    fn from(wallets: SystemWallets) -> Self {
        Self(Arc::new(wallets))
    }
}
