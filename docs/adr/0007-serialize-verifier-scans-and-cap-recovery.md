---
status: accepted
date: 2026-10-14
---

# Reorg recovery never skips Bitcoin blocks the verifier has not read

## In short

A Via verifier reads Bitcoin block by block and saves how far it has read. When
Bitcoin replaced its newest blocks, the verifier's recovery code could move that
saved position forward, past blocks it had never read. On testnet this skipped
the proof for batch 265, and no batch was finalized for 11 days. This decision
makes recovery move the position only backwards, lets only one part of the
verifier change it at a time, and turns a missing proof into a loud pause
instead of a quiet skip.

## Background for a new reader

Via settles on Bitcoin. For each L2 batch, the sequencer writes a proof onto
Bitcoin as an inscription. Verifiers read those proofs, check them, and vote on
Bitcoin. A batch is final, shown as executed, once enough verifiers vote for it.

Two parts of the verifier read Bitcoin:

- The reader (`VerifierBtcWatch`) looks inside each block for proofs and votes.
  It stores the last block it finished in the `via_btc_watch` row of
  `via_indexer_metadata`. Think of it as a bookmark.
- The reorg detector (`ViaVerifierReorgDetector`) only follows block headers to
  notice a reorg. It can be many blocks ahead of the reader.

A reorg happens when Bitcoin replaces its newest few blocks with different ones.
Bitcoin testnet4 does this often. After a reorg, the reverter
(`ViaVerifierBlockReverter`) deletes what was learned from the removed blocks
and moves the bookmark back, so those heights are read again.

## What went wrong

The reverter set the bookmark to "the block before the reorg" without looking
at the bookmark's current value. That is only safe if the reader had already
read that far. Often it had not.

A logged example from 13 October 2026: the reader had finished block 156449.
The detector saw a reorg starting at block 156456. The reverter set the bookmark
to 156455. Blocks 156450 to 156455 were never read, and the proof for batch 266
was inside block 156451.

The proof for batch 265 was lost the same way around 3 October, and the proof
for batch 267 on 13 October, inside block 156463 after a jump from 156461 to
156467. Even read, 266 and 267 would not have helped: they name batch 265 as
their parent, and at that release a proof with a missing parent was skipped
with one info log line. No batch could be finalized, and nothing alerted
anyone, until the stall was noticed 11 days later.

A second problem sat in the same code. The reader and the reverter both wrote
the bookmark, from separate tasks. The reader could finish an old scan and save
its result on top of a recovery that had just moved the bookmark back.

## Decision

1. Recovery moves the bookmark back only. It sets the bookmark to whichever is
   lower: where the reader really is, or the block before the reorg. The real
   reorg height still decides which data is deleted and which withdrawals are
   invalidated.
2. One writer at a time. The reader, the detector, the startup code and the
   reverter all take the same PostgreSQL advisory lock (`lock_btc_watch`)
   before they read the bookmark or fetch blocks. The reader saves what it
   found and its new bookmark in one transaction, so a crash saves both or
   neither. Work that can invalidate withdrawals also takes the withdrawal
   invalidation lock, always in this order: bookmark lock, invalidation lock,
   wallet locks. Any other order can make two tasks wait for each other
   forever.
3. A missing proof stops the reader loudly. A proof whose parent batch is
   missing raises a typed error, `ProofContinuityGap`. The scan's writes are
   rolled back, the reader stays paused, and the gauge
   `via_verifier_btc_watch_proof_continuity_blocked` becomes 1, so an alert can
   page an operator within minutes.

## The main node

The main node had the same two flaws in its own reader, detector, reverter and
startup code. A skipped block there can hold a deposit that never reaches L2.
Since PR #404 it follows points 1 and 2 with its own lock, `BTC_WATCH_OWNER` in
the main-node DAL. That lock uses the two-integer key `(1464095563, 0)`.
PostgreSQL keeps two-integer and bigint advisory keys apart, so it never blocks
a verifier lock even on a shared database. Point 3 is verifier only, because
the main node's reader does not import proofs.

## Why this design

Point 1 follows Bitcoin Core's wallet, which rewinds from the last block it
processed itself, never from a height another component reports. Point 2
follows PostgreSQL replication slots and Kafka consumer groups, which each allow
only one owner of a consumer's position, and graph-node, an Ethereum indexer
that keeps its position in Postgres like Via and saves its pointer and its data
in one locked transaction. Point 3 follows Via's upstream, ZKsync Era, whose
watcher stops on a gap in its event sequence instead of skipping it. The
[research note](../research/verifier-checkpoint-and-proof-continuity.md) links
each source at a fixed commit, and shows why the same "block before" line is
safe in Bitcoin Core but not in Via.

Simpler fixes were rejected. Capping the bookmark alone leaves the two writers
racing. Checking that the bookmark still holds the same number before saving
fails when recovery leaves the number unchanged, or moves it back and the
reader returns to the same number. Replacing the reader with a library such as
BDK would not handle Via's inscriptions, proofs and withdrawals.

## Consequences

- The reader holds the bookmark lock while it fetches blocks from Bitcoin. Slow
  Bitcoin calls therefore delay recovery and, during main-branch scans,
  withdrawal and vote writes. There is no fixed upper limit on that wait until
  the separate fix for blocking Bitcoin calls lands.
- A paused reader stays paused until the process restarts. If the history is
  still missing after a restart, it pauses again. A restart alone fixes
  nothing. An operator must find and restore the missing proof history.
- Only proofs accepted from the configured sequencer address can trigger the
  pause, because the inscription indexer filters `ProofDAReference` by that
  address. A stranger cannot freeze the verifiers.
- Every process that writes one verifier database must run this version. An
  old binary does not take the new lock.

## Not decided here

This change does not repair history that was already skipped. Batches 265 to
267 still need a guarded recovery, with a rehearsal on a copy of the database
and its own approval. Automatic repair after a gap, limiting how close to the
chain tip the detector reads, the withdrawal signing size fix, and isolating
blocking Bitcoin calls are separate work. This decision authorizes no live
deployment, database change, vote or withdrawal.

The testnet hotfix branch applies the same three rules to the release testnet
runs (`8a49f355`), without importing main's newer withdrawal model. Its gap
error records the proof transaction and Bitcoin height but not the block hash,
so the shared Bitcoin client stays unchanged.
