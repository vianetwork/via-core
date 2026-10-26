---
status: decided
---

> Superseded design scope, 2026-10-24. The text below records the earlier design.
> Current reader positions, block-tagged replay and recovery are defined by
> [ADR 0019](../../adr/0019-one-reader-position-with-trailing-consumers.md),
> [ADR 0020](../../adr/0020-tag-bitcoin-rows-and-replay-exactly.md) and
> [ADR 0021](../../adr/0021-automatic-shallow-recovery-above-confirmed-frontier.md).

# Bitcoin reader redesign: design and rationale

This record explains how Via nodes will read Bitcoin, notice reorgs and recover from them, and why.
It is written for an engineer who is new to this code and was not part of the discussion. Each topic
answers six questions in the same order:

1. What Via does today, with file references.
2. Why that breaks, with the concrete mechanism or incident.
3. Which options were considered.
4. What was chosen.
5. Which established projects work that way, with pinned links and licences.
6. Why the choice works better than what Via had.

The evidence lives elsewhere. The [Bitcoin reorg handling research note](../../research/bitcoin-reorg-handling.md)
holds the Bitcoin Core RPC facts, the measured reorg depths and the source revisions of every
comparison project. The accepted decisions are recorded as ADRs 0008 to 0015, linked below. The
work is tracked on the [reader map #426](https://github.com/vianetwork/via-core/issues/426). This
record describes intended behavior, not deployed behavior, and authorizes no migration, deployment
or live recovery.

## Current replay and admission rules

ADRs 0019–0021 supersede the scan-group and general repair design described below. The shared storage crate owns one selected-header transition and the common consumer commit. Each native DAL implements typed role methods and owns its SQL; SQL strings do not cross that boundary. Each consumer saves both applied and oldest-still-needed header positions with its effects. Advancing applied progress cannot release history required after a crash.

Initial replay readiness depends on historical configuration decisions, and bridge rotation decisions depend on local proof verdicts. Requiring global replay readiness for those verdicts would create a dependency cycle. Local verification and verdict save may proceed during replay, gated per proof on complete and mature canonical inputs. Global replay readiness remains mandatory for votes, signatures, L2 execution, finalization and withdrawals. Every final admission also checks fresh selected-source membership, the current mature prefix and its required consumer positions under the existing owner/source fences.

A rotation at h uses canonical proofs after the reset cutoff through h, verified by the verifier itself. Proofs finalized before the reset cutoff are excluded only from that evidence requirement: they are pre-#469 finalized history, and expired testnet DA makes re-verification impossible. Their completed rows and FKs remain unchanged and unproven; there is no new legacy proof incarnation or recomputation route. Mainnet starts with no such rows. The exception confers no other authority. Withdrawal finalization receipts bind the current exact proof-header incarnation, so a retained completed flag alone cannot enable payment after replay or a remine.

A second vote from one member on one exact proof is a replay conflict, even from a different source position or with the same choice. The identical occurrence may retry after all immutable fields compare equal. A conflict persists the stop reason and escapes the reader task for diagnosis.

The committed reader metrics use the `via_bitcoin_reader` prefix and a `role` label: head, frontier, replay_ready, stopped, consumer position, pending and in-flight publication counts, inscription and confirmation latency, account balance and inscription errors. Consumer position also has a `consumer` label. The old role-specific broadcast-time and transient-inscription-error metrics are replaced by these shared publication metrics; pending counts come from committed database state. No matching dashboards or alerts are stored in this repository. Deployment repositories must update metric consumers when rolling out this source change.

## Decision status

Every topic in this record is decided. The maintainer decided each ticket.

| Topic | Ticket | Status | Record |
| --- | --- | --- | --- |
| [One reader per node](#one-reader-per-node-owns-reading-detection-and-recovery) | [#432](https://github.com/vianetwork/via-core/issues/432) | Decided 2026-10-22 | [ADR 0008](../../adr/0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md) |
| [One authoritative Bitcoin node](#one-authoritative-bitcoin-node-decides-which-chain-is-current) | [#448](https://github.com/vianetwork/via-core/issues/448) | Decided 2026-10-22 | [ADR 0008](../../adr/0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md) |
| [Finding the fork point](#a-bounded-hash-walk-finds-the-fork-point) | [#438](https://github.com/vianetwork/via-core/issues/438) | Decided 2026-10-22 | [ADR 0008](../../adr/0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md) |
| [Observe at the tip, act at maturity](#observe-at-the-tip-act-only-at-maturity) | [#435](https://github.com/vianetwork/via-core/issues/435) | Decided 2026-10-22 | [ADR 0009](../../adr/0009-observe-at-the-tip-and-act-behind-maturity-depth.md) |
| [Maturity depth per network](#maturity-depth-per-network-6-on-mainnet-100-on-test-networks) | [#452](https://github.com/vianetwork/via-core/issues/452) | Decided 2026-10-22, mainnet amended to 6 | [ADR 0009](../../adr/0009-observe-at-the-tip-and-act-behind-maturity-depth.md) |
| [Scan groups and transactions](#each-scan-group-commits-its-effects-with-its-own-scan-position) | [#434](https://github.com/vianetwork/via-core/issues/434) | Decided 2026-10-22 | [ADR 0010](../../adr/0010-commit-each-scan-group-with-its-position.md) |
| [Shared code and the copied fork walk](#the-shared-reader-lives-in-via_reorg-and-copies-ldks-fork-walk) | [#433](https://github.com/vianetwork/via-core/issues/433), evidence [#450](https://github.com/vianetwork/via-core/issues/450) | Decided 2026-10-22 | [ADR 0011](../../adr/0011-shared-reader-in-via-reorg-with-copied-ldk-walk.md) |
| [Reorg tests](#test-on-one-chain-model-and-four-real-bitcoin-core-cases) | [#437](https://github.com/vianetwork/via-core/issues/437) | Decided 2026-10-22 | [ADR 0012](../../adr/0012-test-reorgs-on-one-chain-model-and-real-bitcoin-core.md) |
| [PR #398 lands first, with a lock](#pr-398-lands-first-with-the-shared-lock) | [#431](https://github.com/vianetwork/via-core/issues/431) | Decided 2026-10-22 | Planning decision, no ADR |
| [Undo and replay](#every-processor-keeps-evidence-canonical-membership-and-derived-state-apart) | [#439](https://github.com/vianetwork/via-core/issues/439), [#446](https://github.com/vianetwork/via-core/issues/446) | Decided 2026-10-22 | [ADR 0013](../../adr/0013-keep-evidence-membership-and-derived-state-apart.md) |
| [Automatic repair and read-only inspection](#recovery-repairs-only-reversible-provable-steps-and-inspection-is-read-only) | [#436](https://github.com/vianetwork/via-core/issues/436) | Decided 2026-10-22 | [ADR 0014](../../adr/0014-repair-only-reversible-provable-steps-and-inspect-read-only.md) |
| [Verifier set for finalization](#the-verifier-set-at-a-proofs-inclusion-block-decides-its-finalization) | [#440](https://github.com/vianetwork/via-core/issues/440), evidence [#451](https://github.com/vianetwork/via-core/issues/451) | Decided 2026-10-22 | [ADR 0015](../../adr/0015-verifier-set-at-proof-inclusion-decides-finalization.md) |
| [First positions after migration](#each-scan-group-commits-its-effects-with-its-own-scan-position) | [#426](https://github.com/vianetwork/via-core/issues/426) design stop | Decided 2026-10-22 | [ADR 0010](../../adr/0010-commit-each-scan-group-with-its-position.md) |
| [Repeated changes for one role in one block](#every-processor-keeps-evidence-canonical-membership-and-derived-state-apart) | [#426](https://github.com/vianetwork/via-core/issues/426) design stop | Decided 2026-10-22 | [ADR 0013](../../adr/0013-keep-evidence-membership-and-derived-state-apart.md) |
| [Snapshot cleanup and the manual reverter](#recovery-repairs-only-reversible-provable-steps-and-inspection-is-read-only) | [#426](https://github.com/vianetwork/via-core/issues/426) design stop | Decided 2026-10-22 | [ADR 0014](../../adr/0014-repair-only-reversible-provable-steps-and-inspect-read-only.md) |
| [A rotation included while a proof is open](#the-verifier-set-at-a-proofs-inclusion-block-decides-its-finalization) | [#426](https://github.com/vianetwork/via-core/issues/426) design stop | Decided 2026-10-22 | [ADR 0015](../../adr/0015-verifier-set-at-proof-inclusion-decides-finalization.md) |
| [Cadence and committed-state metrics](#cadence-and-committed-state-metrics) | [#426](https://github.com/vianetwork/via-core/issues/426) design stop | Decided 2026-10-22 | This record |
| [Reused code checks](#reused-code-and-how-it-is-checked) | [#426](https://github.com/vianetwork/via-core/issues/426) design stop | Decided 2026-10-22 | This record |

## Words used in this record

A **reorg** happens when Bitcoin replaces its newest blocks with different ones. A block's
**confirmations** are the number of blocks from it to the chain tip, counting the block itself as the
first. A Via **message** is data a Via component writes into a Bitcoin transaction, such as a
deposit, a proof reference, a vote or a governance upgrade. Most messages are inscriptions, data
carried in the witness of a Bitcoin transaction.

The redesign uses these terms from the [glossary](../../../GLOSSARY.md):

- **Bitcoin reader**: the one part of a Via node that reads Bitcoin blocks in order, detects reorgs
  and recovers from them.
- **Authoritative Bitcoin node**: the one Bitcoin Core node whose view of the chain a Via component
  accepts.
- **Scan group**: a set of Via processors that commit their effects and one scan position together.
- **Scan position**: the height and hash of the last Bitcoin block that a scan group has fully
  processed.
- **Fork point**: the last block that the stored chain shares with the authoritative node's chain
  after a reorg.
- **Maturity depth**: the number of confirmations a block needs before a Via node takes an
  irreversible action based on it.
- **Rollback-search limit**: the largest number of stored blocks the reader walks back to find a fork
  point before it stops.
- **Recovery barrier**: the committed record that marks a reorg recovery as in progress.
- **Evidence**: an immutable record of what a Bitcoin transaction carried.
- **Canonical membership**: the fact that a piece of evidence is in a given block of the current
  chain.
- **Derived state**: state that a Via node computes from evidence with canonical membership.
- **Read-only inspection**: a run that reports what recovery would change without writing to any
  store.
- **Mutation rehearsal**: a run of the real recovery writes against a disposable database copy.

## The situation today

A Via node must see every Via message that Bitcoin carries, once, on the chain that Bitcoin keeps.
The main node and the external node need deposits and governance upgrades. The verifier needs
proofs, votes, upgrades, deposits and withdrawals. When a reorg replaces blocks, the node must undo
what it learned from them and read the replacement blocks instead.

Each node splits this work across four tasks that share one database:

| Task | Main node and external node | Verifier and coordinator |
| --- | --- | --- |
| Reader: parses blocks, runs message processors, saves the scan position | `core/node/via_btc_watch` | `via_verifier/node/via_btc_watch` |
| Reorg detector: re-checks recent block hashes | `core/node/via_main_node_reorg_detector` | `via_verifier/node/via_reorg_detector` |
| Reverter: deletes effects of replaced blocks, moves the scan position back | `core/node/via_block_reverter` | `via_verifier/node/via_block_reverter` |
| Startup: prepares the scan position | `core/node/via_node_storage_init` | `via_verifier/node/via_verifier_storage_init` |

The reader stores one scan position, a height, in the `via_btc_watch` row of
`via_indexer_metadata`. Each tick it reads at most 50 blocks, up to the node's tip minus
`block_confirmations`, and fetches each block by height. The detector stores `(height, hash)` rows in
`via_l1_blocks` and re-fetches the last 100 heights every tick to compare hashes. When it finds a
difference it writes a row into `via_l1_reorg`. That row pauses the sender, the aggregator, the zk
verifier, the coordinator and the DA dispatcher until the reverter has finished and deleted it.

This split caused the testnet stall that [ADR 0007](../../adr/0007-serialize-verifier-scans-and-cap-recovery.md)
describes. On 13 October 2026 the verifier's reader had finished block 156449. The detector, which
runs ahead, saw a reorg starting at 156456. The reverter set the scan position to 156455, the block
before the reorg, without looking at where the reader was. Blocks 156450 to 156455 were never read,
and the proof for batch 266 was in block 156451. The proof for batch 265 had been lost the same way
around 3 October. No batch was finalized for 11 days. ADR 0007 fixed the immediate cause: recovery
now only moves the scan position back, one PostgreSQL advisory lock (`lock_btc_watch`) serializes the
four tasks, and a missing proof pauses the reader loudly. The four tasks remain, and so do the other
defects listed in the [research note](../../research/bitcoin-reorg-handling.md#known-failure-modes).

The redesign respects seven conditions recorded on [#426](https://github.com/vianetwork/via-core/issues/426):

- C1: the components that pause on `via_l1_reorg` keep an equivalent committed signal.
- C2: recovery never deletes or edits sender rows, which record what was sent to Bitcoin.
- C3: any change to the confirmation horizon is reconciled with the sender's confirmation check.
- C4: finalization order is safe only because one vote is in flight at a time. The redesign keeps
  that precondition.
- C5: types from the archived `bitcoincore-rpc` crate stay inside `via_btc_client`. rust-bitcoin
  types such as `Block` and `Txid` may cross it.
- C6: reorg tests are deterministic, and per-network limits fail loudly when exceeded.
- C7: a consumer in another process with its own database can adopt the reader later.

## How it fits together

This section follows one Bitcoin block through the new reader. Each step names the topic that
explains it.

1. **Notice a change.** The reader asks its authoritative Bitcoin node for the best block hash. If
   the hash equals the stored tip, nothing has changed. If it differs, the reader continues.
2. **Check that the stored tip is still current.** The reader asks for its stored tip by hash. If
   the node reports `confirmations` of 1 or more, the stored chain is still the current chain and the
   reader goes forward. If the node reports `confirmations: -1`, the stored tip has left the current
   chain, and the reader goes to step 7. If the node is unreachable, or reports only a lower height or
   no header, the result is inconclusive: the reader holds actions that need fresh evidence and asks
   again later ([authoritative node](#one-authoritative-bitcoin-node-decides-which-chain-is-current)).
3. **Fetch the next block outside any transaction.** The reader fetches the header of the next
   height and checks that its parent hash equals the stored tip's hash. A mismatch is a reorg and
   leads to step 7. Then each scan group's adapter prepares its work: it parses the block's Via
   messages and makes any further Bitcoin RPC calls it needs. No database transaction is open during
   these network calls ([scan groups](#each-scan-group-commits-its-effects-with-its-own-scan-position),
   [shared code](#the-shared-reader-lives-in-via_reorg-and-copies-ldks-fork-walk)).
4. **Apply each scan group with its position.** For each scan group the reader opens one short
   transaction. It takes the owner lock and, on the verifier, the source gate. It checks that no
   recovery barrier is set and that the group's scan position still names the parent block. It then
   writes the group's effects and the group's new scan position `(height, hash)`, and commits. A
   group that fails, for example on a proof continuity gap, keeps its old position. The other groups
   continue. The reader also stores the block's `(height, hash, parent_hash)` in its ancestry, so a
   later reorg can be traced back through it. If a row already exists under the same identity, the
   processor compares the full payload: an exact match changes nothing, and a different payload is a
   conflict that stops the group. Configuration changes apply in block, transaction and message
   order and take effect from the next block ([evidence and replay](#every-processor-keeps-evidence-canonical-membership-and-derived-state-apart)).
5. **Wait for maturity before acting.** Recording a deposit, a proof or a vote is reversible, so it
   happens at the tip. A vote, a finalization, a payout or the sender's completion of its own
   transaction is not reversible. Those jobs run from stored records and act only when the source
   block is on the current chain and has at least the network's maturity depth of confirmations:
   6 on mainnet, 100 on testnet4 ([maturity](#observe-at-the-tip-act-only-at-maturity)).
   Finalization counts each verifier's vote once, against the verifier set that was active at the
   proof's inclusion block ([verifier set](#the-verifier-set-at-a-proofs-inclusion-block-decides-its-finalization)).
6. **Fence work done outside the reader.** The zk verifier and other record-driven jobs prepare
   their work without locks. Their final write takes the same source gate, checks every input again
   and holds the gate through commit. Recovery cannot commit between their check and their write
   ([scan groups](#each-scan-group-commits-its-effects-with-its-own-scan-position)).
7. **Recover from a reorg.** The reader commits the recovery barrier first, so no new authority is
   granted while recovery runs. It then walks parent hashes back on the stored branch and the new
   branch until both name the same block, at most 100 stored blocks deep. That block is the proved
   fork point ([fork point](#a-bounded-hash-walk-finds-the-fork-point)).
8. **Undo per group, then clear the barrier.** Each scan group withdraws, once, the canonical
   membership of the evidence from the replaced blocks, rebuilds its derived state and moves its
   scan position back to the fork point, or to its own position if that is lower, in one
   transaction. Evidence, holds, sender requests and signed history stay. A final transaction checks
   that all affected stores agree and clears the barrier. Forward reading then continues on the new
   branch from step 3 ([evidence and replay](#every-processor-keeps-evidence-canonical-membership-and-derived-state-apart)).
9. **Stop loudly when recovery cannot be proved.** If a stored header the walk needs is missing, no
   shared block exists within 100 blocks, a processor has no complete undo, the stores disagree, or a
   step would undo an external effect or executed L2 state, the barrier stays set and the reader
   reports the failed condition and the last proved facts. Revoking stale authority and adding holds
   still happen. An operator decides the next step
   ([repair boundary](#recovery-repairs-only-reversible-provable-steps-and-inspection-is-read-only)). A proof continuity gap is a separate stop that pauses only the proof
   group.
10. **Inspect before acting.** An operator can run a read-only inspection that reports what recovery
    would change, without writing to any store or starting a sender, coordinator or signer. Recovery
    applies a plan only if the identities it was planned against still hold
    ([repair boundary](#recovery-repairs-only-reversible-provable-steps-and-inspection-is-read-only)).

## One reader per node owns reading, detection and recovery

**Today.** Four tasks per node read or write the scan position: reader, detector, reverter and
startup (see [the situation today](#the-situation-today)). The main node and the verifier each carry
their own copy of all four. When `restart_indexing` is set, startup in either tree overwrites the
scan position with a configured start height
(`core/node/via_node_storage_init/src/indexer.rs`, `via_verifier/node/via_verifier_storage_init/src/indexer.rs`).
`via_l1_blocks` has had no row for many blocks since PR #365, and it stores no parent hashes.

**Why it breaks.** Four writers in two copies have to stay in step, and the batch 265 skip happened
when they did not. Startup can move the scan position forward past blocks the reader never read. One
scan position cannot describe two consumers that progress differently: a proof processor paused by a
gap and a withdrawal observer that should keep recording payments. A sparse block table cannot prove
where a fork starts.

**Options considered.**

| Option | Consequence |
| --- | --- |
| Separate tasks with an ordered handoff (today) | Four writers per tree, kept consistent by locks and flags. Caused the batch 265 skip. |
| A separate reader service that sends block and rollback events | Cannot make independent databases change atomically. Breaks the scan transaction ADR 0007 relies on. No studied project recovers outside the process that holds the state. |
| One owner per node with one scan position for everything | Simple, but cannot hold a paused proof processor and a progressing withdrawal observer at once. |
| One owner per node, one scan position per scan group, full ancestry | One authority for chain order. Independent consumers progress or pause on their own. |

**Decision.** One Bitcoin reader per Via node, inside the node process, reads, detects, reverts and
applies blocks in sequence. Startup and pending recovery are part of its lifecycle, and a configured
start height never moves a stored position forward without evidence. Each scan group keeps one
scan position stored as `(height, hash)`. The reader stores `(height, hash, parent_hash)` for every
block it advances over, including blocks with no Via messages.

**Who does it this way.** Bitcoin Core's index framework runs [startup][core-index-startup] and
[catch-up][core-index-sync] inside each index's own `BaseIndex` (MIT). LDK's
[`synchronize_listeners`][ldk-startup] keeps one block locator per listener (MIT or Apache-2.0).
Graph Node keeps one head per subgraph and commits it with that subgraph's writes, [forward][graph-apply]
and [backward][graph-undo] (MIT or Apache-2.0). Reth's execution extensions report a finished height
and hash per consumer ([notifications][reth-notifications], MIT or Apache-2.0). Clementine keeps one
progress row per consumer ([schema][clementine-schema]); it is GPL-3.0, so it is an idea only.

**Why it works better.** Only one piece of code moves a scan position, so the batch 265 class of
skip cannot come from two tasks disagreeing. Recovery runs inside the same sequence as reading, so it
cannot overtake the reader. A proof gap no longer has to stop unrelated work. Stored ancestry lets the
node prove a fork point from its own records instead of trusting a height.

Status: decided in [#432](https://github.com/vianetwork/via-core/issues/432), recorded in
[ADR 0008](../../adr/0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md).

## One authoritative Bitcoin node decides which chain is current

**Today.** The detector fetches blocks by height across its 100-block window
(`fetch_block_window` in `core/lib/via_btc_client/src/client/mod.rs`). If the node's tip is below
the highest stored block, a request for a missing height fails. The detector treats every failed
fetch as inconclusive (`core/node/via_main_node_reorg_detector/src/lib.rs`, and the same pattern in
the verifier detector) and waits until the chain grows back
([#444](https://github.com/vianetwork/via-core/issues/444)).

**Why it breaks.** A node can report a lower tip for two reasons. It may have moved to a shorter
chain with more work, which happens on testnet4 because of its minimum-difficulty rule. Or it may be
behind or restarting. The two need different responses. Today Via gives neither: it waits, and it
keeps acting on blocks its own Bitcoin node has already left.

Bitcoin Core gives a precise signal. `getblockheader` for a known block that is not on the active
chain returns `confirmations: -1`. An unknown hash returns an error. A lower height says nothing about
which branch the node is on.

**Options considered.**

| Option | Consequence |
| --- | --- |
| Wait for the chain to grow (today, LDK in steady state, Chainlink LogPoller) | Safe against a lagging source, but keeps Via acting on blocks its node has disowned. |
| Roll back on any lower height (Fulcrum) | Fast, but wrong when the source is only behind. |
| Roll back when the authoritative node reports the stored tip with `confirmations: -1`, and treat a lower height alone as inconclusive | Acts on positive evidence and holds on ambiguous evidence. Needs one node named as the authority. |
| A failover pool of nodes | Needs a handoff rule, because pool members can disagree or lag. |

**Decision.** Each component reads from one authoritative Bitcoin Core node. When that node reports
the stored tip with `confirmations: -1`, recovery starts at once, even if no replacement block has
arrived. A lower height alone, an unknown header or an unavailable node is inconclusive. The reader
then holds actions that need fresh evidence, deletes nothing and does not invent a fork point. A
failover pool is not assumed.

**Who does it this way.** Bitcoin Core computes `-1` in [`ComputeNextBlockAndDepth`][core-membership]
(MIT). BDK's emitter treats [`confirmations < 0` as stale][bdk-emitter] (MIT or Apache-2.0). Esplora
electrs [acts on a lower known tip][esplora-shrink] and bindex [drops stale tips][bindex-shrink], both
MIT, because they trust their own node. Alpen separates [a lagging prefix from a diverged
chain][alpen-query] (MIT or Apache-2.0). Fulcrum's [height-only rollback][fulcrum-shrink] is
GPL-3.0-or-later and recorded as an idea that Via rejects. LDK's [steady-state poll][ldk-poll] and
LogPoller's [wait for growth][chainlink-shrink] fit sources they distrust, which Via's own node is not.

**Why it works better.** Via reacts to the one answer that proves the stored block is gone, as soon
as its node gives it. A node that is merely behind no longer causes a false rollback, and a real
shrink no longer waits for chain growth. The reader no longer guesses from heights.

Status: decided in [#448](https://github.com/vianetwork/via-core/issues/448), recorded in
[ADR 0008](../../adr/0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md).

## A bounded hash walk finds the fork point

**Today.** `scan_for_reorg` in [`core/lib/via_reorg/src/lib.rs`][via-reorg] compares stored and
fetched hashes by height inside the window and returns the first height that differs. The window is a
hard-coded 100 ([`reorg_window()`][via-window]). Each height in the window costs a `getblockhash` and a
full `getblock` download, and only the hash is used
([#409](https://github.com/vianetwork/via-core/issues/409)).

**Why it breaks.** If every compared height differs, the first height of the window is reported as
the fork point, though nothing proved it. At stored height 200 with a real fork point at 50, the
detector compares 101 to 200 and reports 101. Recovery then restarts from the wrong height with no
error, and walks back one window at a time
([#444](https://github.com/vianetwork/via-core/issues/444)). A height says where a block sits in one
chain. Only a matching hash proves that two chains share a block.

**Options considered.**

| Option | Consequence |
| --- | --- |
| Fixed window, wait for growth (today) | Loses the `-1` signal and treats an unproved window edge as the fork point. |
| Bounded parent-hash walk with distinct outcomes | Proves the fork point by hash, stops loudly when it cannot. Needs stored ancestry and a header-by-hash call. |
| Unbounded walk, or restart from genesis | A pruning error or a source fault turns into a replay of the whole history. BDK's emitter does this without a signal. |

**Decision.** The reader asks for the stored tip by hash. On `confirmations: -1` it commits the
recovery barrier at once. It walks parent hashes back on both branches until they name the same block
and proves that block before any destructive undo. Three outcomes stay distinct: source unavailable,
insufficient local history, and beyond the rollback-search limit. The limit is **100 old-branch
blocks, inclusive**. A fork point exactly 100 blocks below the stored tip is accepted if the other
repair conditions pass. At 101, or when a needed stored header is missing, the barrier stays and
recovery stops loudly with the last proved facts. Forward catch-up does not count toward the limit.
The limit is separate from the maturity depth and from the repair boundary. It limits search cost and
is not a safety guarantee.

**Who does it this way.** LDK's [`find_difference_from_header`][ldk-walk-rc3] walks back the side with
the greater height, or both at equal height, until the hashes match (MIT or Apache-2.0). ord returns a
typed [`Unrecoverable`][ord-bound] outcome and stops when it cannot recover (CC0-1.0). Alpen returns a
typed `PivotNotFound` beyond its history ([query][alpen-query], MIT or Apache-2.0), but retries it
forever with only a log line, which Via rejects. BDK's [`poll_once`][bdk-poll-once] restarts from
genesis when it finds no agreement point, which Via also rejects.

**Why it works better.** The fork point is proved by hash instead of assumed at a window edge. A
shrinking tip is handled at once. A fork that Via cannot handle produces a named stop that an alert
can report, instead of a silent restart from a wrong height. The limit of 100 keeps today's search
cost. On testnet4 the deepest reorg in about 25 months was 73 blocks
([#441](https://github.com/vianetwork/via-core/issues/441)).

Status: decided in [#438](https://github.com/vianetwork/via-core/issues/438), recorded in
[ADR 0008](../../adr/0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md). The current detectors
still get regression tests under [#444](https://github.com/vianetwork/via-core/issues/444).

## Observe at the tip, act only at maturity

**Today.** The reader reads only up to `tip - block_confirmations`
([main node][via-main-scan], [verifier][via-verifier-scan]) and acts on everything it reads. The
repository's base configuration sets `block_confirmations = 0`
(`etc/env/base/via_btc_watch.toml`), and the deployed testnet sets 12.

**Why it breaks.** One number does two jobs. Reading 12 blocks behind the tip delays the moment the
reader sees a fork, so detection needs a separate detector task, the split that caused the batch 265
skip. Acting on everything read assumes 12 blocks are final. On testnet4, reorgs of 12 blocks or more
happened 73 times in 90 days, about 0.8 times a day, and the deepest reached 73 blocks.

**Options considered.**

| Option | Consequence |
| --- | --- |
| Read only to `tip - N` and act on everything read (today, Citrea's DA service, ZKsync Era `eth_watch`) | Simple. Sees forks late and treats N blocks as final. |
| Observe at the tip, act behind a per-network maturity depth (Clementine, Alpen, Babylon, LDK) | Forks are visible at once. Irreversible actions wait. Every action must be classified. |
| Act at the tip and compute confirmations at query time (BDK, NBXplorer) | Right for wallet balances, which can change back. Wrong for votes and payouts, which cannot. |

**Decision.** The reader follows the tip and records only reversible facts there. Votes,
finalization and payouts wait until their block reaches the network's maturity depth. Observing,
adding a preventive hold and revoking authority whose source left the chain happen immediately. The
rollback-search limit is a separate setting.

**Who does it this way.** Clementine's syncer [stores blocks and events at once][clementine-scan] and
its consumers [act only at `finality_depth`][clementine-consumer] (GPL-3.0, idea only). Alpen gates
actions through one predicate, [`is_l1_reorg_safe`][alpen-maturity] (MIT or Apache-2.0). Babylon waits
before an action grants power and acts at once on one that revokes (BSL 1.1, idea only). LDK matures
its on-chain events after `ANTI_REORG_DELAY` (MIT or Apache-2.0).

**Why it works better.** Forks are visible as soon as the authoritative node sees them, so the reader
itself detects reorgs and no separate detector runs ahead of it. The protection against acting on a
replaced block is an explicit depth on each action, not a side effect of where reading stops. The
cost is that the reader sees every shallow reorg: on testnet4 recovery runs about 15 times a day and
becomes a routine path.

Status: decided in [#435](https://github.com/vianetwork/via-core/issues/435), recorded in
[ADR 0009](../../adr/0009-observe-at-the-tip-and-act-behind-maturity-depth.md).

## Maturity depth per network: 6 on mainnet, 100 on test networks

**Today.** Every deployed network uses 12 for both the reader and the sender. No design record
explains 12. A deployment configuration change on 24 December 2025 raised the testnet reader and
sender values from 6 to 12 without a stated reason, most likely because of testnet4's reorgs. The
guide `docs/via_guides/reorg.md` still says deposits wait for 6 confirmations. The two uses also
count differently. The reader first reads a block when it has 13 confirmations, because it stops at
`tip - 12`. The sender's `check_tx_confirmation` in `core/lib/via_btc_client/src/client/mod.rs`
compares Bitcoin Core's inclusive count, so it completes at 12.

**Why it breaks.** 12 is too shallow for testnet4, which exceeds it about once a day. It is deeper
than any mainnet requirement shows: the deepest mainnet reorg recorded since April 2024 replaced 2
blocks. Two counting rules for one number invite off-by-one errors. A sender that completes at 12
while actions wait for 100 would let a shallower check stand in for a deeper one.

**Options considered.**

| Option | Consequence |
| --- | --- |
| 12 on every network | Testnet4 exceeds it about daily. The two counting rules remain. |
| Citrea's table unchanged: 6 mainnet, 100 testnet4, 5 signet | Signet's 5 is below signet's recorded reorgs of 19 and 22 blocks. |
| 12 mainnet, 100 testnet4, 100 signet, 1 regtest | First decision. Keeps a mainnet value that no record justifies. |
| 6 mainnet, 100 testnet4, 100 signet, 1 regtest | Chosen by amendment. |
| A lower testnet4 value, 6 to 30 (as Clementine 6, Rootstock testnet 10, Alpen signet 6) | About 1 to 5.5 hours instead of 18, but the 50 to 59 block testnet4 reorgs of 2026 would each stop recovery for manual repair. |
| A separate depth per action | Settings with no requirement behind them. |

**How the testnet4 value was chosen.** The 18-hour wait was weighed explicitly. Via's earlier testnet
values (0 from April 2025, then 3, 6 and 12 during 2025) never had a recorded reason. Citrea reached
100 by the same path Via would follow: 8 at first, 30 on 13 December 2024 after a 12-block reorg, and
100 on 25 April 2025 after a 30-block reorg, accepting "a correspondingly longer latency" in its
documentation. Projects that pick a low test-network value accept frequent deep-reorg handling. For
Via, a reorg deeper than the maturity depth means an irreversible action rests on a vanished block and
recovery stops for an operator. On testnet4, where reorgs of 50 or more blocks still occur several times
a year, that costs more than the slower feedback. The structural fix is a test network without the
minimum-difficulty rule (BIP 95 testnet5, still a draft, or signet).

**Decision.** Count inclusively: the inclusion block is confirmation 1, and a block at height `h` is
mature when it is on the current chain and `tip - h + 1 >= N`.

| Network | Maturity depth N |
| --- | --- |
| mainnet | 6 |
| testnet4 | 100 |
| signet, if used | 100 |
| regtest, isolated tests only | 1 |

One maturity policy per network drives both actions that grant authority and the sender's
completion of its own transactions.

**Who does it this way.** Citrea's `bitcoin-da` crate keeps a [per-network table][citrea-depth] with
6 on mainnet and 100 on testnet4, and computes an [inclusive finalized height][citrea-height] (MIT or
Apache-2.0). Alpen's [`is_l1_reorg_safe`][alpen-maturity] uses 6 and returns nothing for a height above
the tip (MIT or Apache-2.0). Clementine counts the inclusion block as confirmation 1
([`is_block_finalized`][clementine-confirmations], GPL-3.0, idea only). LDK uses
`ANTI_REORG_DELAY = 6` (MIT or Apache-2.0). Six mainnet confirmations is the long-used Bitcoin convention.

**Why it works better.** Each network's depth is written down with one counting rule, so the reader,
the action jobs and the sender agree. Mainnet deposits, finalization and withdrawals wait about half
as long as under 12. Testnet4's 100 exceeds every recorded testnet4 reorg. The cost on testnet4 is
latency: it produces a block about every 11 minutes, so 100 blocks is about 18 hours. The values are
policy. Historical depths do not bound future reorgs, and signet's operator once produced a
163-block branch by mistake.

Status: decided in [#452](https://github.com/vianetwork/via-core/issues/452) with the mainnet
amendment, recorded in [ADR 0009](../../adr/0009-observe-at-the-tip-and-act-behind-maturity-depth.md).
The guide `docs/via_guides/reorg.md` changes when the policy is implemented.

## Each scan group commits its effects with its own scan position

**Today.** The verifier runs every message processor inside one scan transaction, under the
`lock_btc_watch` lock, and fetches blocks over RPC while that lock is held. A proof whose parent batch
is missing raises `ProofContinuityGap` and stops every kind of message, deposits and withdrawals
included. The withdrawal observer needs its own commit boundary, so the scan on `main` stops at the
first withdrawal block ([#442](https://github.com/vianetwork/via-core/issues/442)). The zk verifier
reads a stored proof, verifies it and saves a verdict without taking the reader's lock. Its only
guard is a check of `via_l1_reorg` just before commit (`via_verifier/node/via_zk_verifier/src/lib.rs`).

**Why it breaks.** PostgreSQL's default isolation level, Read Committed, gives each statement its own
view of committed data and does not lock what a plain read sees. The zk verifier can read "no
recovery", recovery can then delete and re-insert the proof row and commit, and the zk verifier then
commits a verdict on the re-inserted row. Moving the check inside the same transaction does not change
that. A long transaction with RPC inside also holds the lock for as long as Bitcoin takes to answer.

**Options considered.**

| Option | Consequence |
| --- | --- |
| One long transaction with RPC inside (today) | Atomic, but lock hold time depends on the Bitcoin node, and one failure stops everything. |
| Two scan groups | A proof gap still stops deposits and configuration. |
| One scan group per message type | Needs buffering of pending events across groups, which nothing requires. |
| Four groups, short transactions, RPC outside, final write under a lock held through commit | Effects and position commit together. Other writers cannot commit across a recovery. |
| A recovery generation counter checked without a lock | Read Committed lets recovery commit between the check and the write. |

**Decision.** The verifier has four scan groups: configuration (bridge wallets and protocol
upgrades), deposits, proofs with votes, and withdrawal observations. Each commits its effects and its
own `(height, hash)` scan position together, forward and on undo. Configuration is a prerequisite for
the other groups, not a second ordering owner. `via_l1_reorg` stays the committed recovery barrier,
set before the first destructive step and cleared in a final transaction once the affected stores
agree. The final write of outside work, such as a zk verdict, takes the source gate
(`lock_source`), checks all its inputs again and holds the gate through commit. Withdrawal
observation continues during a proof pause. The signing messages do not change. Nodes upgrade by
stopping every writer of a database, migrating and restarting.

Each scan group starts after migration from a position that retained evidence proves. What can be
proved migrates. A group that cannot be proved stops the migration, and an operator chooses an
explicit baseline, such as rebuilding its reversible state from a proved block. Today's hash at an
old height is never taken as proof, and executed, signed or paid state is never inferred. Most
legacy rows store only a height, so testnet4 is expected to need that operator baseline once.
`pg_rewind` [refuses without a common point][pg-rewind-ancestor] and Debezium
[refuses to resume without its position history][debezium-position], the same stance.

**Who does it this way.** Graph Node commits a consumer's effects and head in one locked transaction,
[forward][graph-apply] and [backward][graph-undo] (MIT or Apache-2.0). Its lock is held per session,
while Via's advisory lock is held per transaction, so Via adopts the principle and keeps its own lock.
Via's own [`lock_source`][via-lock-source] already takes a shared advisory lock for writers that
depend on a source and an exclusive lock for invalidation. Chainlink LogPoller keeps RPC outside its
[write transaction][chainlink-insert] (MIT). Citrea's transaction monitor
[rechecks the tip before it installs work][citrea-monitor] (MIT or Apache-2.0). PostgreSQL documents
the [Read Committed][pg-isolation] behavior and [advisory locks][pg-locks].

**Why it works better.** A proof gap stops only proofs and votes. Deposits, configuration and
withdrawal observations keep going. Stale outside work cannot land on a row that recovery replaced,
because the lock, not a flag read, keeps recovery out until commit. Locks are no longer held across
Bitcoin RPC calls.

Status: decided in [#434](https://github.com/vianetwork/via-core/issues/434), recorded in
[ADR 0010](../../adr/0010-commit-each-scan-group-with-its-position.md).

## The shared reader lives in `via_reorg` and copies LDK's fork walk

**Today.** Each tree has its own reader, detector, reverter and startup code. The only shared reorg
code is `core/lib/via_reorg`, a dependency-free crate with `scan_for_reorg` and
`reorg_window_start`. The inscription indexer sits inside the Bitcoin client crate
`core/lib/via_btc_client`. The two `MessageProcessor` traits differ by DAL type and error type.

**Why it breaks.** Every fix must land twice and stay identical. ADR 0007 and PR #404 applied the same
rules to both trees by hand. `AGENTS.md` asks that shared behavior live in `core/lib/` and that
node-specific scheduling, state and shutdown stay in the node layer.

**Options considered.**

| Option | Consequence |
| --- | --- |
| Keep two node-specific readers | Continues the duplication that let the trees drift. |
| Put recovery into `via_btc_client` | Mixes recovery and database code into the transport client every Bitcoin caller uses. |
| A new crate such as `via_btc_reader` | A second owner of the same comparison, with no benefit. |
| Grow the existing `via_reorg` crate, with node adapters | One implementation, node-specific wiring stays in the node layer. |
| Depend on LDK `lightning-block-sync` | Its walk is crate-private behind callbacks that cannot return an error or carry a transaction. Pulls in the whole `lightning` crate. |
| Depend on BDK `bdk_bitcoind_rpc` | Blocking, bypasses Via's client, restarts from genesis without a signal. |
| Depend on BDK `bdk_chain` | Fits technically, but duplicates the SQL ancestry, has no parent hashes, and its next line needs Rust 1.85. |

**Decision.** The reader grows inside `core/lib/via_reorg`: block identities, full ancestry, ordered
apply and undo, and per-group scan positions, without importing either node's DAL. Each node supplies
fallible adapters that prepare inputs outside the transaction and apply one prepared group inside it.
Scheduling, shutdown and processor composition stay in the node layer. LDK's fork walk is copied with
its loop and traversal order unchanged. Types, lookups, errors and the limit are adapted. There is no
BDK or LDK runtime dependency. Headers come from the authoritative node through `via_btc_client` with
hash, height, parent hash and canonical membership, checked for identity and parent linkage. Raw
`bitcoincore-rpc` types stay inside the client. Independent proof-of-work validation is outside this
redesign. The old window API is removed when its callers move.

**Who does it this way.** The copied code is LDK's `ChainDifference`, `find_difference_from_header`
and `look_up_previous_header`, [lines 329 to 457 of `lightning-block-sync/src/lib.rs` at
`v0.3-rc3`][ldk-walk-rc3], licensed MIT or Apache-2.0 at the user's option. It is about 30 lines of
logic. LDK and BDK both separate the block source from the chain logic: LDK through
[`BlockSource`][ldk-source], BDK through its emitter and `bdk_chain`. The
[library fit section](../../research/bitcoin-reorg-handling.md#library-fit-for-the-shared-reader) of
the research note records the dependency probes.

**Why it works better.** One implementation of reading, fork search and recovery serves every node,
so a fix lands once. The copied loop is small and tested, and Via keeps control of its errors and
transactions. A dependency would need adapter code larger than the copy. The archived RPC types stay
inside the client, as C5 requires.

Status: decided in [#433](https://github.com/vianetwork/via-core/issues/433), with
[#450](https://github.com/vianetwork/via-core/issues/450) as evidence, recorded in
[ADR 0011](../../adr/0011-shared-reader-in-via-reorg-with-copied-ldk-walk.md).

## Test on one chain model and four real Bitcoin Core cases

**Today.** No test creates a real reorg: nothing calls `invalidateblock`, `reconsiderblock` or
`preciousblock`. Three separate mock chains exist, in `core/node/via_btc_watch/src/test/`,
`via_verifier/node/via_btc_watch/src/test/` and `via_verifier/node/via_reorg_detector/src/tests.rs`.
The main-node detector has no tests. The manual two-node procedure in `docs/via_guides/test-reorg.md`
and `etc/via_reorg/reorg.sh` has no assertions and does not run in CI. Pull requests run no Rust
tests, and `.github/workflows/tests-via.yml` runs only when started by hand
([#445](https://github.com/vianetwork/via-core/issues/445)).

**Why it breaks.** The withdrawal stall in [#442](https://github.com/vianetwork/via-core/issues/442)
reached `main` with no failing check. The batch 265 skip was found on testnet after 11 days. No
fixture models `confirmations: -1`, so the authority rule could not be tested at all.

**Options considered.**

| Option | Consequence |
| --- | --- |
| Separate mocks per crate (today) | Three models to keep consistent, no real RPC coverage. |
| One mock chain only | Deterministic, but nothing checks the model against real Bitcoin Core. |
| `invalidateblock` cases only | Forced invalidation skips validation paths a real fork takes. |
| Two-node Docker tests only | Fixed waits and peer-to-peer races hide the step that failed. |
| One shared mock chain plus four real Core cases | Deterministic edge cases and a real check of the RPC boundary and chain selection. |

**Decision.** The three chain fixtures merge into one model in `core/lib/via_test_utils`, keeping
`BitcoinRpcServer` as the transport fixture. The maintainer approved merging this main-node and
verifier test code. LDK's eight applicable fork cases are copied, and BDK's PR #2167 restart
regression is adapted. Node-specific database assertions stay with their owners. Four regtest cases
run against real Bitcoin Core: R1 disconnect-only shrink with `invalidateblock`, R2 a natural
competing fork built with Core's fork helper translated to Rust, R3 a fork beyond the
rollback-search limit, and R4 a re-mined message plus a reader restart. These checks are required on
reader pull requests. CI provides a pinned Core binary and a disposable database and fails visibly if
it cannot.

**Who does it this way.** LDK's [`Blockchain` fixture][ldk-fixture-rc3] and
[fork tests][ldk-tests-rc3] (MIT or Apache-2.0). BDK's [restart regression][bdk-regression] from
[PR #2167](https://github.com/bitcoindevkit/bdk/pull/2167), merged on 2026-04-24, which fixed the same
failure class as the batch 265 skip (MIT or Apache-2.0). Bitcoin Core
[PR #32587](https://github.com/bitcoin/bitcoin/pull/32587), merged on 2025-11-26, which replaced
`invalidateblock` simulations with real competing forks built by [`create_empty_fork`][core-fork]
(MIT). The `bitcoind` test crate from [rust-bitcoin `corepc`][corepc-manager] (MIT) is checked for fit
before the build.

**Why it works better.** One model replaces three, every rule in ADR 0008 has a deterministic case,
and four cases check the real RPC boundary. A reader change cannot merge without these checks.

Status: decided in [#437](https://github.com/vianetwork/via-core/issues/437), recorded in
[ADR 0012](../../adr/0012-test-reorgs-on-one-chain-model-and-real-bitcoin-core.md). Running Rust
tests on pull requests is [#445](https://github.com/vianetwork/via-core/issues/445).

## PR #398 lands first, with the shared lock

**Today.** PR #398 records only completed proof verdicts. It changes `via_verifier_storage_init` and
adds a zk verifier transaction whose final write checks the reorg flag before commit. Its ADR is
numbered 0007, which `main` already uses.

**Why it matters.** The flag-then-commit write races the reverter under Read Committed, for the reason
in [scan groups](#each-scan-group-commits-its-effects-with-its-own-scan-position). Merge order alone
does not make it safe.

**Options considered.** Merge it unchanged, which ships the race. Fold it into the redesign by default,
which mixes a verdict-policy change into the redesign review. Fix it and merge it first.

**Decision.** Rebase PR #398 on `main`, make its final verdict write take the shared `lock_source`
lock and check its inputs again before commit, renumber its ADR, then merge it before the redesign
build. Fold it into the redesign only if the lock cannot be added cleanly on its own.

**Who does it this way.** No studied project supplies a merge-order rule. The locking pattern is
Graph Node's [locked write][graph-apply] and Via's own [`lock_source`][via-lock-source].

**Why it works better.** The verdict policy stays reviewable on its own, and the replay rule gets a
fixed verdict model to build on.

Status: decided in [#431](https://github.com/vianetwork/via-core/issues/431). This is a planning
decision with no ADR. The ADR numbers in this record may shift when PR #398 takes its number.

## Every processor keeps evidence, canonical membership and derived state apart

**Today.** A reorg that removes only deposits not yet in a batch is soft on the main node and batch 0
on the verifier. Neither path deletes those deposits, and replay skips the re-mined deposit, so a
deposit that vanished from Bitcoin stays queued. The verifier's deposit insert ignores a conflict on
its key ([`ON CONFLICT (tx_id) DO NOTHING`][via-deposit-insert]) and never compares the payload. Wallet
rows from replaced blocks keep their height, and the current wallet is chosen by `created_at`
([wallet lookup][via-wallet-order]), which has the same value for every row written in one scan
transaction. The verifier's upgrade upsert sets [`executed = FALSE`][via-upgrade-upsert], so replaying
the latest upgrade block marks an executed upgrade as not executed
([#443](https://github.com/vianetwork/via-core/issues/443)).

**Why it breaks.** Recovery deletes by batch, and these rows are not in a batch yet. Most processor
tables do not record which block produced a row
([#449](https://github.com/vianetwork/via-core/issues/449)). Only proofs, votes and withdrawals carry a
block hash. A deposit's identity encodes its Bitcoin position, so a re-mined deposit is a different
L2 operation. Two wallet rotations in one chunk mishandle the second, even without a reorg. On the
main node, sender request and history rows cascade-delete each other, so a rollback that deletes one
removes signed history, which C2 forbids. A rule that only asks for idempotent writes leaves vanished
deposits queued. A rule that only undoes replaced blocks fails when recovery crashes between undo and
redo, and a key-only check lets a changed payload through either way.

**Options considered.**

| Option | Consequence |
| --- | --- |
| Idempotent inserts only | Re-reading converges, but effects of abandoned blocks stay. |
| Undo only | A crash between undo and the next read leaves the gap. A key-only check still lets a changed payload through. |
| Delete everything the replaced range produced | Destroys payment and signing evidence and triggers the C2 cascades. |
| Restore the whole store to a snapshot | Also rolls back sender history. |
| Key-only idempotency (today's deposit insert) | A changed payload under the same key passes without an error. |
| Separate evidence, membership and derived state; compare full payloads on replay; withdraw membership of replaced blocks | Exact undo, evidence kept. Needs provenance per table and targeted schema changes. |
| Order configuration by `created_at` (today) | Ties within one transaction and depends on when a node wrote the row. |
| Allow several changes for one role in one block, applied in transaction order | Needs successive versions inside one block. Not chosen. |
| Reject two conflicting changes for one role in one block | One meaning per role per block. Changes which messages Via accepts. |

**Decision.** Every processor keeps evidence, canonical membership and derived state apart. Recovery
withdraws the membership of the replaced blocks, unbatched deposits included, and rebuilds derived
state. Evidence, holds, sender requests and signed history stay. On replay, a processor compares the
full canonical payload under the same identity. The result is `exists` or
`exists_with_different_<field>`, and a changed payload is a conflict, never an overwrite. A
withdrawal of membership is a conditional update that changes a row only once. Wallet selection and
the queue of unexecuted deposits are rebuilt from evidence. Each table has one undo mechanism. A
re-mined deposit links to the old one by `(txid, vout)`, and a deposit already executed on L2 falls
under the [repair stop](#recovery-repairs-only-reversible-provable-steps-and-inspection-is-read-only).

Configuration changes are ordered by block hash, height, transaction index and message order, never
by `created_at`. A change at height `h` takes effect at `h + 1`. More than one change for the same
role in one block is rejected, even when the changes are identical, and none of them takes effect. **This changes which messages Via
accepts.** The wire bytes do not change. The parser records transaction indices for system messages.

The finalization that the reader decides moves to its own record, off the sender row. Sender
requests and signed history survive recovery, and the foreign-key cascades that delete them are
replaced.

**Who does it this way.** NBXplorer keeps transaction identity, blocks and
[inclusion][nbx-inclusion] in separate tables and clears only the inclusion on a disconnect (MIT,
adapt). TigerBeetle's [`create_transfer_exists`][tb-exists] compares a stored transfer field by field
and returns `exists` or the first `exists_with_different_<field>` ([tests][tb-exists-tests], Zig,
Apache-2.0, idea). Formance ledger v2 [reverts a transaction][formance-revert] with
`UPDATE … WHERE reverted_at IS NULL RETURNING` and reports whether a row changed (Go, MIT, idea).
etcd [rejects a transaction that writes one key twice][etcd-dupkey] before it applies any write
([test][etcd-dupkey-test], Go, Apache-2.0, idea, with the role as the key), and orders writes by
[revision and sub-revision][etcd-revision], not by time (adapt). Apache Fineract
[reverses with a linked opposite entry][fineract-reversal] and keeps the original (Java, Apache-2.0,
adapt). ZKsync Era's upgrade processor [skips an upgrade older than the last seen version][era-upgrade]
(MIT or Apache-2.0, adapt), the rule that fixes the #443 shape. Graph Node keeps versioned entities
and [reopens older versions on revert][graph-entities] (MIT or Apache-2.0). Via's own verifier
withdrawal tables already keep observations, per-block inclusions, holds and fulfillments apart.

**Why it works better.** A deposit that vanished from Bitcoin leaves the queue. Replaying an upgrade
block keeps its `executed` state. The current wallet no longer depends on when a node inserted a row.
Signed history survives a rollback. A changed payload under a known identity stops the group with a
named conflict, instead of being overwritten or skipped. One block can no longer give one role two
meanings that only `created_at` separates.

Status: decided in [#439](https://github.com/vianetwork/via-core/issues/439) and
[#446](https://github.com/vianetwork/via-core/issues/446) as one decision, recorded in
[ADR 0013](../../adr/0013-keep-evidence-membership-and-derived-state-apart.md).

## Recovery repairs only reversible, provable steps, and inspection is read-only

**Today.** The verifier reverter has no floor and may revert finalized batches, as
`docs/via_guides/reorg.md` describes. The 100-block window is not a depth limit. Via's copy of ZKsync
Era's block reverter dropped Era's read-only `print-suggested-values` command. The verifier server
has no component selection, while the main `via_server` has the upstream `--components` flag. The
main-node reverter [skips its executed-batch guard][via-reverter-guard] whenever a reorg row exists,
and it [deletes snapshot files after its SQL commit][via-reverter-files], so a crash between the two
leaves files that no later run deletes.

**Why it breaks.** Automatic repair is safe only when the reader can prove where the fork starts,
still has the data it needs, and knows that every processor's undo and replay is correct. Without
those, it can delete valid state or replay effects already sent to Bitcoin. A numeric depth proves
none of these, and a database `executed` flag does not show what other parties already consumed. A
"dry run" that writes and then rolls back is not read-only in PostgreSQL:
[sequence values stay consumed][pg-sequence], Via uses `BIGSERIAL` keys, the run takes production locks, and it cannot undo an external
action another task already started.

**Options considered.**

| Option | Consequence |
| --- | --- |
| Automatic replay within a numeric depth | Depth does not prove retained history or the absence of external effects. |
| Stop at any finalized batch | Contradicts the current verifier guide, freezes reversible finality repair, and still misses earlier signing and payment risk. |
| Repair reversible state only with a proved fork point, retained data and complete undo and replay, otherwise a typed stop that names the failed condition | Repairs the common case and stops loudly on the rest. Needs every step classified. |
| Run the full node in a rolled-back transaction as a dry run | Not read-only, and cannot contain external effects. |
| Thread a runtime dry-run flag through the normal writers | Every writer must remember to check the flag. |
| A stored effect digest now | Useful only after its field set, history baseline and encoding are defined. |

**Decision.** Every recovery step belongs to one of four classes: **reversible** derived state,
**revocable authority**, **external effect** (signed, broadcast or paid) and **executed L2 state**.
Recovery is automatic only for steps that are reversible or revocable and provable: a proved fork
point, retained history, a complete undo for every affected processor, and stores in agreement.
External effects and executed L2 state are never undone automatically. When a condition fails,
recovery stops loudly with the failed condition and the last proved identities, and keeps the
barrier. Revoking stale authority and adding holds are always allowed.

Inspection is a separate read-only path. It runs in a PostgreSQL `READ ONLY` transaction and wires no
sender, coordinator or signer. Recovery applies a plan only if the identities it was planned against
still hold, and refuses otherwise. A mutation rehearsal runs only on a disposable database copy and
is never called read-only. No general effect digest is added yet.

The main-node reverter records the snapshot files to delete in the same transaction as the revert,
then deletes them, and a crash resumes from that record. The barrier clears only when every recorded
deletion is done, so recovery waits while object storage is down. Main-node hard reorgs are rare, so
the wait costs little, and nothing has to prove that a stale snapshot is never served. The manual
main-node reverter stays as the only operator route for hard repairs, with the same inspection and
identity checks as the reader. Its executed-batch guard loses the reorg-row bypass. An override for
executed state is explicit, separately authorized and never part of automatic recovery.

**Who does it this way.** PostgreSQL's `pg_rewind` [finds the common ancestor][pg-rewind-ancestor]
from timeline identity and start position and fails when there is none (PostgreSQL licence, adapt).
etcd Raft [panics when a conflict reaches a committed entry][raft-committed] (Go, Apache-2.0, adapt).
Kafka followers [find the divergence by leader epoch][kafka-truncate] (Apache-2.0, adapt), and Via
rejects Kafka's fallback to the high watermark when no epoch is known. Debezium
[fails with an operator message][debezium-position] when its stored position is gone (Apache-2.0,
adapt). Apache Fineract [adds a linked opposite entry][fineract-reversal] and TigerBeetle
[corrects a posted transfer with a new one][tb-correct] instead of erasing the original (Apache-2.0,
adapt). Kubernetes server-side dry run [reads, checks and computes without a storage
write][k8s-dryrun] ([tests][k8s-dryrun-tests], Go, Apache-2.0, adapt as a separate path, not a flag).
In a PostgreSQL `READ ONLY` transaction, [`nextval` on a permanent sequence fails][pg-readonly-seq],
so an inspection that tries to allocate a key fails loudly. etcd [compares revisions before it
writes][etcd-compare], and Kubernetes [rejects an update whose resource version changed][k8s-cas]
(Apache-2.0, adapt as guarded SQL writes that refuse instead of retrying). Bitcoin Core's
[`VerifyDB`][core-verify] checks blocks through a detached in-memory view (MIT, adapt). Terraform
[rejects a stale saved plan][terraform-plan], but its code is BSL 1.1, so it is an idea only. ord
stops with [`Unrecoverable`][ord-bound] (CC0-1.0), and ZKsync Era [refuses to revert executed
batches by default][era-reverter] (MIT or Apache-2.0). Kubernetes keeps an object until every
[recorded finalizer][k8s-finalizer] is removed, and Flink [records completion durably][flink-complete]
before it reports it (Go and Java, Apache-2.0, adapt as recorded deletions). No studied project runs real recovery writes
on a disposable copy as a feature. That part is operations practice.

**Why it works better.** The repair boundary is a list of effect classes, not a depth or a database
flag. Stale authority is withdrawn at once, while signed and paid history stays. A repair that cannot
be proved produces a named stop that an alert can report. An operator can see the planned change
before anything writes, and a plan that went stale is refused instead of applied to a different
state.

Status: decided in [#436](https://github.com/vianetwork/via-core/issues/436), recorded in
[ADR 0014](../../adr/0014-repair-only-reversible-provable-steps-and-inspect-read-only.md).

## The verifier set at a proof's inclusion block decides its finalization

**Today.** A governance-signed `UpdateBridge` message included at height `h` activates a new verifier
set from `h + 1`. Three components pass three different sets when they count votes: the reader uses
the set at the vote's block, the reverter the set at the reorg start minus one
(`via_verifier/node/via_block_reverter/src/lib.rs`), and the zk verifier a copy taken at process start
(`via_verifier/node/via_zk_verifier/src/lib.rs`). Main-node finalization is never recomputed after a
reorg, and the main node counts votes per batch instead of per proof
([#454](https://github.com/vianetwork/via-core/issues/454)).

**Why it breaks.** After a reorg, a replay, or simply a verdict that arrives after the deciding vote,
the same batch can be final in one component and not in another. With two stored votes, a
three-member set reaches the threshold while a five-member set does not. The finalized batches form a
prefix only because one vote is in flight at a time (C4).

**Options considered** ([#451](https://github.com/vianetwork/via-core/issues/451)).

| Option | Consequence |
| --- | --- |
| The set at each vote's block (today) | Votes for one proof can be judged by different sets, and replay can change the result. |
| The set at process start, or at the replay tip | Neither replays the same way twice. |
| The set at the first vote | Ties membership to whichever vote lands first, not to the proof. |
| An explicit epoch field in Via messages | Changes the wire format. |
| A joint quorum of old and new sets | Solves replicas switching at different times, which Via does not have. |
| The set active at the proof's inclusion block | One answer for every component. Changes how votes across a rotation are read. |
| Unlimited authority for a retired set, or a fixed expiry | Unbounded, or a time rule with no requirement behind it. |
| No rotation while a proof is unfinalized | Bounds a retired set's authority without a time rule. |

**Decision.** The verifier set active at a proof's canonical inclusion block decides both membership
and the denominator for that proof, in every component: the reader, recovery recomputation and the zk
verifier's final write. With `h + 1` activation, the set for a proof in block `p` is the configuration
through block `p - 1`. A missing set is an error, never a fallback to the latest set. One shared tally
counts each verifier once for one exact proof and set, which fixes #454. Each role keeps its own
proof-verification prerequisite. The verifier set may not rotate while a proof is unfinalized. An
`UpdateBridge` message that Bitcoin includes while an earlier proof is unfinalized is rejected for
good: it stays as evidence but never takes effect, and governance sends a new message after the
proof is finalized. Each node decides this from the same Bitcoin prefix. Holding the rotation and
activating it at finalization was not chosen, because its activation point would depend on
finalization timing instead of the message's own block. **This changes consensus interpretation
across rotations and which messages Via accepts.**

**Who does it this way.** CometBFT loads the validator set for a height with
[`LoadValidators(height)`][comet-load], returns an error when it is missing, and verifies a commit
against [one set for signers and threshold][comet-verify], rejecting duplicate voters (Go, Apache-2.0,
adapt). CometBFT activates validator changes at `h + 2` because each header commits to the next set.
Via has no such commitment and keeps `h + 1`. Lighthouse selects an attestation's committee from
[the target it names][lighthouse-target], never from the head, and bounds old authority to the current and previous epoch
(Rust, Apache-2.0, idea only). ZooKeeper [fixes the quorum on each proposal][zookeeper-quorum] when it
is created (Java, Apache-2.0, idea only). Raft [refuses a configuration change while an earlier one is
pending][raft-config] and drops it instead of queueing it (Go, Apache-2.0, idea only). No donor
decides Via's anchor. Choosing the proof's
inclusion block is Via protocol policy.

**Why it works better.** Every component reaches the same verdict for the same proof, after a
restart, a replay or a reorg. A vote for a different proof of the same batch no longer counts toward
it. A retired set cannot finalize an old proof indefinitely.

Status: decided in [#440](https://github.com/vianetwork/via-core/issues/440), with
[#451](https://github.com/vianetwork/via-core/issues/451) as evidence, recorded in
[ADR 0015](../../adr/0015-verifier-set-at-proof-inclusion-decides-finalization.md).

## Cadence and committed-state metrics

**Cadence.** The reader catches up chunk after chunk without waiting a tick between chunks. When it
is level with the tip, it makes one cheap `getbestblockhash` comparison per existing idle interval. A
failed step retries on that same interval. No new deployment setting is added. electrs
[catches up in a loop][electrs-sync] and Bitcoin Core's index framework [syncs without a fixed
wait][core-index-sync] (MIT, adapt).

**Metrics.** Gauges report only committed state: each group's scan position and lag, the recovery
phase, the proof-continuity pause, reorg depth and count, and classified errors. A reporter reads them
from the database, as Via's existing `L1BatchMetricsReporter` in `house_keeper` does, and registers
them in the shared metrics crate. Graph Node likewise [reports a head only after it commits][graph-apply]
(MIT or Apache-2.0, adapt). Sender rows no longer report progress. Metric names are contracts, so
dashboards and alerts in the infrastructure repository change with them.

## Reused code and how it is checked

Most donors in this record are adapted: Via takes the mechanism and writes its own code. One unit is
taken as code:

| Unit | Donor | Language | Licence | How Via takes it | Record |
| --- | --- | --- | --- | --- | --- |
| Fork walk: `ChainDifference`, `find_difference_from_header`, `look_up_previous_header` | [LDK `lightning-block-sync` at `v0.3-rc3`][ldk-walk-rc3] | Rust | MIT or Apache-2.0 | Copied. Loop and traversal order unchanged. | [ADR 0011](../../adr/0011-shared-reader-in-via-reorg-with-copied-ldk-walk.md) |

Three short rules are Via's own code with the idea from a donor: the field-wise replay comparison
from [TigerBeetle][tb-exists], the conditional membership withdrawal from
[Formance ledger v2][formance-revert] and the duplicate check from [etcd][etcd-dupkey], with the role
as the key ([ADR 0013](../../adr/0013-keep-evidence-membership-and-derived-state-apart.md)). Each is a
few lines, so Via writes it in Rust, cites the donor in an origin comment and ports the donor's test
cases. No licence text is added for them.

The copied walk is checked this way:

1. It carries an origin comment with the pinned permalink and every change from the donor, so a
   reviewer can tell an intended change from a copy error. LDK's licence is the one Via already
   ships, and LDK has no NOTICE file, so no notice file or licence header is added.
2. An independent reviewer, who did not write the change, compares it line by line against the donor
   at the pinned commit.
3. The donor's tests are ported to Rust and must pass. These are the fork cases in
   [ADR 0012](../../adr/0012-test-reorgs-on-one-chain-model-and-real-bitcoin-core.md).
4. A check confirms that the copied loop is textually unchanged from the pinned donor.
5. A manifest records the donor repository, path, commit and file hash. A weekly check opens an issue
   when the donor file changes, so a fix upstream is not missed. The pattern follows Microsoft
   testfx's vendored-files manifest. Both checks are a `provenance` subcommand of the existing Rust
   `via_duplication` tool. Rust decodes the manifest with every field required, so an entry missing
   its commit or path fails instead of passing as empty.

Formal verifiers (Kani, Verus, Creusot, Prusti) and research translators (VERT, Syzygy, AlphaTrans,
the DARPA TRACTOR tools) were considered and not used: the units are small, and the translators
target C, Java or WebAssembly. jscpd 5's experimental cross-language similarity mode can pair Go
with Rust and may be run by hand, but it is not a gate.

GPL, LGPL and BSL sources, such as Clementine, Fulcrum, Babylon, the sBTC signer and Terraform, are
ideas only. No code from them is copied.

## Work outside the redesign

The Bitcoin client fix lands first as its own pull request from `main`: fetch only the hash per height
in the detectors ([#409](https://github.com/vianetwork/via-core/issues/409)), measure RPC duration
per method ([#408](https://github.com/vianetwork/via-core/issues/408)), and stop RPC calls from
blocking async workers ([#406](https://github.com/vianetwork/via-core/issues/406)). Bugs found during
the research are fixed separately: the verifier scan stops at the first withdrawal block
([#442](https://github.com/vianetwork/via-core/issues/442), a release blocker), replaying an upgrade
block resets it ([#443](https://github.com/vianetwork/via-core/issues/443)), a shrinking tip and deep
reorgs go undetected ([#444](https://github.com/vianetwork/via-core/issues/444)), pull requests run no
Rust tests ([#445](https://github.com/vianetwork/via-core/issues/445)), and the main node counts votes
for different proofs of one batch together ([#454](https://github.com/vianetwork/via-core/issues/454)).

Out of scope: stuck transactions, vote pipelining, replacing the archived RPC client, moving the
public testnet to signet, pipeline dashboards, `via_indexer`, `via_consistency_checker` and the DA
clients. A shared Bitcoin context in coordinator and verifier signing messages, and a strict drain of
in-flight work during recovery, are separate protocol decisions if they are ever required.

## Not yet specified

These items are not specified yet. Each is added to this record when it is decided.

| Item | Waits on | Current direction |
| --- | --- | --- |
| Fetching the block body by its checked hash | Implementation design | Fetch the header, check its parent, then fetch the body by hash. Most studied readers do this. Citrea's DA service and Alpen fetch by height. |
| A stored effect digest | Deferred by ADR 0014 | Add only if replay must prove equal effects beyond identity keys and undo. ZKsync OS compares a stored output hash, but it covers a defined VM output, not Via's tables. |
| A consumer in another process | C7 | It keeps its own `(height, hash)` position, commits effects and position together, and reconciles its branch after a missed reorg. |

[core-index-startup]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/index/base.cpp#L105-L148
[core-index-sync]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/index/base.cpp#L208-L341
[core-membership]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/rpc/blockchain.cpp#L155-L203
[core-fork]: https://github.com/bitcoin/bitcoin/blob/79d6e874e1dad6822b5d7e6993aa205d21ed2cbf/test/functional/test_framework/blocktools.py#L119-L137
[bdk-emitter]: https://github.com/bitcoindevkit/bdk/blob/d02f8909171af695fcb078c6fca082833f835b86/crates/bitcoind_rpc/src/lib.rs#L362-L469
[bdk-poll-once]: https://github.com/bitcoindevkit/bdk/blob/5dc6e959379982a1661e8f57c3ee7af334627ca5/crates/bitcoind_rpc/src/lib.rs#L277-L329
[bdk-regression]: https://github.com/bitcoindevkit/bdk/blob/d02f8909171af695fcb078c6fca082833f835b86/crates/bitcoind_rpc/tests/test_emitter.rs#L671-L709
[ldk-source]: https://github.com/lightningdevkit/rust-lightning/blob/0a2b003e7e1602df8933b15b7563ba8b6e198391/lightning-block-sync/src/lib.rs#L62-L89
[ldk-poll]: https://github.com/lightningdevkit/rust-lightning/blob/0a2b003e7e1602df8933b15b7563ba8b6e198391/lightning-block-sync/src/lib.rs#L270-L290
[ldk-startup]: https://github.com/lightningdevkit/rust-lightning/blob/0a2b003e7e1602df8933b15b7563ba8b6e198391/lightning-block-sync/src/init.rs#L142-L225
[ldk-walk-rc3]: https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/lib.rs#L329-L457
[ldk-tests-rc3]: https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/lib.rs#L620-L790
[ldk-fixture-rc3]: https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/test_utils.rs#L22-L218
[corepc-manager]: https://github.com/rust-bitcoin/corepc/blob/8d82c88aebf8268bfc9732aaa15e64a5bba1f9a0/bitcoind/Cargo.toml#L1-L53
[esplora-shrink]: https://github.com/Blockstream/electrs/blob/0963ef518a69f884496e1caf08315df874c9bf13/src/util/block.rs#L136-L185
[bindex-shrink]: https://github.com/romanz/bindex-rs/blob/ea8854a0c9b8535179ff92179bf4d3e38bfef82d/bindex-lib/src/chain.rs#L208-L240
[fulcrum-shrink]: https://github.com/cculianu/Fulcrum/blob/02e22da92355893d8696305f0741acb7a8bc61f4/src/Controller.cpp#L1151-L1187
[ord-bound]: https://github.com/ordinals/ord/blob/df660674bf78d95db866376aec377def38df3a88/src/index/reorg.rs#L25-L48
[nbx-inclusion]: https://github.com/btcpayserver/NBXplorer/blob/b691999b14eb2a06547ffa708c11574fa323d769/NBXplorer/DBScripts/FullSchema.sql#L32-L102
[citrea-depth]: https://github.com/chainwayxyz/citrea/blob/2ed54463bb7c4c3f1ff29dcd66d4b28c26fb63c8/crates/bitcoin-da/src/network_constants.rs#L6-L59
[citrea-height]: https://github.com/chainwayxyz/citrea/blob/2ed54463bb7c4c3f1ff29dcd66d4b28c26fb63c8/crates/bitcoin-da/src/service.rs#L824-L840
[citrea-monitor]: https://github.com/chainwayxyz/citrea/blob/2ed54463bb7c4c3f1ff29dcd66d4b28c26fb63c8/crates/bitcoin-da/src/monitoring.rs#L648-L733
[clementine-schema]: https://github.com/chainwayxyz/clementine/blob/b61efacf56b4196cda0023419e01d4b0d6f2bc5d/core/src/database/schema.sql#L100-L147
[clementine-scan]: https://github.com/chainwayxyz/clementine/blob/b61efacf56b4196cda0023419e01d4b0d6f2bc5d/core/src/bitcoin_syncer.rs#L440-L466
[clementine-consumer]: https://github.com/chainwayxyz/clementine/blob/b61efacf56b4196cda0023419e01d4b0d6f2bc5d/core/src/bitcoin_syncer.rs#L469-L596
[clementine-confirmations]: https://github.com/chainwayxyz/clementine/blob/b61efacf56b4196cda0023419e01d4b0d6f2bc5d/crates/clementine-config/src/protocol.rs#L165-L173
[alpen-query]: https://github.com/alpenlabs/alpen/blob/882107abd8ec691f870dced0a680267124dd8294/crates/btcio/src/reader/query.rs#L504-L581
[alpen-maturity]: https://github.com/alpenlabs/alpen/blob/882107abd8ec691f870dced0a680267124dd8294/crates/primitives/src/l1/mod.rs#L5-L31
[reth-notifications]: https://github.com/paradigmxyz/reth/blob/f88813adb966324ecfc530a2f78df38ed0df5750/crates/exex/types/src/notification.rs#L10-L61
[era-upgrade]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_watch/src/event_processors/decentralized_upgrades.rs#L94-L113
[era-reverter]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/block_reverter/src/lib.rs#L119-L180
[graph-apply]: https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs#L1151-L1214
[graph-undo]: https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs#L1330-L1368
[graph-entities]: https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/relational.rs#L1076-L1119
[chainlink-shrink]: https://github.com/smartcontractkit/chainlink-evm/blob/ea52d84d2f7aecf8b3dfb9cc88db1ab6e6cd6ba2/pkg/logpoller/log_poller.go#L1204-L1238
[chainlink-insert]: https://github.com/smartcontractkit/chainlink-evm/blob/ea52d84d2f7aecf8b3dfb9cc88db1ab6e6cd6ba2/pkg/logpoller/orm.go#L583-L619
[pg-isolation]: https://www.postgresql.org/docs/18/transaction-iso.html
[pg-locks]: https://www.postgresql.org/docs/18/explicit-locking.html
[via-reorg]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/lib/via_reorg/src/lib.rs#L1-L67
[via-window]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/lib/config/src/configs/via_reorg_detector.rs#L17-L26
[via-main-scan]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/node/via_btc_watch/src/lib.rs#L101-L140
[via-verifier-scan]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/node/via_btc_watch/src/lib.rs#L180-L257
[via-lock-source]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/lib/verifier_dal/src/via_votes_dal.rs#L20-L59
[via-deposit-insert]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/lib/verifier_dal/src/via_transactions_dal.rs#L47
[via-wallet-order]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/lib/verifier_dal/src/via_wallet_dal.rs#L53-L77
[via-upgrade-upsert]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/lib/verifier_dal/src/via_protocol_versions_dal.rs#L41-L45
[via-reverter-guard]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/node/via_block_reverter/src/lib.rs#L106-L149
[via-reverter-files]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/node/via_block_reverter/src/lib.rs#L152-L175
[tb-exists]: https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig#L3971-L4033
[tb-exists-tests]: https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine_tests.zig#L1768-L1812
[tb-correct]: https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/docs/coding/recipes/correcting-transfers.md#L1-L34
[formance-revert]: https://github.com/formancehq/ledger/blob/3ab99b3ad28ffcc6034d94b4e3d831b823cce7a5/internal/storage/ledger/transactions.go#L154-L213
[etcd-dupkey]: https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/server/etcdserver/api/v3rpc/key.go#L236-L314
[etcd-dupkey-test]: https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/tests/integration/clientv3/txn_test.go#L32-L44
[etcd-revision]: https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/server/storage/mvcc/kvstore_txn.go#L174-L245
[etcd-compare]: https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/server/etcdserver/txn/txn.go#L32-L101
[raft-committed]: https://github.com/etcd-io/raft/blob/1931ff5beacc1e2539003c0c70f096285f04c425/log.go#L107-L167
[fineract-reversal]: https://github.com/apache/fineract/blob/71c39730e9178301b7ab949ba711238ce81fc66d/fineract-provider/src/main/java/org/apache/fineract/accounting/journalentry/service/JournalEntryWritePlatformServiceImpl.java#L318-L408
[pg-rewind-ancestor]: https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/bin/pg_rewind/pg_rewind.c#L1052-L1085
[pg-readonly-seq]: https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/backend/commands/sequence.c#L649-L660
[kafka-truncate]: https://github.com/apache/kafka/blob/65566690ca1ddb584388c1026e2ff6673e9c6f19/core/src/main/scala/kafka/server/AbstractFetcherThread.scala#L489-L541
[debezium-position]: https://github.com/debezium/debezium/blob/215ffa1ad3f13a484b11c8deb9600a05d9ef08cf/debezium-connector-common/src/main/java/io/debezium/connector/common/BaseSourceTask.java#L146-L167
[k8s-dryrun]: https://github.com/kubernetes/apiserver/blob/9c4ec15b8038e9f47e8caf219903649dc67c992a/pkg/registry/generic/registry/dryrun.go#L30-L107
[k8s-dryrun-tests]: https://github.com/kubernetes/apiserver/blob/9c4ec15b8038e9f47e8caf219903649dc67c992a/pkg/registry/generic/registry/dryrun_test.go#L131-L196
[k8s-cas]: https://github.com/kubernetes/apiserver/blob/9c4ec15b8038e9f47e8caf219903649dc67c992a/pkg/registry/generic/registry/store.go#L873-L896
[terraform-plan]: https://github.com/hashicorp/terraform/blob/7a5717566e57a9d8705bed20fe199a0785ba0416/internal/backend/local/backend_local.go#L282-L345
[comet-load]: https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/state/store.go#L548-L615
[comet-verify]: https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/types/validation.go#L65-L138
[lighthouse-target]: https://github.com/sigp/lighthouse/blob/e423a66763bb1bd780492d635123f208d80c3538/beacon_node/beacon_chain/src/attestation_verification.rs#L1580-L1622
[zookeeper-quorum]: https://github.com/apache/zookeeper/blob/777b3723f56456524a052b79f73649e99da1f1aa/zookeeper-server/src/main/java/org/apache/zookeeper/server/quorum/Leader.java#L1316-L1325
[core-verify]: https://github.com/bitcoin/bitcoin/blob/cce32b16874e85e8a00cfa1a2190f0b6f4dcaf66/src/validation.cpp#L4627-L4760
[pg-sequence]: https://www.postgresql.org/docs/18/functions-sequence.html
[k8s-finalizer]: https://github.com/kubernetes/apiserver/blob/9c4ec15b8038e9f47e8caf219903649dc67c992a/pkg/registry/generic/registry/store.go#L691-L708
[flink-complete]: https://github.com/apache/flink/blob/14fa27183139cb9ec0a5f2b34f5d72f7477115c1/flink-runtime/src/main/java/org/apache/flink/runtime/checkpoint/CheckpointCoordinator.java#L1363-L1441
[raft-config]: https://github.com/etcd-io/raft/blob/1931ff5beacc1e2539003c0c70f096285f04c425/raft.go#L1310-L1345
[electrs-sync]: https://github.com/romanz/electrs/blob/35216c6d30148be8e6763d913d437330f431fc03/src/index.rs#L169-L258
