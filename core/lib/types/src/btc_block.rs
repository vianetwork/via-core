use bitcoin::Txid;

#[derive(Clone)]
pub struct ViaBtcBlockDetails {
    pub number: i64,
    pub hash: Option<Vec<u8>>,
    pub commit_tx_id: Txid,
    pub reveal_tx_id: Txid,
}

impl std::fmt::Debug for ViaBtcBlockDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViaBtcBlockDetails")
            .field("number", &self.number)
            .field("commit_tx_id", &self.commit_tx_id.to_string())
            .field("reveal_tx_id", &self.reveal_tx_id.to_string())
            .finish()
    }
}
