use crate::Resource;
use std::sync::Arc;
use via_btc_client::indexer::BitcoinInscriptionIndexer;

#[derive(Debug, Clone)]
pub struct BtcIndexerResource(pub Arc<BitcoinInscriptionIndexer>);

impl Resource for BtcIndexerResource {
    fn name() -> String {
        "btc_indexer_resource".into()
    }
}

impl From<BitcoinInscriptionIndexer> for BtcIndexerResource {
    fn from(btc_indexer: BitcoinInscriptionIndexer) -> Self {
        Self(Arc::new(btc_indexer))
    }
}
