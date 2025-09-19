# Sequencer

When a user deposits BTC on Bitcoin, the indexer waits for **6 confirmations** before minting tokens on VIA. This is the
recommended threshold to assume a block is final and safe from reorgs. However, in rare cases, the network can
experience deeper reorgs. In such situations, it is critical to have a resilient mechanism that can revert the impacted
batches. The sequencer implements a **reorg detector** to identify when a Bitcoin chain reorganization occurs and to
react accordingly. The **VIA reorg detector** is a background task that continuously monitors Bitcoin blocks for signs
of a reorg. When a reorg is detected, all components interacting with the Bitcoin node and Celestia immediately stop
posting data. At this point, a developer must manually execute a block revert using the
[`via_block_reverter`](core/bin/via_block_reverter) CLI tool before the system can safely resume operation.

## BtcWatch Re-indexing

**BtcWatch** is the entry point for transactions coming from Bitcoin (e.g., deposits and upgrades). If a reorg happens,
it is essential to **reindex transactions** to prevent double deposits and to ensure the system state can be
reconstructed correctly. A double deposit scenario could look like this:

- Alice deposits **1 BTC** in block 100.
- BtcWatch indexes Alice’s deposit and mints **1 BTC** on VIA.
- A reorg occurs at block 90.
- Alice’s transaction is pushed back into the Bitcoin mempool and later included in block 110.
- Without reindexing, BtcWatch would index block 110 and mint **another 1 BTC** for Alice.
- Result: **1 deposit, 2 mints (invalid state).**

To avoid this, BtcWatch reindexes the Bitcoin chain starting from the **last valid block**.

The sequencer maintains an internal mapping of L1 blocks in a table `<number, hash>`. During recovery, the last valid
block is compared against the Bitcoin client, and all data is rolled back in the database to this valid state.

The rollback process includes:

- Deleting all L1 transactions (deposits and upgrades).
- Resetting the indexer’s **“last processed block”** to the last valid block.

This ensures that deposits are reindexed consistently and only minted once.

## Handling L2 Transactions

For **L2 transactions**, the process is simpler:

- Transactions are pushed back into the mempool.
- The sequencer reprocesses them in upcoming batches. Some L2 transactions may fail during reprocessing. This typically
  happens if the transaction depends on a deposit that has not yet been reindexed (e.g., insufficient funds).

---

# Verifier

The verifier has two main layers responsible for detecting and handling reorgs:

- Reorg detector layer.
- Block reverter layer. Continuously monitors for Bitcoin chain reorg. When a reorg is detected, it notifies all
  components that a reorg is in progress. Affected components pause their operations until the reorg is resolved. The
  block reverter executes the rollback to the last valid block and restores the system state to ensure consistency. Once
  the rollback is complete, the verifier resumes normal operation, including batch processing, zero-knowledge (ZK)
  verification, and withdrawal handling.
- If the reorg impacted already finalized transactions, the verifier can revert the finalized batches and process from the last valid batch.