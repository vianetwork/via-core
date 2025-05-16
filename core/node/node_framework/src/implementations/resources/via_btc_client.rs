use std::sync::Arc;

use via_btc_client::client::BitcoinClient;

use crate::Resource;

#[derive(Debug, Clone)]
pub struct BtcClientResource(pub Arc<BitcoinClient>);

impl Resource for BtcClientResource {
    fn name() -> String {
        "btc_client_resource".into()
    }
}

impl From<BitcoinClient> for BtcClientResource {
    fn from(btc_client: BitcoinClient) -> Self {
        Self(Arc::new(btc_client))
    }
}
