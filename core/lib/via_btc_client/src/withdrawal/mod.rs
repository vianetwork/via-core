struct WithdrawalBuilder {
    utxos: Vec<Utxo>,
    client: Arc<dyn BitcoinOps>,
}