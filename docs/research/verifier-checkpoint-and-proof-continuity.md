# Verifier checkpoints and proof continuity

Status: research completed on 2026-10-14. [ADR
0007](../adr/0007-serialize-verifier-scans-and-cap-recovery.md) accepts the
immediate fix. Replaying history that was already skipped, and automatic repair,
remain separate decisions. This note describes source behavior and the evidence
behind the fix. It does not prove any live deployment or recovery.

## The situation

A Via verifier has to see every proof the sequencer writes onto Bitcoin. For
each L2 batch, the sequencer inscribes a proof that names the batch before it,
its parent. The verifier checks each proof and votes. When enough verifiers vote,
the batch is final, and the public API shows it as executed.

The verifier reads Bitcoin in two places:

- The reader (`VerifierBtcWatch`) opens each block and looks for proofs and
  votes. It records the last block it finished in the `via_btc_watch` row of
  `via_indexer_metadata`. This note calls that row the bookmark.
- The reorg detector (`ViaVerifierReorgDetector`) only follows block headers.
  It notices when Bitcoin replaces its newest blocks, a reorg, and records where
  the new chain starts. It is usually ahead of the reader.

After a reorg, the reverter (`ViaVerifierBlockReverter`) deletes what the
verifier learned from the removed blocks and moves the bookmark back, so the
reader reads those heights again.

## What happened on testnet

From early October 2026, no testnet batch after 264 was finalized. Batches 265,
266 and 267 were proven on Bitcoin, but neither verifier database contained
them, so no verifier ever voted. The proofs themselves were present on Bitcoin
and readable through the verifiers' own node. The verifiers had read past them
without seeing them.

The reverter caused the skip. At the deployed release, Via
`8a49f355bfe31720f21b8181db471243709194e5`, `ViaVerifierBlockReverter::reorg`
in `via_verifier/node/via_block_reverter/src/lib.rs` computed the new bookmark:

```rust
let l1_block_number_to_keep = l1_block_number - 1;
```

A few lines later, with deletion calls in between, it saved that value:

```rust
.update_last_processed_l1_block("via_btc_watch", l1_block_number_to_keep as u32)
```

`l1_block_number` is where the detector saw the new chain start. The reverter
set the bookmark to the block just before it, whatever the bookmark said before.
When the reader was behind the detector, that moved the bookmark forward.

A logged case from 13 October 2026 shows the effect. The reader had finished
block 156449. The detector saw a reorg starting at 156456. The reverter set the
bookmark to 156455. The next scan started at 156456, so blocks 156450 to 156455
were never read. The proof for batch 266 was in block 156451. Later that day a
jump from 156461 to 156467 skipped block 156463, which held batch 267's proof.
The proof for batch 265 was lost the same way around 3 October, but the logs
from that time were no longer available.

A second rule would have hidden the stall anyway. Had the reader read the
proofs for 266 and 267, it would have found their parent, batch 265, missing.
At the same release, `VerifierMessageProcessor::process_messages` then writes
an info log line, skips the proof, returns success, and lets the bookmark move
on. Either way, nothing raised an alert for 11 days.

There was also a race. The reader and the reverter run as separate tasks and
both wrote the bookmark. The reader compared the bookmark before and after its
scan and saved its result if the number had not changed. That check misses two
cases: recovery that leaves the number unchanged, and recovery that moves it
back while the reader later returns to the same number.

## The same line in Bitcoin Core is safe

