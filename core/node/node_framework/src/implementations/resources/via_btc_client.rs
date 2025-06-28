use std::sync::Arc;

use via_btc_client::client::BitcoinClient;

use crate::Resource;

#[derive(Debug, Clone)]
pub struct BtcClientResource {
    pub default: Arc<BitcoinClient>,
    pub btc_sender: Option<Arc<BitcoinClient>>,
    pub verifier: Option<Arc<BitcoinClient>>,
    pub bridge: Option<Arc<BitcoinClient>>,
}

impl BtcClientResource {
    pub fn new(default: Arc<BitcoinClient>) -> Self {
        Self {
            default,
            btc_sender: None,
            verifier: None,
            bridge: None,
        }
    }

    pub fn with_btc_sender(self, btc_client: Arc<BitcoinClient>) -> Self {
        Self {
            default: self.default,
            btc_sender: Some(btc_client),
            verifier: self.verifier.clone(),
            bridge: self.bridge.clone(),
        }
    }

    pub fn with_verifier(self, btc_client: Arc<BitcoinClient>) -> Self {
        Self {
            default: self.default,
            btc_sender: self.btc_sender.clone(),
            verifier: Some(btc_client),
            bridge: self.bridge.clone(),
        }
    }

    pub fn with_bridge(self, btc_client: Arc<BitcoinClient>) -> Self {
        Self {
            default: self.default,
            btc_sender: self.btc_sender.clone(),
            verifier: self.verifier.clone(),
            bridge: Some(btc_client),
        }
    }
}

impl Resource for BtcClientResource {
    fn name() -> String {
        "btc_client_resource".into()
    }
}
