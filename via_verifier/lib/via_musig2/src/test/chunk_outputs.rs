#[cfg(test)]
mod tests {
    use bitcoin::{Amount, ScriptBuf, TxOut};

    use crate::transaction_builder::TransactionBuilder;

    fn dummy_txout(val: u64) -> TxOut {
        TxOut {
            value: Amount::from_sat(val),
            script_pubkey: ScriptBuf::new(),
        }
    }

    #[test]
    fn test_chunk_outputs_even_split() {
        let outputs = vec![
            dummy_txout(1),
            dummy_txout(2),
            dummy_txout(3),
            dummy_txout(4),
        ];

        let chunks = TransactionBuilder::chunk_outputs(&outputs, 2);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 2);
        assert_eq!(chunks[1].len(), 2);
    }

    #[test]
    fn test_chunk_outputs_uneven_split() {
        let outputs = vec![
            dummy_txout(1),
            dummy_txout(2),
            dummy_txout(3),
            dummy_txout(4),
            dummy_txout(5),
        ];

        let chunks = TransactionBuilder::chunk_outputs(&outputs, 2);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 3); // 3 + 2
        assert_eq!(chunks[1].len(), 2);
    }

    #[test]
    fn test_chunk_outputs_one_chunk() {
        let outputs = vec![dummy_txout(1), dummy_txout(2)];

        let chunks = TransactionBuilder::chunk_outputs(&outputs, 1);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 2);
    }

    #[test]
    fn test_chunk_outputs_more_chunks_than_outputs() {
        let outputs = vec![dummy_txout(1), dummy_txout(2)];

        let chunks = TransactionBuilder::chunk_outputs(&outputs, 3);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 1);
        assert_eq!(chunks[1].len(), 1);
        assert_eq!(chunks[2].len(), 0);
    }

    #[test]
    fn test_chunk_outputs_empty_input() {
        let outputs = vec![];

        let chunks = TransactionBuilder::chunk_outputs(&outputs, 3);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.is_empty()));
    }
}