Bitcoin Core's wallet does almost exactly what Via's reverter did. In
`CWallet::blockDisconnected`
([bitcoin/bitcoin@f13ae519de94](https://github.com/bitcoin/bitcoin/blob/f13ae519de94394314cd63a27924b0998ac65d45/src/wallet/wallet.cpp),
MIT), the function starts by taking the wallet lock:

```cpp
void CWallet::blockDisconnected(const interfaces::BlockInfo& block)
{
    assert(block.data);
    LOCK(cs_wallet);
```

After updating the transactions in the removed block, its last line is:

```cpp
    SetLastBlockProcessed(block.height - 1, *Assert(block.prev_hash));
```

It also sets its position to "the block before". The difference is who calls
it. The wallet only receives a disconnect for a block it has already processed,
in order, while holding its own wallet lock. So "the block before" is never
ahead of where the wallet really is. Via's detector reads ahead of the reader,
so the same arithmetic is unsafe there. The lesson Via takes is the rule behind
the line: only the reader's own finished work decides where it rewinds to.

## What other established software does

Each source was inspected at a fixed commit. No external code was copied.

- [graph-node](https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs),
  `rewind_or_truncate_with_conn`, MIT or Apache-2.0. The Graph's Ethereum
  indexer keeps its block pointer in Postgres, like Via. It moves the pointer
  and deletes the indexed data in one locked transaction, and its runner ignores
  a revert above its own pointer. This is the closest match. Via adds one thing:
  it holds the lock while fetching too, because several Via tasks share the
  bookmark.
- [PostgreSQL](https://github.com/postgres/postgres/blob/d733fcaf5cac6b88de9417f2b43e51490e36c7b1/src/backend/replication/slot.c),
  `ReplicationSlotAcquire`. A replication slot has one active owner at a time.
  Via takes the same one-owner rule for the bookmark.
- [Kafka](https://github.com/apache/kafka/blob/b9b0c909c6f01a2b50ff3a2478903be11312e35a/group-coordinator/src/main/java/org/apache/kafka/coordinator/group/modern/consumer/ConsumerGroup.java),
  `validateOffsetCommit`, Apache-2.0. A consumer that lost its turn cannot
  commit a late, stale position. Via takes the same idea through its lock.
  Kafka offsets name the next record, while Via's bookmark names the last
  finished block.
- [BDK](https://github.com/bitcoindevkit/bdk/blob/8b15e743e99a67be7d1d06a632b5f96bb7e365da/crates/bitcoind_rpc/src/lib.rs),
  `poll_once`, MIT or Apache-2.0. A Bitcoin wallet library that walks back to a
  block both sides still agree on, then reads forward again. It confirms the
  rule, but replacing Via's reader with it would not handle Via's inscriptions,
  proofs and withdrawals.
- [ZKsync Era](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_watch/src/event_processors/priority_ops.rs),
  `PriorityOpsEventProcessor::process_events`, MIT or Apache-2.0. Via's own
  upstream. Its watcher panics when a priority operation is missing, instead of
  skipping it. Via adapts this as a typed error and a pause, not a panic.
- [Optimism](https://github.com/ethereum-optimism/optimism/blob/16fc715df980e26f7933dbffdf4c97871aba90e0/op-node/rollup/derive/l1_traversal.go),
  `AdvanceL1Block`, MIT. A block with the wrong parent triggers a reset to an
  earlier consistent point. This is a model for later automatic repair.
- [Stacks](https://github.com/stacks-network/stacks-core/blob/82fd36fa5aed7b5569eced311830d80c01946caa/stackslib/src/burnchains/burnchain.rs),
  burnchain restart selection, GPL-3.0, ideas only. After a failure it restarts
  from a block hash its own store really holds. It fixed the same kind of bug,
  two stored positions drifting apart after a reorg.
- Arbitrum Nitro's inbox reader and Babylon's vigilante, both BSL-1.1, ideas
  only. They detect missing input and walk back or stop with a bounded number of
  retries.

## Options that were weighed

- Cap the bookmark only. This fixes the forward jump but leaves the reader and
  the reverter racing. Rejected.
- Cap the bookmark and save only if the number is unchanged. This misses the
  unchanged-number and back-and-return cases above. Rejected.
- One lock for every writer, with findings and bookmark saved together. This is
  the graph-node and Postgres pattern, and needs no schema change. Chosen.
- Store a recovery counter and a block hash with the bookmark. This would let
  the reader fetch outside the lock, but needs a migration. Kept for later.
- Replace the reader with BDK or another indexer. Too broad for this repair.
  Rejected.

Holding the lock while fetching has a cost. Slow Bitcoin calls delay recovery,
and on the main branch also withdrawal and vote writes, with no fixed upper
limit until the separate fix for blocking Bitcoin calls lands.

## Why a gap must stop the reader

A proof whose parent is missing means the verifier's history has a hole. If the
reader skips it, every later proof is also unusable, and nothing tells anyone.
ZKsync, Optimism, Nitro and Babylon all treat a gap as a reason to stop or to
repair, never to continue silently.

The fix raises a typed error, `ProofContinuityGap`. It records the expected and
observed batch numbers, both parent hashes, the proof transaction and the
Bitcoin height. The scan's writes are rolled back, the reader stays paused, and
the gauge `via_verifier_btc_watch_proof_continuity_blocked` becomes 1. Only
proofs accepted from the configured sequencer address reach this path, because
the inscription indexer filters `ProofDAReference` by that address. A proof
with the wrong parent at the expected batch number keeps its existing handling
as an invalid publication. A valid fork keeps its existing parent check.

## Replaying history that was already skipped

The fix stops new skips. It does not bring back proofs that were already
skipped. For that, the bookmark has to move back before batch 265's proof, so
the reader reads those blocks again. Two established tools show how to do such
a reset safely. [Bitcoin Core's
`rescanblockchain`](https://github.com/bitcoin/bitcoin/blob/f13ae519de94394314cd63a27924b0998ac65d45/src/wallet/rpc/transactions.cpp)
runs one rescan at a time, checks the blocks are still available and reports
what it really scanned. [Kafka's offset reset
tool](https://github.com/apache/kafka/blob/b9b0c909c6f01a2b50ff3a2478903be11312e35a/tools/src/main/java/org/apache/kafka/tools/consumer/group/ConsumerGroupCommand.java)
refuses while consumers are running and does a dry run by default.

Reading old blocks again is only safe if every message processor tolerates it.
Some do not obviously do so. At `8a49f355`, `withdrawals_dal.rs` can overwrite
an existing withdrawal's value and L2 hash, and
`via_protocol_versions_dal.rs`, `save_protocol_version`, can reset an executed
upgrade to pending. So a rehearsal on a separate copy of the database must come
first, covering every processor the replay reaches. No vote or finalization may
be written by hand. The replay needs its own approval.

## Open questions

- Automatic repair after a gap needs a verified consistent starting point,
  available history, proof that replay is safe, and a limit on how far back to
  search.
- Limiting the detector to blocks a set number of confirmations deep is separate
  work. The reader and the withdrawal checks already apply their own
  confirmation rules, so delaying the shared header table could apply a delay
  twice.
- This change does not repair history already skipped and proves no live
  deployment.
