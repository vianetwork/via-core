# Verifier checkpoints and proof continuity

Status: source research completed on 2026-10-14. [ADR
0007](../adr/0007-serialize-verifier-scans-and-cap-recovery.md) accepts capped
recovery, shared database ownership, and a visible proof-gap failure. Historical
replay and automatic repair remain separate decisions. This note records source
behavior, not deployment or live recovery evidence.

## Why completed progress matters

A header detector can observe Bitcoin height 156456 while inscription processing
has completed only 156449. Assigning 156455 during recovery falsely completes
the intervening blocks. A proof at 156451 is then behind the next scan start.
The verifier reverter at Via `8a49f355bfe31720f21b8181db471243709194e5`
unconditionally makes that assignment in
`via_verifier/node/via_block_reverter/src/lib.rs`,
`ViaVerifierBlockReverter::reorg`.

The main-node soft-reorg path already caps its target, but reads progress before
its recovery transaction. The verifier watcher checks a height after applying
effects and then updates progress separately. Neither is a complete concurrency
contract. Recovery can leave a lagging height unchanged, or rewind and later
return to the same height. An expected-height comparison cannot distinguish
either case from an uninterrupted scan. Deletion and withdrawal invalidation
must still use the actual divergence boundary, independently of the smaller
completed checkpoint.

## Ownership and recovery candidates

The following sources were inspected at immutable revisions. No external
implementation body is copied.

