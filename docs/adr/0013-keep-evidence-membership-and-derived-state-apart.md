---
status: superseded by ADR-0020
date: 2026-10-22
---

# Every processor keeps evidence, canonical membership and derived state apart

Superseded by [ADR 0020](0020-tag-bitcoin-rows-and-replay-exactly.md). The original record follows unchanged as history.


## In short

Every Via processor that writes state from a Bitcoin block keeps three kinds of
facts apart. Evidence is what a Bitcoin transaction carried. Canonical
membership says that a piece of evidence sits in a given block of the current
chain. Derived state is computed from evidence that has canonical membership.
On a reorg, recovery withdraws the membership of the replaced blocks, exactly
once, and rebuilds the derived state. It keeps evidence, holds, sender requests
and signed history. When a processor reads a block again, it compares the full
payload stored under the same identity, and a changed payload is a conflict.
Configuration changes follow Bitcoin order and take effect from the next block.
A block that changes the same role more than once is rejected for that role.
That last rule changes which messages Via accepts.

## Background for a new reader

[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md) gives each
Via node one Bitcoin reader.
[ADR 0010](0010-commit-each-scan-group-with-its-position.md) splits the
verifier's processors into four scan groups, and each group commits its
effects with its own scan position, forwards and on undo. This decision says
what a group's undo and replay must do with the rows it writes.

