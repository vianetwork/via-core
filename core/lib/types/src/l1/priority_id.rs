#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ViaPriorityOpId(pub u64);

/// VIA priorityId
/// 28 bits for block (268M blocks = 5,100 years when block time = 10min)
/// 20 bits for tx_index (1M transactions per block)
/// 16 bits for vout (65k outputs per transaction)
impl ViaPriorityOpId {
    // Constants for bit field sizes
    const BLOCK_BITS: u32 = 28;
    const TX_INDEX_BITS: u32 = 20;
    const VOUT_BITS: u32 = 16;

    // Bit masks
    const BLOCK_MASK: u64 = (1u64 << Self::BLOCK_BITS) - 1; // 0xFFFFFFF
    const TX_INDEX_MASK: u64 = (1u64 << Self::TX_INDEX_BITS) - 1; // 0xFFFFF
    const VOUT_MASK: u64 = (1u64 << Self::VOUT_BITS) - 1; // 0xFFFF

    // Bit positions
    const TX_INDEX_SHIFT: u32 = Self::VOUT_BITS;
    const BLOCK_SHIFT: u32 = Self::TX_INDEX_BITS + Self::VOUT_BITS;

    // Maximum values
    pub const MAX_BLOCK_NUMBER: u64 = Self::BLOCK_MASK;
    pub const MAX_TX_INDEX: u64 = Self::TX_INDEX_MASK;
    pub const MAX_VOUT: u64 = Self::VOUT_MASK;

    /// Creates a new PriorityOpId from components
    pub fn new(block_number: u64, tx_index: u64, vout: u64) -> Self {
        debug_assert!(
            block_number <= Self::MAX_BLOCK_NUMBER,
            "Block number {} exceeds maximum {}",
            block_number,
            Self::MAX_BLOCK_NUMBER
        );
        debug_assert!(
            tx_index <= Self::MAX_TX_INDEX,
            "TX index {} exceeds maximum {}",
            tx_index,
            Self::MAX_TX_INDEX
        );
        debug_assert!(
            vout <= Self::MAX_VOUT,
            "VOut {} exceeds maximum {}",
            vout,
            Self::MAX_VOUT
        );

        Self(
            ((block_number & Self::BLOCK_MASK) << Self::BLOCK_SHIFT)
                | ((tx_index & Self::TX_INDEX_MASK) << Self::TX_INDEX_SHIFT)
                | (vout & Self::VOUT_MASK),
        )
    }

    /// Extracts the block number
    pub fn block_number(&self) -> u64 {
        (self.0 >> Self::BLOCK_SHIFT) & Self::BLOCK_MASK
    }

    /// Extracts the transaction index
    pub fn tx_index(&self) -> u64 {
        (self.0 >> Self::TX_INDEX_SHIFT) & Self::TX_INDEX_MASK
    }

    /// Extracts the output vout
    pub fn vout(&self) -> u64 {
        self.0 & Self::VOUT_MASK
    }

