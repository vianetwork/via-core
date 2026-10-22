---
status: accepted
date: 2026-10-22
---

# Each scan group commits its effects with its own scan position

## In short

The verifier's Bitcoin reader feeds four scan groups: configuration, deposits,
proofs with votes, and withdrawal observations. Each group saves what it
learned from a block and its own `(height, hash)` scan position in the same
transaction, both when it moves forward and when recovery moves it back. The
existing `via_l1_reorg` record stays the committed recovery barrier from the
first destructive recovery step until every affected store agrees. Work that
runs outside the reader, such as a zk verdict, holds the source lock through
its final commit and checks all of its inputs again before it saves. The
signing messages do not change. Nodes upgrade by stopping, migrating and
restarting.

## Background for a new reader

[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md) gives each
Via node one Bitcoin reader and one scan position per group of processors that
must commit together. This decision names those groups for the verifier and
says how recovery and outside work stay consistent with them.

Today the verifier keeps one scan position, a height in the `via_btc_watch`
row of `via_indexer_metadata`. Since
[ADR 0007](0007-serialize-verifier-scans-and-cap-recovery.md), one scan saves
all its effects and that position in one transaction, under the
`lock_btc_watch` lock. When a proof's parent batch is missing, the reader stops
with `ProofContinuityGap`, and every other kind of Bitcoin message stops with
it.

When the reorg detector finds a reorg, it writes a row into `via_l1_reorg`.
The reverter deletes the row when it finishes. The sender, the aggregator, the
zk verifier, the coordinator and the DA dispatcher read that row and pause
while it exists.