- [Bitcoin
  Core](https://github.com/bitcoin/bitcoin/blob/f13ae519de94394314cd63a27924b0998ac65d45/src/wallet/wallet.cpp),
  `CWallet::blockDisconnected`, MIT. The wallet processes disconnects under its
  wallet mutex and records its processed parent height and hash. Adopt
  consumer-owned progress. Its ordered notifications establish a precondition
  that Via's independent header detector does not have.

- [graph-node](https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs),
  `rewind_or_truncate_with_conn`, MIT or Apache-2.0. A deployment lock and
  transaction cover pointer and entity reversion. Its runner serializes reverts.
  Closest PostgreSQL ownership precedent. Via must also serialize separate tasks
  and compose processor transactions. This historical pin has failed hosted
  checks and is qualified design evidence.

- [Kafka](https://github.com/apache/kafka/blob/b9b0c909c6f01a2b50ff3a2478903be11312e35a/group-coordinator/src/main/java/org/apache/kafka/coordinator/group/modern/consumer/ConsumerGroup.java),
  `validateOffsetCommit`, Apache-2.0. Member and assignment epochs reject stale
  progress commits. Reuse stale-owner fencing semantics. Kafka does not
  atomically commit Via's SQL effects. Its offsets identify the next record,
  unlike Via's last completed block.

- [PostgreSQL](https://github.com/postgres/postgres/blob/d733fcaf5cac6b88de9417f2b43e51490e36c7b1/src/backend/replication/slot.c),
  `ReplicationSlotAcquire`, PostgreSQL licence. A logical slot has one active
  owner. Logical decoding can resend data after a crash. Reuse ownership and
  replay discipline. Forward-only confirmed WAL positions are not a Bitcoin
  rewind algorithm.

- [BDK](https://github.com/bitcoindevkit/bdk/blob/8b15e743e99a67be7d1d06a632b5f96bb7e365da/crates/bitcoind_rpc/src/lib.rs),
  `poll_once` and `poll`, MIT or Apache-2.0. An emitter finds a retained common
  ancestor and revisits replacement blocks. Changesets need caller persistence.
  Corroborates consumer restart identity. Failed audit jobs qualify the
  inspected pin. Replacing Via's indexer would not supply its inscription or SQL
  contracts.

- [Electrs](https://github.com/romanz/electrs/blob/d975dd03f2a410c4473116d5a2be950a205e0443/src/tracker.rs),
  `Tracker`, MIT. One mutable tracker owns synchronization, while current
  indexing delegates to bindex. Established application, but bindex maturity was
  not established. Reject it as the primary persistence implementation source.

  These projects have established users and maintainers. The reports checked
  relevant tests and hosted-check histories. Inspected donor tests were not
  executed locally, and successful hosted jobs do not establish that every test
  passed at each pin. Graph-node's forward pointer SQL checks direction, not an
  expected old hash or recovery generation. Recording a hash without checking
  the relevant work identity does not fence stale completion.

The accepted schema-compatible adaptation holds a database owner through
fetching and application. Fetching outside that interval would need a checked
recovery generation and fetched-chain identity before any effects commit. A
persisted generation is an alternative with a migration cost. A cap alone or a
height-only completion check leaves races unresolved. Whole-indexer replacement
is disproportionate.

## Historical replay remains conditional

[Bitcoin Core's
`rescanblockchain`](https://github.com/bitcoin/bitcoin/blob/f13ae519de94394314cd63a27924b0998ac65d45/src/wallet/rpc/transactions.cpp)
reserves a rescan owner, checks available history, and reports actual
completion. [Kafka's offset-reset
tool](https://github.com/apache/kafka/blob/b9b0c909c6f01a2b50ff3a2478903be11312e35a/tools/src/main/java/org/apache/kafka/tools/consumer/group/ConsumerGroupCommand.java)
uses `ConsumerGroupService.resetOffsets` and `prepareOffsetsToReset` to require
inactive consumers and separate preparation from execution. These provide
guarded maintenance precedents, not permission to change Via state.

A full rewind through the normal watcher preserves its wallet-update chunking
and message order. A stopped-watcher bounded replay reduces repeated work but
needs a supported historical driver. Neither is safe merely because inserts have
conflict clauses. At Via `8a49f355`, `withdrawals_dal.rs` can update an existing
value and L2 hash, and `via_protocol_versions_dal.rs`, `save_protocol_version`,
can reset an executed upgrade to pending. An isolated semantic rehearsal must
cover every reached processor, repeated and interrupted replay, real parent and
proof identities, wallet history, and retained payment and signing risk. No
verdict or finalization may be fabricated. The full-rewind preference and
bounded-hole alternative remain conditional on that proof and separate live
approval.

## Gaps must prevent completed progress

At the inspected Via baseline, `VerifierMessageProcessor::process_messages` logs
and skips a nonsequential proof whose parent is absent. It returns success,
allowing the checkpoint to pass that publication. A sequential publication with
the wrong parent is a separate invalid-publication path. Existing valid-fork
handling changes the canonical suffix only after its retained-parent check
succeeds.

- [ZKsync
  Era](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_watch/src/event_processors/priority_ops.rs),
  `PriorityOpsEventProcessor::process_events`, MIT or Apache-2.0. Serial gaps
  fail loudly, including gaps across processing batches. Adapt continuity
  failure to a typed checkpoint-blocking error and visible pause. Do not copy
  the panic or call it automatic repair.

- [Optimism](https://github.com/ethereum-optimism/optimism/blob/16fc715df980e26f7933dbffdf4c97871aba90e0/op-node/rollup/derive/l1_traversal.go),
  `AdvanceL1Block`, MIT. A wrong parent requests a pipeline reset. Consistent
  origins and bounded ancestry govern recovery. Useful future reset contract.
  Ethereum defaults and engine state do not transfer to Via. The historical pin
  has failed and pending checks.

- [Nitro](https://github.com/OffchainLabs/nitro/blob/ccc5c828d9ea22f161efc784bac4d4a6559b7e59/arbnode/inbox_reader.go),
  inbox reader consistency and backward search, BSL-1.1. Counts and accumulators
  reveal missing input. Serialized tracker batches own rollback. Ideas only
  under the restricted licence. Bitcoin inscriptions lack its authoritative L1
  message counter.

- [Stacks](https://github.com/stacks-network/stacks-core/blob/82fd36fa5aed7b5569eced311830d80c01946caa/stackslib/src/burnchains/burnchain.rs),
  burnchain restart selection, GPL-3.0. After downloader failure, restart walks
  to a header hash actually present in the consumer store. Direct precedent for
  consumer-store authority. The historical regression is ignored by default. No
  GPL code port is proposed.

- [Vigilante](https://github.com/babylonlabs-io/vigilante/blob/d3bde724674a9117687ef0c6f9d2b1661b59dc35/reporter/block_handler.go),
  reporter block handling, BSL-1.1. Gaps fail and trigger bootstrap with bounded
  retries and a fatal stopping point. Organisationally backed with limited
  history. Direct gap-to-bootstrap integration coverage was not established.
  Ideas only.

  The immediate gap failure preserves expected and observed batch and parent
  identities, proof transaction, and scanned Bitcoin location. It rolls back the
  scan's earlier writes and pauses polling rather than repeatedly applying
  partial upserts. It does not grant a missing-parent claim authority to delete
  state or rewind history automatically.

The shared owner does not require the exclusive withdrawal gate for every
participant. Detector header inserts keep existing hashes on conflict and only
record pending divergence. The reverter performs deletion and invalidation.
Initialization writes only the progress checkpoint. Both can retain the scan
owner without excluding withdrawal and vote writers. Main scans acquire the
exclusive gate before their first wallet check so valid-fork handling never
upgrades after a wallet lock. The old release scan acquires it after fetching,
before processor effects. Recovery keeps owner, gate, then wallet order.

Only a proof accepted from the configured sequencer address reaches the gap
path. The shared client filters `ProofDAReference` by that address. Main already
retains the scanned hash for persisted provenance. The release gap instead
reports the proof transaction and height without changing the shared client.
Those identify the publication to investigate, but do not establish its exact
historical inclusion hash or canonical ancestry.

## Open questions and limits

Automatic repair needs a verified consistent prefix, historical availability,
semantic replay proof, and bounded work. A detector horizon change is separate.
Via's watcher confirmation cutoff and withdrawal confirmation policy have
different consumers, so delaying the shared header table can apply the delay
twice. No new confirmation count, recovery depth, or retry number is selected by
these comparisons. Source changes do not repair previously skipped history or
prove any live deployment.