[#449](https://github.com/vianetwork/via-core/issues/449) classified every
table that a Via processor writes per Bitcoin block. Only two families keep
evidence, membership and derived state in separate rows: the verifier
withdrawal tables, and the verifier proof and vote rows written since
migration `20260924`. Every other processor table mixes the three in one row.
Block hashes reach only proofs, votes and bridge withdrawals. Deposits, wallet
changes and protocol upgrades are processed by height only. A deposit's
`priority_id` packs its height, transaction index and output index, and its L2
transaction hash derives from that value. System messages carry no
transaction index. Protocol upgrades store no Bitcoin txid, height or hash.

## What went wrong

- A reorg that removes only deposits that are not yet in a batch counts as
  soft on the main node and the external node, and as batch 0 on the
  verifier. Neither path deletes those deposits. Replay skips the re-mined
  deposit, so a deposit that vanished from Bitcoin stays queued
  ([#439](https://github.com/vianetwork/via-core/issues/439)).
- The verifier's deposit insert ignores a conflict on its key
  (`ON CONFLICT (tx_id) DO NOTHING`) and never compares the payload. A
  different payload under the same key passes without an error.
- Wallet rows from replaced blocks keep their height. The current wallet for
  a role is chosen by `created_at`, which has the same value for every row
  written in one scan transaction. Two rotations in one chunk mishandle the
  second, even without a reorg.
- The verifier's upgrade upsert sets `executed = FALSE` on conflict, so
  replaying the latest upgrade block marks an executed upgrade as not executed
  ([#443](https://github.com/vianetwork/via-core/issues/443)).
- On the main node, `via_btc_inscriptions_request` and
  `via_btc_inscriptions_request_history` cascade-delete each other. A rollback
  that deletes one row removes signed history, which condition C2 on
  [#426](https://github.com/vianetwork/via-core/issues/426) forbids.
- Votes, upgrades and withdrawal observations were not known to be safe to
  replay, and the recovery of 16 October 2026 never replayed them
  ([#446](https://github.com/vianetwork/via-core/issues/446)).

## Decision

1. **Every processor keeps three kinds of facts apart.** Evidence is the
   immutable record of what a Bitcoin transaction carried. Canonical
   membership records that the evidence is in a given block of the current
   chain. Derived state is computed from evidence with canonical membership.
   Recovery withdraws the membership of the replaced blocks, including
   deposits that are not yet in a batch, and rebuilds derived state. Evidence,
   holds, sender requests and signed history stay.
2. **Replay compares the full payload under the same identity.** The result is
   `exists` when every field of the stored canonical payload matches, or
   `exists_with_different_<field>` for the first field that differs. A changed
   payload is a conflict and never overwrites the stored row. An exact replay
   keeps `executed` and completed verdicts as they are.
3. **Membership is withdrawn conditionally and exactly once.** The withdrawal
   is one conditional update that changes a row only if its membership is
   still active, and it returns whether it changed the row. A second
   withdrawal for the same cause changes nothing.
4. **Small derived state is rebuilt from evidence.** Wallet selection and the
   queue of unexecuted deposits are rebuilt from evidence with canonical
   membership. Each table has one undo mechanism.
5. **A re-mined deposit links to the old one by `(txid, vout)`.** A deposit
   already executed on L2 falls under the repair stop of
   [ADR 0014](0014-repair-only-reversible-provable-steps-and-inspect-read-only.md).
6. **Configuration follows Bitcoin order.** Configuration changes are ordered
   by block hash, height, transaction index and message order, never by
   `created_at`. A change included at height `h` takes effect at `h + 1`. The
   parser records the transaction index of every system message.
7. **More than one change for the same role in one block is rejected.**
   None of them takes effect, even when the changes are identical. The wire bytes do not change, but this changes
   which messages Via accepts. Every node must apply the same rule.
8. **Finalization moves off the sender row.** The finalization that the reader
   decides gets its own record. Sender requests and signed history survive
   recovery. The foreign-key cascades that delete them are replaced.

The comparison in step 2, the conditional withdrawal in step 3 and the
duplicate check in step 7 are short rules that Via writes in Rust, with the
idea taken from the donors named below. The donors' tests for each rule are
ported as test cases.

## Considered options

**Idempotent writes only.** Rejected. Reading the same block twice converges,
but effects of abandoned blocks stay, so a vanished deposit stays queued.

**Undo only.** Rejected. A crash between undo and the next read leaves the
same gap, and a key-only check still lets a changed payload through.

**Delete everything the replaced range produced.** Rejected. It destroys
payment and signing evidence and triggers the cascades that C2 forbids.

**Restore the whole store to a snapshot**, as ord does with savepoints.
Rejected. It also rolls back sender history.

**Key-only idempotency**, as today's deposit insert does. Rejected. A changed
payload under the same key passes without an error.

**Order configuration by `created_at`.** Rejected. It ties within one
transaction and depends on when a node wrote the row, not on Bitcoin.

**Allow several changes for one role in one block, applied in transaction
order.** Not chosen. Rejection is the simpler rule, and etcd's check
implements it without change.

**Count identical changes for one role as one.** Not chosen. An operator has
no reason to send the same change twice in one block, so a duplicate signals
an error, and one rule without an exception is simpler.

**Adopt a whole ledger, or NBXplorer's full trigger schema.** Rejected. Via
needs the separation, not the surrounding machinery.

## Why this design

Each mechanism comes from an established project that was read at a pinned
commit:

- **Evidence apart from inclusion.** NBXplorer keeps transactions, blocks and
  the inclusion relation in separate tables and clears only the inclusion when
  a block is disconnected
  ([`FullSchema.sql`](https://github.com/btcpayserver/NBXplorer/blob/b691999b14eb2a06547ffa708c11574fa323d769/NBXplorer/DBScripts/FullSchema.sql#L32-L102),
  C#, MIT). Adapted.
- **Field-wise replay comparison.** TigerBeetle's `create_transfer_exists`
  compares the stored transfer field by field and returns `exists` or the
  first `exists_with_different_<field>`
  ([`state_machine.zig`](https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig#L3971-L4033),
  [tests](https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine_tests.zig#L1768-L1812),
  Zig, Apache-2.0). Idea; Via writes its own comparison. Via defines its own payload fields and does
  not copy TigerBeetle's optional-field exceptions.
- **Conditional withdrawal.** Formance ledger v2 reverts a transaction with
  `UPDATE … WHERE reverted_at IS NULL RETURNING` and reports whether a row
  changed
  ([`transactions.go`](https://github.com/formancehq/ledger/blob/3ab99b3ad28ffcc6034d94b4e3d831b823cce7a5/internal/storage/ledger/transactions.go#L154-L213),
  Go, MIT on the `main_v2` branch). Idea; Via writes its own SQL. A repeated withdrawal for the
  same reorg converges instead of returning an error.
- **Duplicate check before a batch applies.** etcd rejects a transaction that
  writes one key twice before it executes any write
  ([`key.go`](https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/server/etcdserver/api/v3rpc/key.go#L236-L314),
  [test](https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/tests/integration/clientv3/txn_test.go#L32-L44),
  Go, Apache-2.0). Idea; Via writes its own check, with the role as the key. etcd orders writes by
  revision and sub-revision, not by time
  ([`kvstore_txn.go`](https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/server/storage/mvcc/kvstore_txn.go#L174-L245)),
  which Via adapts as block, transaction and message order.
- **Compensation, not erasure.** Apache Fineract reverses a journal entry by
  adding a linked opposite entry and keeps the original
  ([`JournalEntryWritePlatformServiceImpl.java`](https://github.com/apache/fineract/blob/71c39730e9178301b7ab949ba711238ce81fc66d/fineract-provider/src/main/java/org/apache/fineract/accounting/journalentry/service/JournalEntryWritePlatformServiceImpl.java#L318-L408),
  Java, Apache-2.0). TigerBeetle keeps a pending hold apart from its post or
  void
  ([`state_machine.zig`](https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig#L4036-L4127)).
  Both are adapted to Via's existing holds.
- **Version-keyed upgrade replay.** ZKsync Era's upgrade processor skips an
  upgrade older than the last one it saw
  ([`decentralized_upgrades.rs`](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_watch/src/event_processors/decentralized_upgrades.rs#L94-L113),
  Rust, MIT or Apache-2.0). Adapted. It fixes the shape of #443.

Via's own verifier withdrawal tables already keep observations, per-block
inclusions, holds and fulfillments apart. This decision applies that shape to
every processor.

Compared with what Via had, a deposit that vanished from Bitcoin leaves the
queue. Replaying an upgrade block keeps its `executed` state. The current
wallet no longer depends on when a node inserted a row. Signed history
survives a rollback. A changed payload under a known identity stops the group
with a named conflict, instead of being overwritten or skipped. The
[research note](../research/bitcoin-reorg-handling.md#decomposed-prior-art-per-sub-problem)
lists every donor with its licence and verdict.

## Consequences

- Deposit, wallet and upgrade rows need the block hash and Bitcoin position
  that produced them. The parser needs the transaction index of system
  messages.
- Finalization needs its own record, and the sender cascades are replaced.
- A block that carries two changes for one role was accepted before, and
  `created_at` decided which change won. Now none of them applies.
  All nodes that share a database switch together, by the stop, migrate and
  restart upgrade in ADR 0010.
- Each rule taken from a donor carries an origin comment with its pinned
  permalink and what Via changed. No licence text is added: Via writes these
  rules itself.

## Not decided here

- Which old rows can be tied to the block that produced them during
  migration: what cannot be proved needs an operator baseline, decided in
  [ADR 0010](0010-commit-each-scan-group-with-its-position.md).
- Whether past blocks on any network carry two changes for one role. This has
  not been checked.

This decision builds on
[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md) and
[ADR 0010](0010-commit-each-scan-group-with-its-position.md). It authorizes no
live deployment or database change. Decision tickets:
[#439](https://github.com/vianetwork/via-core/issues/439) and
[#446](https://github.com/vianetwork/via-core/issues/446), decided together.
Evidence: [#449](https://github.com/vianetwork/via-core/issues/449) and
[#459](https://github.com/vianetwork/via-core/issues/459).
