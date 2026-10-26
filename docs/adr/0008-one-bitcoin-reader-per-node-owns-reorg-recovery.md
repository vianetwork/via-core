---
status: accepted
date: 2026-10-24
---

# One Bitcoin reader per Via node owns ordering, reorg detection and recovery

Amended 2026-10-24: decisions 2 and 5–7 supersede the older four-group,
100-block and recovery descriptions below. Their historical rationale is retained.


## In short

Each Via node gets one Bitcoin reader. The reader reads blocks in order,
detects reorgs and recovers from them. Startup and any recovery left unfinished
by a crash are part of its lifecycle. The reader trusts one Bitcoin Core node
and starts recovery as soon as that node reports that the stored tip has left
its main chain. It stores the height, hash and parent hash of every block it
passes, and it finds the fork point by walking parent hashes back, at most 100
blocks. A deeper fork stops recovery loudly instead of guessing. This replaces
four separate tasks that hand work to each other through the database, the
shape that lost the proof for batch 265
([ADR 0007](0007-serialize-verifier-scans-and-cap-recovery.md)).

## Background for a new reader

Via nodes read Bitcoin to learn about deposits, proofs, votes, withdrawals,
bridge wallet changes and protocol upgrades. A reorg happens when Bitcoin
replaces its newest blocks with different ones. Everything a node learned from
the replaced blocks must then be undone and read again from the new blocks.

Today each node splits this work across four tasks, in two source trees
(`core/` for the main node and external node, `via_verifier/` for the verifier
and coordinator):

- The reader (`via_btc_watch` in each tree) parses blocks and stores its scan
  position.
- The reorg detector (`via_main_node_reorg_detector`, `via_reorg_detector`)
  follows block hashes to notice a reorg.
- The reverter (`via_block_reverter` in each tree) deletes what the replaced
  blocks produced and moves the scan position back.
- Startup setup (`via_node_storage_init`, `via_verifier_storage_init`)
  prepares the scan position when the process starts.

The node stores block hashes in `via_l1_blocks`, as `(number, hash)` pairs.
Since PR #365 that table is sparse. It has no row for many blocks and no parent
hashes.

## What went wrong

The four tasks share one scan position and change it at different times. On
the verifier the reverter moved the scan position past blocks the reader had
never read, and the proof for batch 265 was skipped. No batch was finalized for
11 days. ADR 0007 stopped the skip with one shared lock and a rule that
recovery only moves the position back. That keeps four writers in step by
convention, in two trees that must each get it right.

The same split shows up elsewhere:

- When `restart_indexing` is set, startup in either tree overwrites the scan
  position with a configured start height. That can move the position forward,
  past blocks the reader never read
  ([#432](https://github.com/vianetwork/via-core/issues/432)).
- The detectors re-fetch a fixed window of the last 100 heights and compare
  hashes by height. If the Bitcoin node's tip falls below the highest stored
  block, the fetch fails and the detector calls the result inconclusive until
  the chain grows back. If the fork is deeper than 100 blocks, the detector
  reports the window's first height as the fork point, with no error
  ([#444](https://github.com/vianetwork/via-core/issues/444)).
- Because `via_l1_blocks` is sparse and has no parent hashes, a node cannot
  prove from its own records where its chain and Bitcoin's chain diverge.

## Decision

1. **One Bitcoin reader per Via node, inside the node process.** It reads,
   detects reorgs, reverts and applies blocks in sequence. Startup and pending
   recovery run inside its lifecycle, not as separate tasks. A configured start
   height never moves a stored scan position forward without evidence.
2. **One reader position per node.** The highest committed canonical header
   is the reader position. Slow consumers keep their own durable completion,
   as [ADR 0019](0019-one-reader-position-with-trailing-consumers.md) specifies.
   A proof gap holds dependent work, not Bitcoin observation.

3. **Full block ancestry.** The reader stores `(height, hash, parent_hash)` for
   every block it advances over, including blocks with no Via messages. This
   is the history it uses to prove a fork point. `via_l1_blocks` cannot serve,
   because it is sparse.
4. **One authoritative Bitcoin node per component.** Each component reads from
   one Bitcoin Core node. When that node reports the stored tip with
   `confirmations: -1`, recovery starts immediately, even if no replacement
   block has arrived. A lower reported height alone, an unknown header or an
   unavailable node means the source is behind or the result is inconclusive.
   The reader then holds actions that need fresh evidence, deletes nothing and
   does not invent a fork point. A failover pool of Bitcoin nodes is not
   assumed. A pool would need its own handoff rule.
5. **A bounded hash walk finds the fork point.** On proved disconnection,
   commit the existing admission hold, then walk checked parents on both
   branches to the exact shared block before destructive undo. Count old-side
   removals only. No sparse-window edge is an ancestor.

6. **Inconclusive source, missing history and unsafe recovery stay distinct.**
   An unavailable/unknown source holds fresh authority and retries without
   deletes. Missing local ancestry, N or more old-side removals, crossing the
   durable confirmed frontier or unreconciled retained effects stops the node
   before suffix deletion, with last proved identities for diagnosis.

7. **Automatic recovery requires d < N and a preserved confirmed frontier.**
   N is 6 mainnet, 100 testnet4, 100 signet and 1 isolated regtest. The frontier
   stores a protected height and hash and never moves backward. This replaces
   the former independent 100-block search limit. At d >= N or a frontier
   crossing the node stops for diagnosis; no stopped-node repair command is
   promised here. [ADR 0021](0021-automatic-shallow-recovery-above-confirmed-frontier.md)
   gives the durable boundary.

## Considered options

**Separate tasks with an ordered handoff (today).** Rejected. This shape caused
the batch 265 skip, and it needs four writers kept in step across two trees.

**A separate reader service** that reads Bitcoin once and sends block and
rollback events to each node. Rejected. Delivering events cannot make
independent databases change atomically, and no studied project recovers
outside the process that holds the state. A service would also break the scan
transaction that ADR 0007 relies on, where effects and position commit
together ([#427](https://github.com/vianetwork/via-core/issues/427)).

**One scan position per node.** Rejected. One position cannot describe a proof
processor that is paused and a withdrawal observer that keeps going
([#427](https://github.com/vianetwork/via-core/issues/427),
[#447](https://github.com/vianetwork/via-core/issues/447)).

**Waiting for chain growth before a rollback**, the steady-state policy of LDK
and Chainlink LogPoller, and today's behavior on a failed fetch. Rejected. Via
would keep acting on blocks its source has already disowned.

**Rolling back on any lower height**, as Fulcrum does. Rejected. A lagging
source also reports a lower height. Alpen handles that case explicitly.

**A fixed comparison window (today).** Rejected. It reports the window edge as
a fork point it never proved, and it fails on a shorter chain.

**An unbounded walk, or a restart from genesis** as BDK's emitter does when it
finds no agreement point. Rejected. A pruning error or a source fault would
turn into a replay of the whole history, without any signal.

## Why this design

Every studied system with stateful consumers keeps reorg recovery in the
process that holds the state: Bitcoin Core `BaseIndex`, LDK
`lightning-block-sync`, Graph Node and Reth ExEx. The `-1` rule follows
Bitcoin Core's own chain membership calculation, `ComputeNextBlockAndDepth`,
the shrink handling in esplora-electrs and bindex, and the `confirmations < 0`
check in BDK's `Emitter`. The walk is LDK's `find_difference_from_header`,
which moves back the higher side, or both sides at equal height, until the
hashes match. A matching hash proves the shared block. A matching height does
not. The explicit stop follows ord's `Unrecoverable` outcome.
[ADR 0011](0011-shared-reader-in-via-reorg-with-copied-ldk-walk.md) records how
the walk is copied. The
[research note](../research/bitcoin-reorg-handling.md) links each source at a
fixed commit.

The limit of 100 keeps today's search cost. On testnet4, the deepest reorg in
about 25 months was 73 blocks and none reached 100
([#441](https://github.com/vianetwork/via-core/issues/441)). Those figures are
history, not a bound.

## Consequences

- The reader is the only code that moves a scan position. Recovery can no
  longer run on a schedule of its own.
- Each node stores one ancestry row for every Bitcoin block it reads.
- A shorter chain that the authoritative node has moved to is handled at once.
  It no longer waits until the chain grows past the stored height.
- A component whose Bitcoin node falls behind pauses actions that need fresh
  evidence. It does not roll back.
- A fork deeper than 100 blocks, or a gap in stored ancestry, pauses the node
  with the recovery barrier set until an operator acts. Each case has its own
  outcome, so an alert can name it.
- A component cannot switch to another Bitcoin node mid-run without a new
  decision on handoff.
- [#444](https://github.com/vianetwork/via-core/issues/444) still gets
  regression tests on today's detectors. The two fork algorithms do not
  coexist long term.

## Not decided here

- Which reverted state recovery may repair automatically, and what a read-only
  inspection looks like: decided later in
  [ADR 0014](0014-repair-only-reversible-provable-steps-and-inspect-read-only.md)
  ([#436](https://github.com/vianetwork/via-core/issues/436)).
- How each scan group's first position is proved during migration: decided
  later in [ADR 0010](0010-commit-each-scan-group-with-its-position.md).
- How a consumer in another process follows the reader. It stays open on the
  [reader map](https://github.com/vianetwork/via-core/issues/426).

This decision builds on
[ADR 0007](0007-serialize-verifier-scans-and-cap-recovery.md). It authorizes no
live deployment or database change. Decision tickets:
[#432](https://github.com/vianetwork/via-core/issues/432) (ownership),
[#448](https://github.com/vianetwork/via-core/issues/448) (source authority) and
[#438](https://github.com/vianetwork/via-core/issues/438) (fork search and its
limit).