    /// Gets the raw u64 value
    pub fn raw(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_id_creation_and_extraction() {
        let block_number = 1_000_000u64;
        let tx_index = 500_000u64;
        let vout = 42u64;

        let priority_id = ViaPriorityOpId::new(block_number, tx_index, vout);

        assert_eq!(priority_id.block_number(), block_number);
        assert_eq!(priority_id.tx_index(), tx_index);
        assert_eq!(priority_id.vout(), vout);
    }

    #[test]
    fn test_ordering_by_block_number() {
        let id1 = ViaPriorityOpId::new(100, 999, 999);
        let id2 = ViaPriorityOpId::new(101, 0, 0);

        assert!(id2 > id1, "Higher block number should have higher priority");
    }

    #[test]
    fn test_ordering_by_tx_index() {
        let id1 = ViaPriorityOpId::new(100, 500, 999);
        let id2 = ViaPriorityOpId::new(101, 501, 0);

        assert!(
            id2 > id1,
            "Higher tx_index should have higher priority in same block"
        );
    }

    #[test]
    fn test_ordering_by_vout() {
        let id1 = ViaPriorityOpId::new(100, 500, 42);
        let id2 = ViaPriorityOpId::new(100, 500, 43);

        assert!(
            id2 > id1,
            "Higher vout should have higher priority in same tx"
        );
    }

    #[test]
    fn test_maximum_values() {
        let priority_id = ViaPriorityOpId::new(
            ViaPriorityOpId::MAX_BLOCK_NUMBER,
            ViaPriorityOpId::MAX_TX_INDEX,
            ViaPriorityOpId::MAX_VOUT,
        );

        assert_eq!(
            priority_id.block_number(),
            ViaPriorityOpId::MAX_BLOCK_NUMBER
        );
        assert_eq!(priority_id.tx_index(), ViaPriorityOpId::MAX_TX_INDEX);
        assert_eq!(priority_id.vout(), ViaPriorityOpId::MAX_VOUT);
    }

    #[test]
    fn test_minimum_values() {
        let priority_id = ViaPriorityOpId::new(0, 0, 0);

        assert_eq!(priority_id.block_number(), 0);
        assert_eq!(priority_id.tx_index(), 0);
        assert_eq!(priority_id.vout(), 0);
        assert_eq!(priority_id.raw(), 0);
    }

    #[test]
    fn test_bit_field_isolation() {
        // Test that setting one field doesn't affect others
        let id1 = ViaPriorityOpId::new(0xFFFFFFF, 0, 0);
        let id2 = ViaPriorityOpId::new(0, 0xFFFFF, 0);
        let id3 = ViaPriorityOpId::new(0, 0, 0xFFFF);

        assert_eq!(id1.block_number(), 0xFFFFFFF);
        assert_eq!(id1.tx_index(), 0);
        assert_eq!(id1.vout(), 0);

        assert_eq!(id2.block_number(), 0);
        assert_eq!(id2.tx_index(), 0xFFFFF);
        assert_eq!(id2.vout(), 0);

        assert_eq!(id3.block_number(), 0);
        assert_eq!(id3.tx_index(), 0);
        assert_eq!(id3.vout(), 0xFFFF);
    }

    #[test]
    fn test_round_trip() {
        let test_cases = vec![
            (0, 0, 0),
            (1, 2, 3),
            (1000000, 500000, 42),
            (
                ViaPriorityOpId::MAX_BLOCK_NUMBER,
                ViaPriorityOpId::MAX_TX_INDEX,
                ViaPriorityOpId::MAX_VOUT,
            ),
        ];

        for (block, tx, vout) in test_cases {
            let priority_id = ViaPriorityOpId::new(block, tx, vout);
            assert_eq!(priority_id.block_number(), block);
            assert_eq!(priority_id.tx_index(), tx);
            assert_eq!(priority_id.vout(), vout);
        }
    }

    #[test]
    fn test_constants() {
        assert_eq!(ViaPriorityOpId::MAX_BLOCK_NUMBER, 0xFFFFFFF);
        assert_eq!(ViaPriorityOpId::MAX_TX_INDEX, 0xFFFFF);
        assert_eq!(ViaPriorityOpId::MAX_VOUT, 0xFFFF);
    }

    #[test]
    #[should_panic]
    fn test_debug_assert_block_overflow() {
        // This should panic in debug mode
        ViaPriorityOpId::new(ViaPriorityOpId::MAX_BLOCK_NUMBER + 1, 0, 0);
    }

    #[test]
    #[should_panic]
    fn test_debug_assert_tx_overflow() {
        // This should panic in debug mode
        ViaPriorityOpId::new(0, ViaPriorityOpId::MAX_TX_INDEX + 1, 0);
    }

    #[test]
    #[should_panic]
    fn test_debug_assert_vout_overflow() {
        // This should panic in debug mode
        ViaPriorityOpId::new(0, 0, ViaPriorityOpId::MAX_VOUT + 1);
    }
}
