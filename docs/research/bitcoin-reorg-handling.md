# Bitcoin reorg handling

Status: research completed on 2026-10-22 for the
[Bitcoin reader redesign map](https://github.com/vianetwork/via-core/issues/426). This note records
primary-source findings: how Bitcoin Core exposes reorgs, how deep reorgs go on the networks Via uses,
how Via reads Bitcoin today and where that breaks, and how established Bitcoin indexers, wallets and
layer 2 systems handle each part of the problem. It does not accept a design. The
[Bitcoin reader redesign record](../design/bitcoin-reader-redesign/README.md) explains the options and
the decisions. ADRs [0008](../adr/0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md),
[0009](../adr/0009-observe-at-the-tip-and-act-behind-maturity-depth.md),
[0010](../adr/0010-commit-each-scan-group-with-its-position.md),
[0011](../adr/0011-shared-reader-in-via-reorg-with-copied-ldk-walk.md),
[0012](../adr/0012-test-reorgs-on-one-chain-model-and-real-bitcoin-core.md),
[0013](../adr/0013-keep-evidence-membership-and-derived-state-apart.md),
[0014](../adr/0014-repair-only-reversible-provable-steps-and-inspect-read-only.md) and
[0015](../adr/0015-verifier-set-at-proof-inclusion-decides-finalization.md) accept the decisions. [ADR 0007](../adr/0007-serialize-verifier-scans-and-cap-recovery.md) owns the earlier fix for
the batch 265 skip. Nothing here proves a live deployment or a recovery.

## Question

How does Bitcoin reorganize its chain as seen through Bitcoin Core's RPC interface, how deep do
reorgs go on the networks Via uses, how does Via read Bitcoin today and where does that break, and
how do established projects handle each part of the problem? The parts are ownership, detection,
scan positions and transactions, undo and provenance, replay, the depth at which a node observes
versus acts, the stop after a reorg deeper than the supported limit, writers outside the reader,
library reuse and tests.

## Source revisions

Via code is cited at `main`
[`6bcdc07a6ad2605c5fc7124836a923a14a75954c`](https://github.com/vianetwork/via-core/tree/6bcdc07a6ad2605c5fc7124836a923a14a75954c).
Comparison projects were read at the commits below. No comparison code was built or run, except the
dependency probes described under [Library fit](#library-fit-for-the-shared-reader). Their tests were
inspected, not executed. Licences come from each repository's licence files and crate metadata. A
path-specific licence takes precedence over the workspace default.

| Project | Revision | Licence |
| --- | --- | --- |
| Bitcoin Core | `bitcoin/bitcoin@e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6`, `cce32b16874e85e8a00cfa1a2190f0b6f4dcaf66` for the index header and ZMQ files, and `79d6e874e1dad6822b5d7e6993aa205d21ed2cbf` for the functional-test fork helper | MIT |
| BIPs | `bitcoin/bips@8c4d9e258b8cec913ffe4bddeb1d96e9f0c01ae5`, BIP 95 at `c134c7db552d26801ba2ef86e00c50febe843718` | per BIP |
| BDK | `bitcoindevkit/bdk@d02f8909171af695fcb078c6fca082833f835b86` (master) and `5dc6e959379982a1661e8f57c3ee7af334627ca5` (releases `chain-0.23.4`, `core-0.6.4`, `bitcoind_rpc-0.22.1`) | MIT or Apache-2.0 |
| LDK `lightning-block-sync` | `lightningdevkit/rust-lightning@0a2b003e7e1602df8933b15b7563ba8b6e198391`, and `d7a980641c6325ddda5072a0f470ef8392676cf5` (tag `v0.3-rc3`) for the code chosen for copying | MIT or Apache-2.0 |
| rust-bitcoin `corepc` | `rust-bitcoin/corepc@8d82c88aebf8268bfc9732aaa15e64a5bba1f9a0` (`bitcoind` 0.41.0) | MIT |
| electrs | `romanz/electrs@35216c6d30148be8e6763d913d437330f431fc03` (v0.11.1) | MIT |
| Esplora electrs | `Blockstream/electrs@0963ef518a69f884496e1caf08315df874c9bf13` | MIT |
| bindex | `romanz/bindex-rs@ea8854a0c9b8535179ff92179bf4d3e38bfef82d` | MIT |
| Fulcrum | `cculianu/Fulcrum@02e22da92355893d8696305f0741acb7a8bc61f4` | GPL-3.0-or-later |
| ord | `ordinals/ord@df660674bf78d95db866376aec377def38df3a88`, tests at `fd152bf94d814c7b158853438bc6a1c6a9d37096` | CC0-1.0 |
| NBXplorer | `btcpayserver/NBXplorer@b691999b14eb2a06547ffa708c11574fa323d769` | MIT |
| Citrea | `chainwayxyz/citrea@2ed54463bb7c4c3f1ff29dcd66d4b28c26fb63c8` | GPL-3.0-only, except `crates/bitcoin-da`, which is MIT or Apache-2.0 |
| Clementine | `chainwayxyz/clementine@b61efacf56b4196cda0023419e01d4b0d6f2bc5d` | GPL-3.0 |
| Alpen | `alpenlabs/alpen@882107abd8ec691f870dced0a680267124dd8294` | MIT or Apache-2.0 |
| Babylon | `babylonlabs-io/babylon@0de36afadb84fec624c80b07d743a0d9f5341722` | BSL 1.1 |
| Rootstock | `rsksmart/rskj@ecef55ddf2b79fe5cd84c824118117dd825744b2` | LGPL-3.0-or-later |
| sBTC signer | `stacks-network/sbtc@ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef` | GPL-3.0 |
| Reth execution extensions | `paradigmxyz/reth@f88813adb966324ecfc530a2f78df38ed0df5750` | MIT or Apache-2.0 |
| ZKsync Era | `matter-labs/zksync-era@ff5f519b11cff863edcfa0f75af10fea113806b0` | MIT or Apache-2.0 |
| ZKsync OS server | `matter-labs/zksync-os-server@e29a2c0a584b7f3f985e9a414bfbe179456c093d` | MIT or Apache-2.0 |
| Optimism `op-node` | `ethereum-optimism/optimism@1f94d97c7dd870bbacc24b5f962959529506225d` | MIT |
| Graph Node | `graphprotocol/graph-node@6838f4e3cf3607c5bc32ef705dd05aec1eb057ab` | MIT or Apache-2.0 |
| Chainlink LogPoller | `smartcontractkit/chainlink-evm@ea52d84d2f7aecf8b3dfb9cc88db1ab6e6cd6ba2` | MIT for `pkg/logpoller` |
| PostgreSQL | documentation for version 18 | PostgreSQL |

Via is MIT or Apache-2.0. Only MIT, Apache-2.0 and CC0 sources can be copied or translated into Via
code. GPL, LGPL and BSL sources are recorded as ideas only.

## How a reorg looks through Bitcoin Core's RPC

A Bitcoin block header names its parent. Bitcoin Core stores that field as
[`hashPrevBlock`][core-header], and rust-bitcoin exposes it as `prev_blockhash`. A block is
identified by its hash. A height identifies a position in one particular chain, and two chains can
hold different blocks at the same height.

A node follows the valid chain with the most cumulative work. Core orders candidate tips with
[`CBlockIndexWorkComparator`][core-work-order]: first by `nChainWork`, then by the order in which the
node received the blocks. [`FindMostWorkChain`][core-most-work] selects the best candidate. Two
consequences follow. A shorter chain with more work replaces a longer one, which matters on
testnet4 (see below). Two honest nodes can disagree about which of two equal-work branches is
active, because each prefers the branch it saw first.

Four RPCs expose the active chain and its forks:

| RPC | What it returns at the pinned revision | What it means for a reader |
| --- | --- | --- |
| [`getblockhash h`][core-getblockhash] | The hash at height `h` in the active chain at the moment of the call. It fails with "Block height out of range" when `h` is above the active tip. | Two calls can answer from different branches. A query for a stored height fails when the active chain becomes shorter than what the reader stored. |
| [`getblockheader hash`][core-getblockheader] | For a known block, `confirmations` is computed by [`ComputeNextBlockAndDepth`][core-membership]: `tip height - block height + 1` on the active chain, `1` for the tip, and `-1` for a known block that is not on the active chain. An unknown hash fails with "Block not found". | `confirmations: -1` is positive evidence that the node has left the stored block. A lower height and an unknown hash are different observations and prove less. |
| [`getchaintips`][core-getchaintips] | Every known tip, with `branchlen`, the height difference between the tip and its fork point on the active chain, and a `status`. `valid-fork` marks a fully validated branch that is no longer active. `valid-headers` marks a branch whose blocks were never fully validated. | A `valid-fork` tip is a branch the node once had active and then left. Its `branchlen` measures the depth of that reorg against today's active chain. |
| [`getbestblockhash`][core-getbestblockhash] | The hash of the active tip. | One call is enough to see whether anything changed since the last poll. |

Core's ZMQ `hashblock` notification [skips a tip update that only disconnects
blocks][core-zmq]. A reader that relies on that notification does not learn about a shrinking tip,
so polling stays necessary for that case.

### Why testnet4 reorganizes deeply

Testnet4 keeps the testnet rule that a block whose timestamp is more than 20 minutes after its
parent may use the minimum difficulty. Core implements it in
[`GetNextWorkRequired`][core-min-difficulty], enabled for testnet4 by
[`fPowAllowMinDifficultyBlocks = true`][core-testnet4-params], and
[BIP 94][bip94-rule] keeps the rule on purpose. Its rationale states that "a block mined at actual
difficulty could easily replace even multiple 20-minute exception blocks" ([BIP 94][bip94-rationale]).
A run of difficulty-1 blocks adds almost no work, so one real-difficulty block mined on an older
parent replaces the whole run. Depth in blocks is large while depth in work is small.

Every Bitcoin Core change proposed to remove the exception was closed without merging:
[#31117](https://github.com/bitcoin/bitcoin/pull/31117),
[#34420](https://github.com/bitcoin/bitcoin/pull/34420) and
[#35081](https://github.com/bitcoin/bitcoin/pull/35081). Testnet 5,
[BIP 95][bip95], is a draft that removes the exception. Its Core implementation,
[#35861](https://github.com/bitcoin/bitcoin/pull/35861), was open on 2026-10-22.

## Measured reorg depths per network

[Issue #441](https://github.com/vianetwork/via-core/issues/441) records the measurement. mempool.space
publishes the raw `getchaintips` output of its Bitcoin Core node for each network at
`/api/v1/chain-tips`. Each `valid-fork` tip was counted as one reorg of depth `branchlen`. The
testnet4 figures were cross-checked against mempool.ninja, which runs a separate node. Snapshots were
taken on 2026-10-22 at testnet4 height 157,244, signet height 327,409 and mainnet height 972,546.

| Network | Window | Reorgs | Typical depth | Deepest | Reorgs of 12 or more blocks |
| --- | --- | --- | --- | --- | --- |
| testnet4 | last 90 days | about 1,430, about 15 a day | median 2, 90% at 8 or less, 99% at 23 or less | 50 | 73, about 0.8 a day |
| testnet4 | last 365 days | about 4,940 | median 2, 99% at 24 or less | 59 (56 on the second node) | 288 |
| testnet4 | September 2024 to October 2026 | about 17,500 | 61% at depth 1 | 73 (2025-03-20) | none reached 100 |
| signet | last 90 days | 112, all depth 1 | 1 | 1 | 0 |
| signet | last 365 days | 186 | 99% at 9 or less | 19 (2026-01-19) | 1 |
| signet | February 2024 to October 2026 | 193 | mostly 1 | 22 (2024-02-18) | 3 |
| mainnet | last 365 days | 15 | 1 | 2 | 0 |
| mainnet | April 2024 to October 2026 | 36 | 1 | 2 (height 941,882, 2026-03-23) | 0 |

The deep testnet4 events are network-wide. Of the reorgs of depth 20 or more on the second node, 81
of 88 have the same tip hash on mempool.space. Testnet4 also had 1,422 competing branches in the
last 90 days that this node never activated, the longest 83 blocks. A node that received them first
could have reorganized through them. Testnet4 produced 12,107 blocks in the last 90 days, about one
block every 11 minutes.

Signet behaves differently because one operator signs its blocks. Since June 2026 the operator
creates about one deliberate one-block reorg a day
([Delving Bitcoin, "Regular signet reorgs"](https://delvingbitcoin.org/t/regular-signet-reorgs/2549)).
That thread also reports a misfire that produced branches of 163 and 148 blocks around height
307,510. The mempool.space node never recorded those branches. Signet is therefore not a stand-in
for testnet4, and its sample does not bound what its operator can do. Via's own one-block reorg at
testnet4 height 156,770 matches one-block forks the public node recorded at 156,769 to 156,771.

The method has limits. `branchlen` is measured against today's active chain, so a later and deeper
reorg below the same fork point makes an older branch look longer. For one event it is an upper
bound. Each node sees only the forks it received while it was online, and equal-work ties make two
nodes go through different reorgs. Coverage starts when the node's data directory was created. The
counts are branches, not timed events, and no public dataset records testnet4 reorgs with event times.
The figures are historical evidence. They do not bound future reorgs, and nothing in testnet4's
rules prevents a reorg of 100 blocks or more.

## How Via reads Bitcoin today

Each node runs four separate tasks that touch the Bitcoin chain. On the main node and the external
node they are the reader `core/node/via_btc_watch`, the reorg detector
`core/node/via_main_node_reorg_detector`, the reverter `core/node/via_block_reverter` and the startup
initializer `core/node/via_node_storage_init`. On the verifier and the coordinator they are
`via_verifier/node/via_btc_watch`, `via_verifier/node/via_reorg_detector`,
`via_verifier/node/via_block_reverter` and `via_verifier/node/via_verifier_storage_init`. Both trees
use the shared client in `core/lib/via_btc_client`, which also contains the inscription indexer. The
only shared reorg code is [`core/lib/via_reorg`][via-reorg], a dependency-free crate whose
`scan_for_reorg` returns the first height at which stored and fetched hashes differ.

The detector stores one row per block it has walked in `via_l1_blocks`, with height and hash. Each
tick it re-fetches the last [`reorg_window()`][via-window] heights, a hard-coded 100, through
[`fetch_block_window`][via-fetch-window], and compares hash by height
([`detect_reorg`][via-detect]). The window fetch downloads full blocks by height and uses only their
hashes. A mismatch writes a `via_l1_reorg` row. That row pauses the reader, the senders, the
aggregator, the zk verifier, the coordinator and the DA dispatcher until the reverter has deleted
the effects of the replaced blocks and moved the scan position back.

The reader starts at its scan position plus 1. It stops at the lowest of the scan position plus
`L1_BLOCKS_CHUNK` (50), the node's tip minus `block_confirmations`, and the detector's last stored
block ([verifier `process_scan`][via-verifier-scan], [main-node `process_scan`][via-main-scan]). It
fetches each block by height ([`process_block`][via-process-block]), runs the message processors and
saves the new scan position. Since [ADR 0007](../adr/0007-serialize-verifier-scans-and-cap-recovery.md),
the reader, detector, reverter and startup code take one PostgreSQL advisory lock
(`lock_btc_watch`), the scan and its scan position commit in one transaction, and recovery only moves
the scan position back.

### Where the number 12 comes from

The repository's base configuration sets `block_confirmations = 0` for the reader
([`via_btc_watch.toml`][via-base-confirmations]) and for the sender
([`via_btc_sender.toml`][via-sender-confirmations]). The deployed testnet uses 12 for both. No design
record explains 12. A deployment configuration change on 24 December 2025 raised the testnet reader
and sender values from 6 to 12 without a stated reason, most likely because of testnet4's reorgs
([#452](https://github.com/vianetwork/via-core/issues/452)). The implemented-behavior guide
[`docs/via_guides/reorg.md`](../via_guides/reorg.md) still states that deposits wait for 6
confirmations. No mainnet deployment exists.

The reader and the sender count differently. The reader processes blocks up to
`tip - block_confirmations`, so with 12 a block is first read when it has 13 confirmations, counting
the inclusion block as the first. The sender's [`check_tx_confirmation`][via-sender-check] compares
Bitcoin Core's inclusive `confirmations` value with the setting, so it completes at 12. The reader
also acts on everything it reads, so one number serves as both the observation horizon and the
action horizon.

### Known failure modes

The following failure modes are known at `6bcdc07a6`. All except the first were found by reading
code and are not reproduced by a test.

| Failure | Mechanism | Tracked in |
| --- | --- | --- |
| Recovery skipped unread blocks | The reverter set the scan position to the block before the reorg, which was ahead of where the reader was. Batch 265's proof was skipped this way. | [ADR 0007](../adr/0007-serialize-verifier-scans-and-cap-recovery.md), fixed |
| A shrinking tip is not detected | The window fetch up to the stored height fails when the node's tip is lower, and the detector treats every fetch error as inconclusive. | [#444](https://github.com/vianetwork/via-core/issues/444) |
| A reorg deeper than the window is reported at the wrong height | Only 100 heights are compared, so a deeper fork is reported at the window start, with no error. At stored height 200 with a real fork point at 50, the detector reports 101. | [#444](https://github.com/vianetwork/via-core/issues/444) |
| Detection downloads full blocks | Each window height costs `getblockhash` plus `getblock`, and only the hash is used. | [#409](https://github.com/vianetwork/via-core/issues/409) |
| RPC calls block async workers | Synchronous RPC runs on the async runtime, so parallel fetches run one at a time and timeouts cannot fire. No metric shows call duration per method. | [#406](https://github.com/vianetwork/via-core/issues/406), [#408](https://github.com/vianetwork/via-core/issues/408) |
| The verifier scan stops at the first withdrawal | `record_withdrawal_observation` refuses to run inside a transaction ([`withdrawals_dal.rs`][via-withdrawal-guard]), and the scan runs every processor inside one. | [#442](https://github.com/vianetwork/via-core/issues/442) |
| Replaying an upgrade block resets it | The verifier compares the upgrade version with the compiled version ([`governance_upgrade.rs`][via-upgrade-skip]), and the upsert sets `executed = FALSE`. | [#443](https://github.com/vianetwork/via-core/issues/443) |
| Deposits and wallet rows from replaced blocks survive | A reorg that removes only unbatched deposits is soft, no path deletes them, and replay skips the re-mined deposit. Wallet rows keep their height, and the current wallet is chosen by `created_at`. | [#428](https://github.com/vianetwork/via-core/issues/428), [#439](https://github.com/vianetwork/via-core/issues/439) |
| Three verifier counts decide finalization | The reader, the reverter and the zk verifier each read the verifier set at a different point. | [#451](https://github.com/vianetwork/via-core/issues/451), [#440](https://github.com/vianetwork/via-core/issues/440) |
| The main node counts votes per batch, not per proof | Votes are stored per proof transaction, but the count filters by batch number only. Votes for an earlier proof of the same batch can count with votes for a later one. | [#454](https://github.com/vianetwork/via-core/issues/454) |
| The zk verifier writes without the reader's lock | It checks the `via_l1_reorg` flag just before commit ([`via_zk_verifier`][via-zk-final]), which does not stop recovery committing in between. | [#428](https://github.com/vianetwork/via-core/issues/428), [#434](https://github.com/vianetwork/via-core/issues/434) |
| Startup can move the scan position forward | In both trees, with `restart_indexing` set, startup overwrites the scan position with the configured start block ([main node][via-restart-indexing], [verifier][via-restart-indexing-verifier]). | [#432](https://github.com/vianetwork/via-core/issues/432) |
| Blocks are read by height | A block fetched by height after detection can come from a different branch than the one the detector checked. | [#426](https://github.com/vianetwork/via-core/issues/426) |
| No stored ancestry | `via_l1_blocks` is sparse since PR #365 and has no parent hashes, so it cannot prove a fork point. | [#432](https://github.com/vianetwork/via-core/issues/432) |
| No real reorg test | No test calls `invalidateblock`, three separate mock chains exist, and pull requests run no Rust tests. | [#430](https://github.com/vianetwork/via-core/issues/430), [#445](https://github.com/vianetwork/via-core/issues/445) |

### What the processor tables record

[#449](https://github.com/vianetwork/via-core/issues/449) classified every table a Via processor writes
per Bitcoin block. A row can hold immutable evidence (what Bitcoin carried), canonical membership
(whether that evidence is on the accepted chain) or derived state (state computed from both).
Only two families keep the three apart: the verifier withdrawal tables and the verifier proof and
vote rows written since migration `20260924`. Every other processor table mixes all three in one row.

Block hashes reach only proofs, votes and bridge withdrawals. Deposits, wallet changes and upgrades
are processed by height only. A deposit's `priority_id` packs height, transaction index and output
index, and the L2 transaction hash derives from it, so a deposit re-mined at another position is a
different L2 operation. System messages carry no transaction index, and protocol upgrades store no
Bitcoin txid, height or hash. On the main node, `via_btc_inscriptions_request` and
`via_btc_inscriptions_request_history` cascade-delete each other, so deleting one confirmed history
row removes its request and every other signed attempt. For migration, block hashes exist only for
the newer verifier proof, vote and withdrawal rows. Today's hash at an old height does not prove
which block produced an old row.

### How the verifier set is chosen

[#451](https://github.com/vianetwork/via-core/issues/451) traced the rule in code. A governance-signed
`UpdateBridge` message included at height `h` activates the new verifier set from `h + 1`. The set is
applied per vote: each vote is admitted under the set at its own block. The reader, the reverter and
the zk verifier pass three different sets as the denominator: the set at the vote's block, the set
at the reorg start minus one, and a copy taken at process start. The verifier stops at a bridge
rotation on `main`, because a set change arrives only with a new bridge address. No inscription
carries a set identity or epoch.

## What established projects do

Each topic below states the observed behavior, then a table with the source and a copy verdict.
"Copy 1:1" means an unchanged copy of permissively licensed Rust code with its notices. "Translate"
means a 1:1 translation into Rust. "Adapt" means the mechanism transfers but the code must change for
Via's PostgreSQL transactions, processors or trust model. "Idea only" means no code may be taken,
because of the licence or because the shape does not fit. Verdicts are research proposals. The ADRs
record which ones Via accepted.

### Ownership: one ordering owner, several consumer positions

Every studied reader with stateful consumers keeps reading, detection and recovery in the process
that holds the state. Bitcoin Core's index framework runs [startup][core-index-startup] and
[catch-up][core-index-sync] inside `BaseIndex`, and its validation interface delivers ordered
callbacks per subscriber but [no ordering between subscribers][core-callbacks]. LDK's
[`synchronize_listeners`][ldk-startup] keeps a separate block locator for each listener and
disconnects each one to its own agreement point. Graph Node keeps one head per subgraph and commits
a consumer's writes and head together ([apply][graph-apply], [rewind][graph-undo]). Reth's execution
extensions report a `FinishedHeight` that may fall during a reorg ([notifications][reth-notifications]).
No studied project recovers outside the process that holds the consumer's state.

| Source | Observed behavior | Licence | Verdict |
| --- | --- | --- | --- |
| Bitcoin Core `BaseIndex` | Each index owns its database, locator and sync thread. Startup restores the saved hash and rewinds if it is off-chain. | MIT | Adapt the lifecycle |
| LDK `init.rs` | One block source, one locator per listener, shared fetches during reconnection. | MIT or Apache-2.0 | Adapt |
| Graph Node | Per-consumer head, effects and head in one locked transaction in both directions. | MIT or Apache-2.0 | Adapt |
| Reth execution extensions | Consumer head as height and hash, committed separately by the consumer. | MIT or Apache-2.0 | Adapt the event cases |
| Clementine [`bitcoin_syncer_event_handlers`][clementine-schema] | One durable progress row per consumer, advanced in the consumer's transaction. | GPL-3.0 | Idea only |

### Detection: parent hash at the frontier, then a walk to a proved ancestor

No studied reader re-downloads a fixed window of full blocks to find a fork. LDK compares the new
tip with its last known header by hash and chainwork, checks each walked header's parent link in
[`check_builds_on`][ldk-check], and [walks the higher side back][ldk-walk] until both sides meet.
`op-node` checks the next block's parent in [`AdvanceL1Block`][op-frontier] before fetching its
receipts. ord compares the stored hash at `h - 1` with the incoming block's parent
([`reorg.rs`][ord-bound]). Chainlink LogPoller does the same and then
[searches stored blocks for the common ancestor][chainlink-detect].

For a shrinking tip, the projects split. BDK walks its checkpoints and treats
[`confirmations < 0` as stale][bdk-emitter]. Esplora electrs handles
[a lower known tip with no new headers][esplora-shrink], including a manual `invalidateblock`.
bindex [drops stale tips][bindex-shrink] when its header request returns nothing. Fulcrum
[undoes a block][fulcrum-shrink] when its stored tip is above the node's tip. LDK, in steady state,
[ignores a tip with less work][ldk-poll], and LogPoller [waits for the chain to grow][chainlink-shrink].
Alpen separates a source that is [a lagging prefix of the stored chain][alpen-query], where it waits,
from a diverged chain, where it reverts. Both behaviors are right for their trust model. LDK and
Alpen distrust or tolerate a lagging source. Fulcrum, bindex and Esplora trust their own node.

| Source | Observed behavior | Licence | Verdict |
| --- | --- | --- | --- |
| LDK fork walk and header checks | Height-aware walk to the common ancestor, parent and chainwork checks per header. | MIT or Apache-2.0 | Copy 1:1 the walk. The proof-of-work checks are a separate choice |
| `op-node` `AdvanceL1Block` | Parent check before the data fetch, with distinct reset, temporary and end-of-data errors. | MIT | Translate the check and its outcome table |
| Bitcoin Core `ComputeNextBlockAndDepth` | `-1` for a known block off the active chain. | MIT | Adapt as the stale-tip rule |
| Esplora electrs, bindex | Act on a lower tip without waiting for a new block. | MIT | Adapt, bounded |
| Alpen pivot search | Lagging prefix versus divergence, typed `PivotNotFound` beyond history. | MIT or Apache-2.0 | Adapt the classification |
| Fulcrum | Undo on a lower tip by height alone. | GPL-3.0-or-later | Idea only. Its height-only rule is wrong for a lagging source |
| LDK steady-state poll, LogPoller | Ignore or wait on a shorter chain. | MIT or Apache-2.0, MIT | Reject for a trusted Via node |

### Scan positions and transactions

Readers that keep effects and position in one store commit them together. electrs writes a chunk of
rows and its tip key in one batch ([`index.rs`][electrs-fetch]). Graph Node commits a consumer's
effects and head in one locked PostgreSQL transaction. LogPoller writes blocks and logs in one
transaction ([`InsertLogsWithBlocks`][chainlink-insert]) and keeps RPC outside it. The sBTC signer
fetches a block [before it opens the transaction][sbtc-fetch]. ord is the exception and keeps RPC
inside its write transaction, and so does Via today. Projects with several independently committed
consumers give each consumer its own position, stored as height and hash, or as a durable event
identifier that resolves to a block (Clementine).

Fetching headers first and then the block by its checked hash keeps a body from coming from another
branch. LDK's [`BlockSource`][ldk-source] fetches by hash with height only as a hint, and electrs
fetches bodies by the hashes from its header list. Citrea's DA service and Alpen's reader fetch by
height, so this is common practice, not a universal one.

| Source | Observed behavior | Licence | Verdict |
| --- | --- | --- | --- |
| Graph Node `deployment_store.rs` | Effects and head commit together under a lock, forward and backward. | MIT or Apache-2.0 | Adapt to Via's DAL |
| LogPoller `orm.go` | Blocks and logs in one transaction, RPC outside. | MIT | Adapt |
| sBTC `block_observer.rs` | Fetch before the transaction, one transaction per block. | GPL-3.0 | Idea only |
| LDK `BlockSource` | Header first, body by hash. | MIT or Apache-2.0 | Adapt the interface shape behind Via's client |
| ord updater | RPC inside the write transaction. | CC0-1.0 | Reject that part |

### Undo and provenance

Three shapes recur. The first deletes effects by the height or block that produced them: Citrea's
rollback deletes every per-height row in the range, and its fix #2815 made it visit every height
instead of only heights with commitment rows. The second keeps rows and changes their state by block
hash: Bitcoin Core's wallet [marks transactions inactive][core-wallet] on disconnect, and NBXplorer
[keeps transaction identity separate from inclusion][nbx-inclusion] and clears only the inclusion.
The third keeps an undo record for state that was overwritten: Graph Node
[deletes versions created after the kept block and reopens older ones][graph-entities]. All three
need to know which block produced each row.

| Source | Observed behavior | Licence | Verdict |
| --- | --- | --- | --- |
| Graph Node `relational.rs` | Versioned entities, revert reopens prior versions. | MIT or Apache-2.0 | Adapt per table |
| NBXplorer `FullSchema.sql` | Separate transaction, block and inclusion tables. | MIT | Adapt the separation, reject the full trigger schema |
| Bitcoin Core wallet | State change by stored block hash, no delete. | MIT | Adapt for rows other components reference |
| Citrea rollback | Range delete per L1 height, scan position written last. | GPL-3.0-only | Idea only |
| Whole-store restore (ord savepoints) | Restores the oldest retained snapshot. | CC0-1.0 | Reject. It would also roll back sender history |

### Replay: duplicate convergence and orphan undo

Every studied system does both. Re-reading the same block must converge to the same state, and
effects of abandoned blocks are undone separately. ZKsync Era's upgrade processor
[skips an upgrade older than the last seen version and refuses one whose prerequisite is
missing][era-upgrade]. Its priority-operation processor skips serial numbers below the next expected
one and stops on a gap. LogPoller inserts logs idempotently under a conflict key. Clementine raises an
error when the same identity arrives with different data. A message re-mined in a new block keeps
its identity and gets a new inclusion, so identity and inclusion are separate keys.

| Source | Observed behavior | Licence | Verdict |
| --- | --- | --- | --- |
| ZKsync Era `decentralized_upgrades.rs` | Version-keyed upgrade replay rule. | MIT or Apache-2.0 | Adapt to Via's version and `executed` owner. Fixes the #443 shape |
| LogPoller `orm.go` | Idempotent insert by conflict identity. | MIT | Adapt to existing Via keys |
| Clementine kickoff deduplication | Conflicting payload under one identity is an error. | GPL-3.0 | Idea only |
| ZKsync OS `execute_block_in_vm.rs` | Replay compares a stored [expected block output hash][os-output]. It covers the VM output only. | MIT or Apache-2.0 | Adapt only after a complete Via effect definition exists |

### Observe at the tip, act at maturity

Bitcoin layer 2 systems separate what the node has seen from what it may act on irreversibly.
Clementine's syncer stores blocks and events at once ([scan][clementine-scan]) and its consumers act
only on blocks [`finality_depth` deep][clementine-consumer]. Its predicate counts the inclusion block
as confirmation 1 ([`is_block_finalized`][clementine-confirmations]). Alpen gates manifests through
one predicate, [`is_l1_reorg_safe`][alpen-maturity], which returns nothing for a height above the tip.
Babylon waits `k` blocks before an action grants power and acts at once on an unbonding, which only
revokes. LDK matures its on-chain events after `ANTI_REORG_DELAY`. Citrea's DA service is the
counterexample. It reads only up to [`tip - finality_depth + 1`][citrea-height] and treats what it
reads as final, like Via today and ZKsync Era's `eth_watch`.

The action depths that other projects chose:

| Project | Depth | Source | Licence |
| --- | --- | --- | --- |
| Citrea `bitcoin-da` | 6 mainnet, 100 testnet4, 5 signet and regtest | [`network_constants.rs`][citrea-depth] | MIT or Apache-2.0 |
| Rootstock peg-in | 100 mainnet | [`BridgeMainNetConstants.java`][rsk-depth] | LGPL-3.0-or-later |
| Babylon | `k`, reported as 30 on mainnet | Zellic audit remediation, finding 3.8. Not verified from chain parameters | BSL 1.1 |
| Alpen | 6, one configuration value | [`is_l1_reorg_safe`][alpen-maturity] | MIT or Apache-2.0 |
| Clementine | 6 in its testnet4 parameter set | `scripts/docker/configs/testnet4/protocol_paramset.toml` at the pin | GPL-3.0 |
| LDK | `ANTI_REORG_DELAY = 6` | `lightning/src/chain/channelmonitor.rs` at the pin | MIT or Apache-2.0 |

Citrea's testnet4 depth went from 8 to 30 in [commit `cb4a86e8`](https://github.com/chainwayxyz/citrea/commit/cb4a86e8de714fea15698742d77dbafeef82a95a)
on 2024-12-13 ("since we now see reorgs bigger than 8"), and to 100 in
[PR #2276](https://github.com/chainwayxyz/citrea/pull/2276), merged 2025-04-25, which gives no reason
for the number. Its [reorg documentation](https://docs.citrea.xyz/advanced/reorg-handling) attributes the
steps to a 12-block and then a 30-block testnet4 reorg, and its
[bridge documentation](https://docs.citrea.xyz/developer-documentation/system-contracts/bridge) accepts
the longer wait: deposits and withdrawals "appear on Citrea only after the respective number of
confirmations". Its mainnet depth went from 8 to 6 in September 2025.

Test-network values elsewhere are lower and accept the risk: Rootstock requires 10 confirmations for a
testnet peg-in ([help desk](https://helpdesk.rootstock.io/solutions/2waypeg.html)) against 100 on
mainnet, Alpen's documentation sets 6 on its signet, and Botanix's sidecar SDK requires 1 on the
Mutinynet signet. Bitcoin Core contributors describe testnet4's minimum-difficulty rule as the cause:
[BIP 94](https://bitcoin.org/bip/94/) keeps it on purpose although it "has led to block storms", and
the [Optech podcast of 2026-06-16](https://bitcoinops.org/en/podcast/2026/06/16/) reports heights with
"five or even ten different candidate blocks". Proposed fixes (Bitcoin Core PRs #31117, #34420,
#35081) are unmerged, and [BIP 95](https://bips.dev/95/) testnet5, which removes the exception, is a
draft.

Via's own testnet deployment used 0 confirmations from April 2025, then 3 (September 2025), 6
(23 December 2025) and 12 (24 December 2025), each through a deployment configuration change without a
recorded reason. Exchanges and custodians publish
their own deposit confirmation requirements for Bitcoin, and six mainnet confirmations is a long-used
convention. This note did not collect or verify any exchange's published values, so they are context
only. Donor numbers are evidence of practice, not Via defaults.

| Source | Observed behavior | Licence | Verdict |
| --- | --- | --- | --- |
| Citrea `network_constants.rs` and `get_last_finalized_block_header` | Per-network depth table, `tip - depth + 1`. | MIT or Apache-2.0 | Adapt the table shape and unit, not the values |
| Alpen `is_l1_reorg_safe` | One maturity predicate. | MIT or Apache-2.0 | Adapt |
| Clementine maturity-gated consumer | Observe at once, act after depth. | GPL-3.0 | Idea only |
| Babylon grant and revoke rule | Grant waits for depth, revoke acts at once. | BSL 1.1 | Idea only |

### Stop beyond the supported depth

A reorg deeper than what a system can undo stops it with a named action in most projects. Babylon
records the largest reorg and [halts its chain][babylon-halt] when it reaches the confirmation depth.
Clementine's syncer errors with an instruction to increase the finality depth and resync. ord returns
[`Unrecoverable`][ord-bound] and stalls the index. ZKsync Era refuses to revert past the earliest
retained batch and rejects reverting executed batches by default ([`block_reverter`][era-reverter]).
ZKsync OS skips an operator revert whose start block no longer has the expected hash
([`from_block_hash` guard][os-guard]). Alpen is the counterexample. Its `PivotNotFound` is logged and
retried every tick. BDK's emitter, when it finds no agreement point, silently restarts from genesis
([`poll_once`][bdk-poll-once]). The repair-boundary research for
[#429](https://github.com/vianetwork/via-core/issues/429) found that none of these raises a dedicated
alert. They log, fail the process or halt.

| Source | Observed behavior | Licence | Verdict |
| --- | --- | --- | --- |
| ord `Recoverable` and `Unrecoverable` | Typed outcome, index stops. | CC0-1.0 | Copy the outcome names, add an alert |
| ZKsync Era block reverter | Earliest-batch refusal, executed-batch guard. | MIT or Apache-2.0 | Reuse Via's existing descendant |
| ZKsync OS `from_block_hash` | A re-run of a completed revert is a no-op. | MIT or Apache-2.0 | Adapt for operator recovery |
| Babylon `LargestBtcReorg` | Recorded largest reorg drives a halt. | BSL 1.1 | Idea only |
| Alpen `PivotNotFound` loop, BDK genesis reset | Logs forever, or replays from genesis without a signal. | MIT or Apache-2.0 | Reject |

### Other writers and locks

PostgreSQL's Read Committed level gives each statement its own snapshot and does not lock what it
reads ([isolation][pg-isolation], [explicit locking][pg-locks]). A writer that checks "no recovery in
progress" and then commits can therefore commit after recovery has started. Putting the check inside
`BEGIN` does not change that. Graph Node holds a lock across the whole apply. Via already has the
right shape in [`lock_source`][via-lock-source], which takes a shared advisory lock and checks
canonical inclusion, while invalidation takes the exclusive lock. Citrea's transaction monitor copies
its input under a mutex, fetches outside it and installs only if the old tip still matches
([`monitoring.rs`][citrea-monitor]). sBTC's coordinator drops work when the tip changes
([`transaction_coordinator.rs`][sbtc-coordinator]). Those are stale-work checks, not proof that an
unlocked PostgreSQL writer is safe.

Dry-run has a related trap. PostgreSQL does [not roll back sequence allocation][pg-sequence], and Via
uses `BIGSERIAL` keys, so a run that writes and then rolls back is not read-only. Bitcoin Core's
[`VerifyDB`][core-verify] checks blocks through a detached in-memory view, which is the closer model
for a read-only inspection.

| Source | Observed behavior | Licence | Verdict |
| --- | --- | --- | --- |
| Via `lock_source` | Shared and exclusive advisory gate under Read Committed. | MIT or Apache-2.0 | Reuse |
| Graph Node apply | Lock held through commit. Its lock is session-scoped, while Via's is transaction-scoped. | MIT or Apache-2.0 | Adapt the principle, not the lock implementation |
| Citrea monitor, sBTC coordinator | Recheck the tip before installing work. | MIT or Apache-2.0 for Citrea `bitcoin-da`, GPL-3.0 for sBTC | Adapt the Citrea recheck, idea only for sBTC. Neither replaces a lock |
| Bitcoin Core `VerifyDB` | Check through a detached view. | MIT | Adapt for a read-only inspection |

### Library fit for the shared reader

[#450](https://github.com/vianetwork/via-core/issues/450) checked whether Via should depend on BDK or
LDK instead of copying their algorithms. Via builds with rustc 1.82 (`nightly-2024-08-01`), `bitcoin`
0.32.7 and `bitcoincore-rpc` 0.19.0. Throwaway crates that pinned those versions were compiled with
`cargo check` against Via's lockfile.

| Candidate | Finding | Verdict |
| --- | --- | --- |
| LDK `lightning-block-sync` 0.2 and 0.3.0-rc3 | Async and compiles on Via's toolchain. The fork walk lives in the crate-private `ChainNotifier` ([`lib.rs`][ldk-walk-rc3]) and is reachable only through `lightning::chain::Listen`, whose callbacks take `&self`, return nothing and have no transaction or error path. The dependency pulls in the whole `lightning` crate (about 260,000 lines in 0.3.0-rc3) and a second HTTP RPC client. Each minor release breaks the `BlockSource` API. | Reject as a dependency. Copy the walk |
| BDK `bdk_bitcoind_rpc` 0.22.1 | Blocking. Generic over the raw `bitcoincore_rpc::RpcApi` and returns `bitcoincore_rpc::Error`, so it bypasses Via's client, retries and RPC metrics. When it finds no agreement point it restarts from genesis without a signal. The archived `bitcoincore-rpc` remains its dependency, and BDK has announced a later API break. | Reject |
| BDK `bdk_chain` 0.23.4 with `bdk_core` 0.6.4 | Pure, about 8,700 lines, two new crates, compiles on Via's toolchain. It is an in-memory sparse `(height, hash)` chain without parent hashes, needs a genesis anchor, has no depth limit and duplicates the SQL ancestry Via needs. BDK `master` requires Rust 1.85. | Not now. Revisit if the toolchain reaches 1.85 |

The part worth copying is small: LDK's `ChainDifference`, `find_difference_from_header` and
`look_up_previous_header` ([lines 329 to 457 at `v0.3-rc3`][ldk-walk-rc3]), about 30 lines of logic.
Its eight applicable fork-shape tests ([`chain_notifier_tests`][ldk-tests-rc3], about 170 lines) and
its `Blockchain` mock chain ([`test_utils.rs`][ldk-fixture-rc3], about 200 lines) fit Via's tests.
The ninth test covers filtered blocks, which Via does not have. LDK's `LICENSE.md` grants MIT or
Apache-2.0 at the user's option and refers to version control for authorship. LDK's header polling
also checks proof of work and chainwork on every header, and difficulty only on mainnet
([`poll.rs`][ldk-poll-rc3]). Copying the walk does not require copying those checks.

### Tests

Established projects run most reorg cases against an in-memory chain and keep a few cases on a single
regtest node. LDK's test chain supports `fork_at_height` and `disconnect_tip`, and its listener fails
on any unexpected call. Its scenarios cover [shorter, equal-length and longer forks][ldk-tests]. BDK's
[`testenv`][bdk-testenv] invalidates blocks on one regtest node and mines a replacement. electrs has a
[same-height replacement test][electrs-test], ord an [unrecoverable reorg test][ord-test], and Alpen
[named scenarios][alpen-tests] for lagging and diverged sources and a crash between two writes.

Bitcoin Core [PR #32587](https://github.com/bitcoin/bitcoin/pull/32587), merged on 2025-11-26,
replaced `invalidateblock`-based reorg simulations in Core's own tests with real competing forks,
because forced invalidation skips validation paths. Its helper
[`create_empty_fork`][core-fork] builds a valid branch from a chosen ancestor. BDK
[PR #2167](https://github.com/bitcoindevkit/bdk/pull/2167), merged on 2026-04-24, fixed skipped
invalidated heights when an emitter starts above the agreement point, the same failure class as the
batch 265 skip. Its [regression test][bdk-regression] restarts an emitter above the fork point.
rust-bitcoin's `corepc` repository contains a `bitcoind` test crate ([`Cargo.toml`][corepc-manager])
that starts and stops a Core process for tests. Its 0.41.0 manifest declares Rust 1.75.

| Source | Observed behavior | Licence | Verdict |
| --- | --- | --- | --- |
| LDK `test_utils` and fork tests | Mock chain plus strict listener expectations. | MIT or Apache-2.0 | Copy 1:1 the scenarios where unchanged |
| BDK `testenv` and emitter restart test | Regtest reorg helpers, restart after a reorg. | MIT or Apache-2.0 | Adapt the regression test |
| Bitcoin Core `create_empty_fork` | Real competing branch from a chosen ancestor. | MIT | Translate to Rust |
| electrs same-height test, ord unrecoverable test | Single focused cases. | MIT, CC0-1.0 | Copy |
| Alpen reader tests and crash failpoint | Lagging prefix, divergence, pivot not found, crash after a canonical write. | MIT or Apache-2.0 | Copy the scenarios, decide the failpoint mechanism separately |
| `corepc` `bitcoind` crate | Test-only Core process manager. | MIT | Check fit with Via's toolchain before use |

### Decomposed prior art per sub-problem

The first research rounds attributed several recommendations to "own design": Via's withdrawal model
applied to every processor, the verifier set for a proof, the four scan groups and stale-work
fencing, the repair boundary and read-only inspection, and the node adapters.
[#459](https://github.com/vianetwork/via-core/issues/459) split those recommendations into smaller,
domain-neutral sub-problems and searched for established projects that solve each one, including
ledgers, databases, consensus systems and stream processors. Four independent passes read about 40
projects at code level.

The result: every disputed mechanism has established donors, and the useful units are smaller than
a whole reader. No donor decides Via's protocol choices: which event anchors a verifier set, which
processors belong in one scan group, which effects are reversible, or when a payment that may have
been signed can release its hold. Those choices are recorded in the ADRs.

The additional projects were read at these commits. No donor code was built or run, and their tests
were inspected, not executed.

| Project | Language | Revision | Licence |
| --- | --- | --- | --- |
| TigerBeetle | Zig | `tigerbeetle/tigerbeetle@0450c32b2c3c3ab6114f3402cc601341cc8192aa` | Apache-2.0 |
| Formance ledger v2 | Go | `formancehq/ledger@3ab99b3ad28ffcc6034d94b4e3d831b823cce7a5` (branch `main_v2`) | MIT. The v3 rewrite on the default branch has no licence file and is an idea only |
| Apache Fineract | Java | `apache/fineract@71c39730e9178301b7ab949ba711238ce81fc66d` | Apache-2.0 |
| etcd | Go | `etcd-io/etcd@bf9cadf044f0531036a61c9b6dbeba8aef2a3b30` | Apache-2.0 |
| etcd Raft | Go | `etcd-io/raft@1931ff5beacc1e2539003c0c70f096285f04c425` | Apache-2.0 |
| CometBFT | Go | `cometbft/cometbft@99dfb19d9fb406389269b079d34aba555ee4f3d5` | Apache-2.0 |
| Lighthouse | Rust | `sigp/lighthouse@e423a66763bb1bd780492d635123f208d80c3538` | Apache-2.0 |
| Apache ZooKeeper | Java | `apache/zookeeper@777b3723f56456524a052b79f73649e99da1f1aa` | Apache-2.0 |
| Apache Kafka | Java and Scala | `apache/kafka@a3444d8b32beee97bd0afaa267a9c4180815de60`, and `65566690ca1ddb584388c1026e2ff6673e9c6f19` for follower truncation | Apache-2.0 |
| PostgreSQL | C | `postgres/postgres@a39e53a700c5b728f2f7dab964e33b157979fb3a`, and `856a6594d86e4a7c0e9490253912af3f6159802f` for the recovery state | PostgreSQL |
| Apache Flink | Java | `apache/flink@14fa27183139cb9ec0a5f2b34f5d72f7477115c1` | Apache-2.0 |
| Temporal | Go | `temporalio/temporal@5baee6e5efb99a1dc4fa5816aa396696ff7f1746` | MIT |
| Kubernetes apiserver | Go | `kubernetes/apiserver@9c4ec15b8038e9f47e8caf219903649dc67c992a` | Apache-2.0 |
| Debezium | Java | `debezium/debezium@e895a2cd6d08e587bcdc8cd8ba033ef6358cbf44`, and `215ffa1ad3f13a484b11c8deb9600a05d9ef08cf` for the position check | Apache-2.0 |
| Marten | C# | `JasperFx/marten@7045d09bb0afef1b4f34c4124e3c0852e8e7e03d` | MIT |
| Terraform | Go | `hashicorp/terraform@7a5717566e57a9d8705bed20fe199a0785ba0416` | BSL 1.1, idea only |

"Translate" in the table below means a 1:1 translation of a small unit into Rust. The other verdicts
keep the meanings defined at the start of this section.

| Sub-problem | Donor and source | Verdict | Accepted in |
| --- | --- | --- | --- |
| Keep evidence, change accepted membership, reverse instead of delete | NBXplorer [inclusion table][nbx-inclusion]; TigerBeetle [pending resolution][dp-tb-pending]; Formance [conditional revert][dp-formance-revert]; Fineract [linked reversal][dp-fineract-reversal] | Adapt. Translate Formance's `UPDATE … WHERE reverted_at IS NULL RETURNING` | [ADR 0013](../adr/0013-keep-evidence-membership-and-derived-state-apart.md) |
| Replay the same identity, reject a changed payload | TigerBeetle [`create_transfer_exists`][dp-tb-exists] and [tests][dp-tb-tests]; etcd [revision compare][dp-etcd-txn] | Translate TigerBeetle's field-wise comparator. Adapt etcd | ADR 0013 |
| Keep a hold separate from settlement | TigerBeetle [pending, post and void][dp-tb-pending]; Fineract [hold and release][dp-fineract-hold] | Adapt to Via's existing holds | ADR 0013 |
| Stable order inside one batch, reject conflicting updates | etcd [revision and sub-revision][dp-etcd-revision] and [duplicate-key check][dp-etcd-dupkey]; Raft [pending configuration][dp-raft-conf]; Kubernetes [resource version][dp-k8s-cas] | Translate etcd's flat duplicate-key check, with the role as the key. Adapt the rest | ADR 0013 |
| Membership at a historical position for one referenced object | CometBFT [`LoadValidators(height)`][dp-comet-load] and [single-set verify][dp-comet-verify]; Lighthouse [target committee][dp-lighthouse-target]; ZooKeeper [per-proposal quorum][dp-zookeeper-quorum] | Adapt CometBFT's lookup and tally contract. Lighthouse and ZooKeeper are ideas only. The Via anchor stays policy | [ADR 0015](../adr/0015-verifier-set-at-proof-inclusion-decides-finalization.md) |
| Effects and consumer position in one commit | PostgreSQL [replication origin][dp-pg-origin]; Kafka Connect [offsets in the transaction][dp-kafka-eos]; Marten [progress compare-and-swap][dp-marten-cas] | Adapt, with the position as a compare-and-swap on `(height, hash)` | [ADR 0010](../adr/0010-commit-each-scan-group-with-its-position.md) |
| Durable multi-step completion | Kafka [transaction coordinator][dp-kafka-coordinator]; Flink [checkpoint completion][dp-flink-checkpoint]; PostgreSQL [`pg_control` state][dp-pg-control] | Adapt to the existing `via_l1_reorg` barrier | ADR 0010 |
| Reject late work from an obsolete execution | Temporal [attempt and version check][dp-temporal-token]; Kafka [epoch fencing][dp-kafka-fence]; etcd [revision compare][dp-etcd-txn] | Adapt. Capture the never-reused row `id` | ADR 0010 |
| Inspect without writes, apply only if unchanged | Kubernetes [server dry run][dp-k8s-dryrun]; etcd [compare][dp-etcd-txn]; PostgreSQL [`READ ONLY` refuses `nextval`][dp-pg-readonly]; Terraform [saved-plan check][dp-terraform-plan] | Adapt. Terraform is an idea only, because of BSL 1.1 | [ADR 0014](../adr/0014-repair-only-reversible-provable-steps-and-inspect-read-only.md) |
| Prove the common ancestor before undo | LDK [walk][ldk-walk-rc3]; PostgreSQL [`pg_rewind` ancestor][dp-pg-rewind]; Raft [committed-entry guard][dp-raft-committed] | Copy the LDK loop. Adapt the rest | [ADR 0011](../adr/0011-shared-reader-in-via-reorg-with-copied-ldk-walk.md), ADR 0014 |
| Refuse rather than guess when history is missing | PostgreSQL [`pg_rewind`][dp-pg-rewind]; Debezium [position check][dp-debezium-position]; Kafka [follower truncation][dp-kafka-truncate] | Adapt. Reject Kafka's fallback to the high watermark | ADR 0014 |
| Compensate rather than erase | Fineract [linked reversal][dp-fineract-reversal]; TigerBeetle [correcting transfers][dp-tb-correct] | Adapt | ADR 0014 |
| Separate source, transition and delivery | Kafka Connect [sink task][dp-kafka-sink]; Debezium [source coordinator][dp-debezium-coordinator]; LDK [walk][ldk-walk-rc3] | Adapt. Copy the LDK walk | ADR 0011 |

Only four units are taken as code: the LDK walk, copied in Rust, and TigerBeetle's comparator,
Formance's conditional revoke and etcd's duplicate-key check, translated from Zig and Go. The passes
disagreed on Marten's progress update, which one pass rated a 1:1 translation and another an
adaptation. ADR 0010 records it as adapted. Whole implementations are rejected: a new financial
ledger, Raft consensus, a change-data-capture broker, Flink checkpointing, a physical `pg_rewind` of
Via's store, and LDK's listener interface. Key-only idempotency, Marten's unchecked rewind and Kafka's
high-watermark fallback are rejected as patterns.

The passes also confirmed two facts in Via code at `eb50293e7`, unchanged since `6bcdc07a6`. The
main-node reverter skips its executed-batch guard whenever a reorg row exists
([`via_block_reverter`][dp-via-reverter-guard]), and deletes snapshot files after its SQL commit, so
a crash between the two is not resumed ([`via_block_reverter`][dp-via-reverter-files]). The zk
verdict update keys on the natural key `(l1_batch_number, proof_reveal_tx_id)`
([`verify_votable_transaction`][dp-via-verdict]) rather than on the row's identity.

## Limits

The depth figures come from two public nodes and are upper bounds per event. The signet misfire
branches come from the operator's forum thread, not from a node snapshot. Babylon's mainnet `k` of
30 and Citrea's depth history come from an audit report and project documentation, not from code or
chain parameters. Clementine's mainnet depth is set by environment and was not verified. No test in a
comparison project was run. The dependency probes compiled crates but ran no code. The PostgreSQL
concurrency findings follow the documented isolation rules and a reasoned schedule, not a
reproduction. The statements about Via's tables, transactions and failure modes were read from code
at `6bcdc07a6` and, except for the batch 265 skip, are not reproduced. The origin of the testnet value
12 comes from a deployment configuration history that is not public. The exchange context is a
convention, not a verified survey.

## Open questions

The open decisions are tracked on [#426](https://github.com/vianetwork/via-core/issues/426) and
explained in the [design record](../design/bitcoin-reader-redesign/README.md). The factual questions
that this research could not settle are these:

- Which old wallet, deposit, main-node vote and upgrade rows can be tied to the block that produced
  them during migration. Retained data does not prove it for most of them.
- Whether any `UpdateBridge` message has been included on testnet4, which decides whether past votes
  crossed a verifier-set rotation. This needs a chain census.
- Whether any production path has produced votes for two proofs of one batch on the main node
  ([#454](https://github.com/vianetwork/via-core/issues/454)).
- What the CI runner provides for a pinned Bitcoin Core binary and a disposable database, and whether
  the `bitcoind` test crate builds with Via's toolchain
  ([#445](https://github.com/vianetwork/via-core/issues/445)).
- Via can measure reorgs itself by storing its own node's `getchaintips` output and logging every
  rollback with old tip, new tip and depth.

[core-header]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/primitives/block.h#L27-L42
[core-work-order]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/node/blockstorage.cpp#L174-L192
[core-most-work]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/validation.cpp#L3122-L3135
[core-getblockhash]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/rpc/blockchain.cpp#L608-L636
[core-getblockheader]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/rpc/blockchain.cpp#L638-L706
[core-membership]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/rpc/blockchain.cpp#L155-L203
[core-getchaintips]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/rpc/blockchain.cpp#L1612-L1705
[core-getbestblockhash]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/rpc/blockchain.cpp#L308-L330
[core-zmq]: https://github.com/bitcoin/bitcoin/blob/cce32b16874e85e8a00cfa1a2190f0b6f4dcaf66/src/zmq/zmqnotificationinterface.cpp#L151-L158
[core-min-difficulty]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/pow.cpp#L14-L36
[core-testnet4-params]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/kernel/chainparams.cpp#L350-L370
[core-index-startup]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/index/base.cpp#L105-L148
[core-index-sync]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/index/base.cpp#L208-L341
[core-callbacks]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/validationinterface.h#L35-L48
[core-wallet]: https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/wallet/wallet.cpp#L1570-L1615
[core-verify]: https://github.com/bitcoin/bitcoin/blob/cce32b16874e85e8a00cfa1a2190f0b6f4dcaf66/src/validation.cpp#L4627-L4760
[core-fork]: https://github.com/bitcoin/bitcoin/blob/79d6e874e1dad6822b5d7e6993aa205d21ed2cbf/test/functional/test_framework/blocktools.py#L119-L137
[bip94-rule]: https://github.com/bitcoin/bips/blob/8c4d9e258b8cec913ffe4bddeb1d96e9f0c01ae5/bip-0094.mediawiki#L46
[bip94-rationale]: https://github.com/bitcoin/bips/blob/8c4d9e258b8cec913ffe4bddeb1d96e9f0c01ae5/bip-0094.mediawiki#L95
[bip95]: https://github.com/bitcoin/bips/blob/c134c7db552d26801ba2ef86e00c50febe843718/bip-0095.md
[bdk-emitter]: https://github.com/bitcoindevkit/bdk/blob/d02f8909171af695fcb078c6fca082833f835b86/crates/bitcoind_rpc/src/lib.rs#L362-L469
[bdk-poll-once]: https://github.com/bitcoindevkit/bdk/blob/5dc6e959379982a1661e8f57c3ee7af334627ca5/crates/bitcoind_rpc/src/lib.rs#L277-L329
[bdk-testenv]: https://github.com/bitcoindevkit/bdk/blob/d02f8909171af695fcb078c6fca082833f835b86/crates/testenv/src/lib.rs#L379-L428
[bdk-regression]: https://github.com/bitcoindevkit/bdk/blob/d02f8909171af695fcb078c6fca082833f835b86/crates/bitcoind_rpc/tests/test_emitter.rs#L671-L709
[ldk-source]: https://github.com/lightningdevkit/rust-lightning/blob/0a2b003e7e1602df8933b15b7563ba8b6e198391/lightning-block-sync/src/lib.rs#L62-L89
[ldk-poll]: https://github.com/lightningdevkit/rust-lightning/blob/0a2b003e7e1602df8933b15b7563ba8b6e198391/lightning-block-sync/src/lib.rs#L270-L290
[ldk-walk]: https://github.com/lightningdevkit/rust-lightning/blob/0a2b003e7e1602df8933b15b7563ba8b6e198391/lightning-block-sync/src/lib.rs#L417-L489
[ldk-check]: https://github.com/lightningdevkit/rust-lightning/blob/0a2b003e7e1602df8933b15b7563ba8b6e198391/lightning-block-sync/src/poll.rs#L127-L161
[ldk-startup]: https://github.com/lightningdevkit/rust-lightning/blob/0a2b003e7e1602df8933b15b7563ba8b6e198391/lightning-block-sync/src/init.rs#L142-L225
[ldk-tests]: https://github.com/lightningdevkit/rust-lightning/blob/0a2b003e7e1602df8933b15b7563ba8b6e198391/lightning-block-sync/src/lib.rs#L646-L724
[ldk-walk-rc3]: https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/lib.rs#L329-L457
[ldk-tests-rc3]: https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/lib.rs#L620-L790
[ldk-fixture-rc3]: https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/test_utils.rs#L22-L218
[ldk-poll-rc3]: https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/poll.rs#L105-L136
[corepc-manager]: https://github.com/rust-bitcoin/corepc/blob/8d82c88aebf8268bfc9732aaa15e64a5bba1f9a0/bitcoind/Cargo.toml#L1-L53
[electrs-fetch]: https://github.com/romanz/electrs/blob/35216c6d30148be8e6763d913d437330f431fc03/src/index.rs#L169-L258
[electrs-test]: https://github.com/romanz/electrs/blob/35216c6d30148be8e6763d913d437330f431fc03/src/chain.rs#L243-L259
[esplora-shrink]: https://github.com/Blockstream/electrs/blob/0963ef518a69f884496e1caf08315df874c9bf13/src/util/block.rs#L136-L185
[bindex-shrink]: https://github.com/romanz/bindex-rs/blob/ea8854a0c9b8535179ff92179bf4d3e38bfef82d/bindex-lib/src/chain.rs#L208-L240
[fulcrum-shrink]: https://github.com/cculianu/Fulcrum/blob/02e22da92355893d8696305f0741acb7a8bc61f4/src/Controller.cpp#L1151-L1187
[ord-bound]: https://github.com/ordinals/ord/blob/df660674bf78d95db866376aec377def38df3a88/src/index/reorg.rs#L25-L48
[ord-test]: https://github.com/ordinals/ord/blob/fd152bf94d814c7b158853438bc6a1c6a9d37096/src/subcommand/server.rs#L4489-L4512
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
[alpen-tests]: https://github.com/alpenlabs/alpen/blob/882107abd8ec691f870dced0a680267124dd8294/crates/btcio/src/reader/query.rs#L1062-L1614
[babylon-halt]: https://github.com/babylonlabs-io/babylon/blob/0de36afadb84fec624c80b07d743a0d9f5341722/x/btcstaking/keeper/btc_reorg.go#L10-L30
[rsk-depth]: https://github.com/rsksmart/rskj/blob/ecef55ddf2b79fe5cd84c824118117dd825744b2/rskj-core/src/main/java/co/rsk/peg/constants/BridgeMainNetConstants.java#L22-L23
[sbtc-fetch]: https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/src/block_observer.rs#L417-L458
[sbtc-coordinator]: https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/src/transaction_coordinator.rs#L326-L367
[reth-notifications]: https://github.com/paradigmxyz/reth/blob/f88813adb966324ecfc530a2f78df38ed0df5750/crates/exex/types/src/notification.rs#L10-L61
[era-upgrade]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_watch/src/event_processors/decentralized_upgrades.rs#L94-L113
[era-reverter]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/block_reverter/src/lib.rs#L119-L180
[os-guard]: https://github.com/matter-labs/zksync-os-server/blob/e29a2c0a584b7f3f985e9a414bfbe179456c093d/node/bin/src/lib.rs#L1016-L1051
[os-output]: https://github.com/matter-labs/zksync-os-server/blob/e29a2c0a584b7f3f985e9a414bfbe179456c093d/lib/sequencer/src/execution/execute_block_in_vm.rs#L455-L468
[op-frontier]: https://github.com/ethereum-optimism/optimism/blob/1f94d97c7dd870bbacc24b5f962959529506225d/op-node/rollup/derive/l1_traversal.go#L60-L95
[graph-apply]: https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs#L1151-L1214
[graph-undo]: https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/deployment_store.rs#L1330-L1368
[graph-entities]: https://github.com/graphprotocol/graph-node/blob/6838f4e3cf3607c5bc32ef705dd05aec1eb057ab/store/postgres/src/relational.rs#L1076-L1119
[chainlink-detect]: https://github.com/smartcontractkit/chainlink-evm/blob/ea52d84d2f7aecf8b3dfb9cc88db1ab6e6cd6ba2/pkg/logpoller/log_poller.go#L1052-L1083
[chainlink-shrink]: https://github.com/smartcontractkit/chainlink-evm/blob/ea52d84d2f7aecf8b3dfb9cc88db1ab6e6cd6ba2/pkg/logpoller/log_poller.go#L1204-L1238
[chainlink-insert]: https://github.com/smartcontractkit/chainlink-evm/blob/ea52d84d2f7aecf8b3dfb9cc88db1ab6e6cd6ba2/pkg/logpoller/orm.go#L583-L619
[pg-isolation]: https://www.postgresql.org/docs/18/transaction-iso.html
[pg-locks]: https://www.postgresql.org/docs/18/explicit-locking.html
[pg-sequence]: https://www.postgresql.org/docs/18/functions-sequence.html
[via-reorg]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/lib/via_reorg/src/lib.rs#L1-L67
[via-window]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/lib/config/src/configs/via_reorg_detector.rs#L17-L26
[via-fetch-window]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/lib/via_btc_client/src/client/mod.rs#L44-L68
[via-sender-check]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/lib/via_btc_client/src/client/mod.rs#L124-L132
[via-detect]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/node/via_main_node_reorg_detector/src/lib.rs#L169-L241
[via-verifier-scan]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/node/via_btc_watch/src/lib.rs#L180-L257
[via-main-scan]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/node/via_btc_watch/src/lib.rs#L101-L140
[via-process-block]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/lib/via_btc_client/src/indexer/mod.rs#L121-L127
[via-base-confirmations]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/etc/env/base/via_btc_watch.toml#L5
[via-sender-confirmations]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/etc/env/base/via_btc_sender.toml#L13
[via-withdrawal-guard]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/lib/verifier_dal/src/withdrawals_dal.rs#L545-L548
[via-upgrade-skip]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/node/via_btc_watch/src/message_processors/governance_upgrade.rs#L66
[via-zk-final]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/node/via_zk_verifier/src/lib.rs#L267-L277
[via-restart-indexing]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/node/via_node_storage_init/src/indexer.rs#L43-L54
[via-restart-indexing-verifier]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/node/via_verifier_storage_init/src/indexer.rs#L57-L65
[via-lock-source]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/lib/verifier_dal/src/via_votes_dal.rs#L20-L59
[dp-tb-exists]: https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig#L3971-L4033
[dp-tb-pending]: https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig#L4036-L4127
[dp-tb-tests]: https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine_tests.zig#L1768-L1812
[dp-tb-correct]: https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/docs/coding/recipes/correcting-transfers.md#L1-L34
[dp-formance-revert]: https://github.com/formancehq/ledger/blob/3ab99b3ad28ffcc6034d94b4e3d831b823cce7a5/internal/storage/ledger/transactions.go#L154-L213
[dp-fineract-reversal]: https://github.com/apache/fineract/blob/71c39730e9178301b7ab949ba711238ce81fc66d/fineract-provider/src/main/java/org/apache/fineract/accounting/journalentry/service/JournalEntryWritePlatformServiceImpl.java#L318-L408
[dp-fineract-hold]: https://github.com/apache/fineract/blob/71c39730e9178301b7ab949ba711238ce81fc66d/fineract-provider/src/main/java/org/apache/fineract/portfolio/savings/service/SavingsAccountWritePlatformServiceJpaRepositoryImpl.java#L1878-L1982
[dp-etcd-txn]: https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/server/etcdserver/txn/txn.go#L32-L101
[dp-etcd-revision]: https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/server/storage/mvcc/kvstore_txn.go#L174-L245
[dp-etcd-dupkey]: https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/server/etcdserver/api/v3rpc/key.go#L236-L314
[dp-raft-conf]: https://github.com/etcd-io/raft/blob/1931ff5beacc1e2539003c0c70f096285f04c425/raft.go#L1310-L1345
[dp-raft-committed]: https://github.com/etcd-io/raft/blob/1931ff5beacc1e2539003c0c70f096285f04c425/log.go#L107-L167
[dp-k8s-cas]: https://github.com/kubernetes/apiserver/blob/9c4ec15b8038e9f47e8caf219903649dc67c992a/pkg/registry/generic/registry/store.go#L873-L896
[dp-k8s-dryrun]: https://github.com/kubernetes/apiserver/blob/9c4ec15b8038e9f47e8caf219903649dc67c992a/pkg/registry/generic/registry/dryrun.go#L30-L107
[dp-comet-load]: https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/state/store.go#L548-L615
[dp-comet-verify]: https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/types/validation.go#L65-L138
[dp-lighthouse-target]: https://github.com/sigp/lighthouse/blob/e423a66763bb1bd780492d635123f208d80c3538/beacon_node/beacon_chain/src/attestation_verification.rs#L1580-L1622
[dp-zookeeper-quorum]: https://github.com/apache/zookeeper/blob/777b3723f56456524a052b79f73649e99da1f1aa/zookeeper-server/src/main/java/org/apache/zookeeper/server/quorum/Leader.java#L1316-L1325
[dp-pg-origin]: https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/backend/replication/logical/worker.c#L2543-L2582
[dp-pg-control]: https://github.com/postgres/postgres/blob/856a6594d86e4a7c0e9490253912af3f6159802f/src/include/catalog/pg_control.h#L97-L180
[dp-pg-readonly]: https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/backend/commands/sequence.c#L649-L660
[dp-pg-rewind]: https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/bin/pg_rewind/pg_rewind.c#L1052-L1085
[dp-kafka-eos]: https://github.com/apache/kafka/blob/a3444d8b32beee97bd0afaa267a9c4180815de60/connect/runtime/src/main/java/org/apache/kafka/connect/runtime/ExactlyOnceWorkerSourceTask.java#L266-L341
[dp-kafka-coordinator]: https://github.com/apache/kafka/blob/a3444d8b32beee97bd0afaa267a9c4180815de60/core/src/main/scala/kafka/coordinator/transaction/TransactionStateManager.scala#L550-L604
[dp-kafka-fence]: https://github.com/apache/kafka/blob/a3444d8b32beee97bd0afaa267a9c4180815de60/core/src/main/scala/kafka/coordinator/transaction/TransactionCoordinator.scala#L625-L708
[dp-kafka-sink]: https://github.com/apache/kafka/blob/a3444d8b32beee97bd0afaa267a9c4180815de60/connect/runtime/src/main/java/org/apache/kafka/connect/runtime/WorkerSinkTask.java#L624-L661
[dp-kafka-truncate]: https://github.com/apache/kafka/blob/65566690ca1ddb584388c1026e2ff6673e9c6f19/core/src/main/scala/kafka/server/AbstractFetcherThread.scala#L489-L541
[dp-flink-checkpoint]: https://github.com/apache/flink/blob/14fa27183139cb9ec0a5f2b34f5d72f7477115c1/flink-runtime/src/main/java/org/apache/flink/runtime/checkpoint/CheckpointCoordinator.java#L1363-L1441
[dp-temporal-token]: https://github.com/temporalio/temporal/blob/5baee6e5efb99a1dc4fa5816aa396696ff7f1746/service/history/api/activity_util.go#L58-L79
[dp-debezium-coordinator]: https://github.com/debezium/debezium/blob/e895a2cd6d08e587bcdc8cd8ba033ef6358cbf44/debezium-connector-common/src/main/java/io/debezium/pipeline/ChangeEventSourceCoordinator.java#L124-L162
[dp-debezium-position]: https://github.com/debezium/debezium/blob/215ffa1ad3f13a484b11c8deb9600a05d9ef08cf/debezium-connector-common/src/main/java/io/debezium/connector/common/BaseSourceTask.java#L146-L167
[dp-marten-cas]: https://github.com/JasperFx/marten/blob/7045d09bb0afef1b4f34c4124e3c0852e8e7e03d/src/Marten/Events/Daemon/Progress/UpdateProjectionProgress.cs#L43-L91
[dp-terraform-plan]: https://github.com/hashicorp/terraform/blob/7a5717566e57a9d8705bed20fe199a0785ba0416/internal/backend/local/backend_local.go#L282-L345
[dp-via-reverter-guard]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/node/via_block_reverter/src/lib.rs#L106-L149
[dp-via-reverter-files]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/node/via_block_reverter/src/lib.rs#L152-L175
[dp-via-verdict]: https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/via_verifier/lib/verifier_dal/src/via_votes_dal.rs#L417-L441