Some work is driven by stored records, not by blocks. The zk verifier is one
example. It reads a stored proof, verifies it and saves a verdict. It does not
take the reader's lock. Its only guard is a reorg check just before its commit
([#428](https://github.com/vianetwork/via-core/issues/428)).

## What went wrong

- With one scan position, a gap in proofs also stopped deposits, bridge wallet
  changes and withdrawal observations, which do not depend on proofs.
- A zk verdict could land on a proof row that recovery had deleted and the
  reader had inserted again. Under PostgreSQL's Read Committed isolation, a
  check inside the transaction does not stop recovery from committing between
  that check and the verdict's own commit
  ([#434](https://github.com/vianetwork/via-core/issues/434)).
- The scan position is a height only. It cannot show which branch of the chain
  the reader processed, so recovery cannot check it against the chain.

## Decision

1. **Four scan groups on the verifier.** Configuration (bridge wallets and
   protocol upgrades), deposits, proofs with votes, and withdrawal
   observations. Each group commits its effects and its own `(height, hash)`
   scan position together, forwards and on revert. Configuration is a
   prerequisite for the other groups, not a second ordering owner. Applying zk
   results, reconciling withdrawals and sender work stay jobs driven by stored
   records. The main node and the external node instantiate only the groups
   that apply to them.
2. **`via_l1_reorg` stays the committed recovery barrier.** Recovery sets it
   before its first destructive step and keeps it across every recovery
   transaction. A final protected transaction clears it once the affected
   stores and consumers agree. A proof gap stays a separate stop that applies
   to proofs only.
3. **Stale work is fenced by lock and full revalidation.** The final write of
   outside work takes the owner and source gate (`lock_source`), checks the
   complete verdict inputs again and holds that coordination through commit.
   The inputs are the immutable proof inclusion, the row incarnation, the exact
   deposit selection, the protocol version and the applicable verifier set. A
   persisted recovery generation counter is added only if complete input
   revalidation cannot be established.
4. **Withdrawal observation continues during a proof pause.** It keeps
   recording evidence and conservative holds. It reconciles only sources that
   already satisfy the existing authority and maturity rules.
5. **The signing messages do not change.** Each node rejects stale work at its
   own fenced admission point.
6. **The barrier stops new durable authority, not work in flight.** It takes
   effect at the final fenced save. Work already dispatched stays recorded in
   the existing send and signing history. Recovery does not wait for in-flight
   work to drain.

**Migration.** Old and new writers do not run side by side. To upgrade, stop
every writer that shares a database, migrate and reconcile, then restart on the
new contract. Rolling coexistence is not a requirement.

**First positions after migration.** Each scan group starts from a position
that retained evidence proves. What can be proved migrates. A group that
cannot be proved stops the migration, and an operator chooses an explicit
baseline, such as rebuilding the group's reversible state from a proved block.
Today's hash at an old height is never taken as proof, because it does not
show which block produced a row. Executed, signed or paid state is never
inferred. Most legacy rows store only a height, so testnet4 is expected to
need that operator baseline once.

**Refinements.** These were recorded on #434 from the research in
[#459](https://github.com/vianetwork/via-core/issues/459), without a new
decision:

- The zk verdict's final write is keyed by the proof row's `id`, captured when
  the work starts. That `id` is an identity value that is never reused. Today
  `verify_votable_transaction` updates by the natural key
  `(l1_batch_number, proof_reveal_tx_id)`
  ([`via_votes_dal.rs`, lines 417 to 441](https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/lib/verifier_dal/src/via_votes_dal.rs#L417-L441)),
  so a verdict can land on a row that recovery deleted and the reader inserted
  again.
- Each group's scan position advances and rewinds as a compare-and-swap on
  `(height, hash)`, in the same transaction as the group's effects. If the
  stored position is not the expected one, the transaction fails and the
  group keeps its position.

## Considered options

**Two scan groups.** Rejected. A proof gap would still stop deposits and
configuration.

**One scan group per message type.** Rejected. It needs buffering of pending
events across groups, and nothing needs that.

**A new record for recovery state.** Rejected. It adds code and carries the
same obligations as `via_l1_reorg`.

**A pause held only as a lock or in memory.** Rejected. It does not survive
across recovery steps or between processes.

**Checking a recovery flag or generation counter without a lock.** Rejected.
Read Committed allows recovery to commit between the check and the write.

**Freezing withdrawal observation with proofs.** Rejected. It misses payments.

**Reconciling withdrawals against the latest state.** Rejected. It mixes
configurations from different blocks.

**Infer a legacy position from today's chain.** Rejected. Today's hash at the
stored height looks like proof but does not show which block a row came from.

**Rebuild every group from Via's first block.** Not chosen as the rule. It
works for reversible state but cannot rebuild executed L2 state. It remains
one baseline an operator may choose.

## Why this design

Graph Node keeps effects and position per deployment and commits them together,
forwards and on undo. Via's own `lock_source` already has the needed shape: a
shared lock for writers that depend on a source, and an exclusive lock that the
reverter holds while it deletes that source. The rbitcoin wallet fixed the same
race between a background writer and a reorg by copying the writer's input
under the reorg's lock. The
[research note](../research/bitcoin-reorg-handling.md) links each source at a
fixed commit.

For the refinements, Marten advances a projection's progress with a
compare-and-swap on the previous position, in the same transaction as the
projected writes
([`UpdateProjectionProgress.cs`](https://github.com/JasperFx/marten/blob/7045d09bb0afef1b4f34c4124e3c0852e8e7e03d/src/Marten/Events/Daemon/Progress/UpdateProjectionProgress.cs#L43-L91),
C#, MIT). PostgreSQL logical replication writes the source position into the
same commit as the applied rows
([`worker.c`](https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/backend/replication/logical/worker.c#L2543-L2582),
C, PostgreSQL licence). Via adapts both with `(height, hash)` in place of a
sequence number, because a reorg can replace the block at a height. Via's
rewind is also a compare-and-swap, unlike Marten's rewind, which overwrites the
position without a check. Temporal and etcd reject late work by comparing an
identity that a re-created record cannot share, which is the role of the
captured row `id`.

For migration, `pg_rewind` refuses to run when it cannot find a common point
between two histories
([`pg_rewind.c`](https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/bin/pg_rewind/pg_rewind.c#L1052-L1085),
C, PostgreSQL licence), and Debezium refuses to resume when its stored
position history is gone and asks for a new snapshot
([`BaseSourceTask.java`](https://github.com/debezium/debezium/blob/215ffa1ad3f13a484b11c8deb9600a05d9ef08cf/debezium-connector-common/src/main/java/io/debezium/connector/common/BaseSourceTask.java#L146-L167),
Java, Apache-2.0). Via adapts the same stance: no proof, no automatic start.

## Consequences

- A proof gap no longer stops deposits, configuration or withdrawal
  observations.
- Each scan group's position can differ. Readiness checks and readers of the
  scan position must say which group they mean.
- Every outside writer of recovery-owned state, the zk verifier included, must
  take the source gate in its final transaction.
- An upgrade to this design needs downtime for every process that writes a
  shared database.

## Not decided here

- How replay treats votes, upgrades and withdrawal observations, and which
  verifier set decides finalization during replay: decided later in
  [ADR 0013](0013-keep-evidence-membership-and-derived-state-apart.md) and
  [ADR 0015](0015-verifier-set-at-proof-inclusion-decides-finalization.md).
- A shared Bitcoin signing context in coordinator and verifier messages, and a
  strict in-flight drain. Each is a separate protocol decision if it is ever
  required.

This decision builds on
[ADR 0007](0007-serialize-verifier-scans-and-cap-recovery.md) and
[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md). It
authorizes no live deployment or database change. Decision ticket:
[#434](https://github.com/vianetwork/via-core/issues/434).
