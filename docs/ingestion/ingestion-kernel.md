# Ingestion Kernel: pruned Bitcoin node support and de-triplication of L1 ingestion

- Status: IMPLEMENTED through the database adapter on `spike/kernel-base`: typed contract (wire format 3), positioned parser, production engine, generic Postgres adapter, and 22 registered conformance fixtures plus the adapter-local concurrent-genesis test passing against real Postgres for all three roles. Sections below marked as proposals were since ratified or revised; each carries its current status.
- Date: 2026-07-10
- Authors: Romano, with analysis by Claude (Fable 5); consulted independently: Codex (gpt-5.6-sol), GPT (omp), GLM
- Evidence: verbatim consultant reports, archived in the untracked local folder `docs/_local/ingestion-research/` (cited below as `research/<file>`; ask the maintainers for a copy); all file and line citations in section 3 re-verified against main HEAD `27df0dd7caff` on 2026-07-10 (see Appendix)

## 1. Why this document exists

Two problems arrived together and turned out to share one root cause.

**Problem 1: full-node cost.** Running Via requires archival Bitcoin nodes with `txindex=1`. On mainnet that is 700+ GB and growing, doubled for redundancy. Testnet4 hides the problem because it is young and small. The question was: can via-core run against a pruned Bitcoin node, and if not, what would make it possible?

**Problem 2: ingestion triplication.** PR 379 (`feat(via-btc): support pruned Bitcoin nodes`, +2713/-877, 55 files) attempted problem 1 and, in doing so, exposed problem 2. Roughly 58% of its lines are the same change repeated across three sibling trees (core watcher, verifier watcher, standalone indexer). The three DALs it adds are 94-98% identical; its three migrations are byte-identical. This is not a criticism of the PR author alone: the codebase's shape forces any ingestion change to be made three times, and the copies drift. The drift is not cosmetic. The core reverter is missing locator cleanup that the sibling trees have, which is a real state-consistency bug produced directly by the triplication.

The conclusion of a four-model investigation (Claude, Codex, GPT, GLM, reports archived in the local research folder) is that both problems are solved by the same structural move: extract deterministic Bitcoin block interpretation into one shared unit (the "ingestion kernel") whose durable output makes historical Bitcoin data locally recoverable, so the Bitcoin node no longer needs to serve as an archive.

## 2. Established facts about pruned Bitcoin nodes

These were verified against Bitcoin Core source (30.x, 31.x, master) during the investigation. Citations with file and line references are in `research/glm-pruned-ibd-tailing.md` and `research/codex-pruned-review.md`.

1. `-prune=N` (N is a MiB target, not a block count) is incompatible with `-txindex=1`. Bitcoin Core refuses to start with both (`init.cpp`). Master additionally gates `-txospenderindex`. `blockfilterindex` is not gated.
2. Without txindex, `getrawtransaction(txid)` fails for anything not in the mempool. The two-argument form `getrawtransaction(txid, blockhash)` works only while the block body is still on disk. After pruning deletes the block, the call fails even with a correct block hash. A txid-to-blockhash locator is therefore provenance, not storage.
3. `scantxoutset` works on pruned nodes but scans the full chainstate, is single-flight, and blocks. In via-core it is regtest-only today.
4. `getblockfrompeer` is not a bulk backfill mechanism. Peers serving historical blocks disconnect requesters asking for blocks older than about 7 days of work (`HISTORICAL_BLOCK_AGE`), and transfer is capped at 16 in-flight blocks per peer. Fetched blocks persist only until the next prune.
5. Prune retention during deep IBD collapses toward `MIN_BLOCKS_TO_KEEP` (288 blocks) regardless of the configured prune target. There is no external prune-lock. "Tail the IBD and index before the prune catches up" is not a safe replay strategy.
6. Restart semantics are asymmetric: full to pruned just deletes old blocks (no redownload); pruned back to full requires a complete redownload.

Consequence: a pruned-compatible via-core must never issue a txid-only historical RPC on its production path. Everything it will ever need to re-read must be persisted in its own database at observation time, and the Bitcoin node becomes a source of new blocks only.

## 3. Established facts about via-core's current ingestion

All line numbers below verified on main HEAD `27df0dd7caff` (2026-07-10, evidence pack).

1. Hard txindex dependencies: `get_raw_transaction(txid, None)` at `core/lib/via_btc_client/src/client/rpc_client.rs:148` and `get_raw_transaction_info(txid, None)` at `:199`; prev-tx validation fetches in `core/lib/via_btc_client/src/indexer/mod.rs` at `:375`, `:386`, `:398`, `:416`; bootstrap transaction fetched by bare txid at `core/lib/via_btc_client/src/bootstrap/mod.rs:34`; `fetch_utxos` at `core/lib/via_btc_client/src/client/mod.rs:77`; confirmation check via bare-txid raw-info lookup at `:99-101`. Scan cursor persisted at `core/node/via_btc_watch/src/lib.rs:208`. `scantxoutset` (regtest-only path) at `rpc_client.rs:129`.
2. Fail-closed hazard: validation lookups wrapped in `.unwrap_or(false)` at `core/lib/via_btc_client/src/indexer/mod.rs:328` (bridge withdrawal) and `:345` (governance) convert an unavailable lookup (RPC error, missing data) into "invalid message" while the scan cursor advances. On a pruned node this becomes silent, permanent data loss. PR 379's `_with_locator_store` variants preserve this pattern.
3. Triplication: three sibling trees (`core/node/via_btc_watch`, 1251 Rust LOC; `via_verifier/node/via_btc_watch`, 1949 LOC; `via_indexer/node/indexer`, 704 LOC) each own a watcher loop, message processors, DAL, reverter, and reorg handling. Reproducible overlap metric (distinct common lines / combined lines, maximum 0.5 for identical files): core to verifier 0.27, core to indexer 0.16, verifier to indexer 0.17. Six migration file sets are byte-identical across DB families on main today (for example `via_wallet_migration` up and down, identical in all three). On PR 379's branch, its three added locator DALs were 94-98% identical and its three migrations byte-identical. Every protocol-semantics change is made three times and reviewed three times; omissions (the reverter hole) reach production. Reorg-detector writes to `via_l1_blocks` at `core/node/via_main_node_reorg_detector/src/lib.rs:110`, `:142`, `:178`; deletion at `:266`.
4. Wallet-update semantics are underspecified. All three watchers parse a block range, apply wallet updates, then reprocess some or all of the range, with core/verifier and standalone doing it differently. When a rotated wallet becomes effective (same block, containing block, next block) is a protocol question the current code answers by accident.
5. Message ordering does not follow Bitcoin transaction order. `FullInscriptionMessage::sort_messages` sorts by enum declaration order (`core/lib/via_btc_client/src/types.rs:283-306`), so a wallet update can retroactively affect transactions that precede it in the same block once the range is reparsed.
6. Transaction identity is conflated. The parser keys messages by `compute_ntxid()` — the normalized txid, which strips signature data — at `core/lib/via_btc_client/src/indexer/parser.rs:765`, `:839` and six other sites, while storage and dedup logic use the canonical txid. Neither commits to witness data (wtxid), where Via inscriptions actually live. Three distinct identities (txid, wtxid, ntxid) are used interchangeably.
7. Deposit parsing is ambiguous for multi-output transactions: the parser selects the first output paying the bridge script (`parser.rs:128`) while amount validation sums all bridge outputs (`indexer/mod.rs:356-363`). A transaction with two bridge outputs passes or fails depending on which accident dominates.
8. `block_confirmations` defaults to 0 (`core/lib/config/src/configs/via_btc_watch.rs:37`): the watchers effectively process at the tip by default.

## 4. Verdict on PR 379

Reviewed independently by Claude and Codex; both landed on request-changes. Full reviews: `research/codex-pr379-review.md`.

Good and worth keeping regardless of anything else:

| PR 379 piece | Disposition |
|---|---|
| `BitcoinUtxo` carrying the `TxOut` from `listunspent`/`scantxoutset` (removes per-UTXO historical fetches) | Port as an independent fix with tests |
| Explicit block-scoped raw transaction RPC | Port as a distinct method; a retained-window tool, never a historical store |
| Active-chain confirmation check | Port into the canonical inclusion/reorg design |
| Chain-info RPC model | Nothing to port: `get_blockchain_info` already exists on main (`traits.rs:85`); PR 379 only added a mock. The missing piece is the runtime prune/IBD guard caller — seam A work, not salvage |
| Mock/unit fixtures | Reuse selectively |

Fatal to the architecture and not worth iterating:

- Locator-only storage (txid to blockhash) as the historical mechanism: fails fact 2 above the moment the block prunes.
- Automatic all-chain locator backfill starting from block 1: chain-scale table growth with no recoverability gain.
- Bootstrap locator config instead of a verifiable artifact.
- The surviving `.unwrap_or(false)` paths.
- No runtime prune/IBD guard despite adding the RPC model for one.
- The whole change triplicated across the sibling trees, including drift (the reverter cleanup hole).

## 5. Decision (proposed): the ingestion kernel

One shared deterministic unit interprets Bitcoin blocks; three thin per-schema adapters commit the result. The shared artifact is code and a typed contract, not a shared database: core, verifier, and standalone each keep their own Bitcoin node and their own database, preserving verifier independence.

The kernel's responsibility is deliberately narrow:

```text
validated block envelope + explicit protocol context + resolved dependencies
    -> deterministic, versioned BlockPlan
```

RPC, retries, SQL, polling, wallet import, metrics, and process supervision all stay outside, at explicit seams. Implemented shape: the contract lives in `core/lib/via_btc_ingestion`, the engine (`ViaProtocolEngine`) and a positioned panic-free parser surface live in `core/lib/via_btc_client`, and one role-configured Postgres adapter lives in `core/lib/via_ingestion_adapter`. The legacy `BitcoinInscriptionIndexer` path is untouched and keeps running until shadow comparison passes.

### 5.1 Seams

- **A. Bitcoin source shell.** Owns RPC, retry, IBD/prune state, confirmed-height selection. Emits a `BitcoinBlockEnvelope` (network, height, hash, prev_hash, time, transactions with raw bytes). Enforces the runtime guard: not in IBD, and every next required full block at or above `pruneheight`.
- **B. Protocol engine (the kernel proper).** Two deterministic stages: `inspect(envelope, protocol_context) -> draft + required_dependency_keys`, then iterative finalization: `finalize(draft, resolved_dependencies)` returns a complete `BlockPlan`, a typed failure, or `NeedDependencies` with newly discovered keys (an attestation names a proof transaction whose content names the batch transaction, so closure can take two rounds). Dependencies are resolved from the current block or the local observation store, never from a txid-only historical RPC. This is enforced by construction, not convention: the kernel's dependencies expose only local reads (raw variants, inclusions, tracked outputs) — no type carrying `get_transaction` or any RPC capability is accepted. Zero historical fallback becomes inexpressible rather than merely tested; the RPC-counting spy in the fixture suite is a backstop.
- **C. Versioned BlockPlan.** Block anchor; immutable raw relevant transactions; inclusions; tracked output creations/spends; ordered typed protocol events (deposit, withdrawal, wallet rotation, governance/upgrade, DA/proof reference, verifier attestation), each positioned by `EventOrdinal { tx_index, MessageLocation::Input(n) | Output(n) }`; explicit rejected/duplicate dispositions; next protocol context; kernel/schema version and a deterministic plan hash. The plan and its hash are role-neutral so that core, verifier, and standalone produce identical plans for the same block; role-specific outcomes (which events this node projects vs records as irrelevant) live in a separate per-role `ProjectionReceipt` with its own hash. The plan hash covers canonical serialization only — no timestamps, generated row IDs, or iteration-order artifacts.
- **D. Aggregate database adapter (one per schema family).** Coarse operations only: `load_checkpoint_and_context()`, `apply_block(expected_checkpoint, plan)`, `revert_to(expected_checkpoint, ancestor_anchor)`, `audit_coverage(range)`. `apply_block` is one native transaction: lock/check checkpoint, verify `plan.height == checkpoint.height + 1` and `plan.prev_hash == checkpoint.hash`, insert raw txs idempotently (txid and wtxid recomputed at the boundary, section 5.2), insert inclusions and tracked outputs, exhaustively project every event/disposition, apply protocol-state changes, compare-and-swap the checkpoint, commit. Any failure rolls back the whole block.
- **E. Shared canonical-chain/reorg coordinator.** Header-based common-ancestor discovery, serialized against `apply_block` through the checkpoint. Every adapter implements the same `revert_to` contract: canonical inclusions, tracked-output state, domain projections, protocol context, and checkpoint revert together; immutable raw bytes remain as content-addressed cache. This seam is what prevents the next core/verifier/standalone cleanup omission. `revert_to` carries the same fault-injection/crash matrix as `apply_block`: what it must guarantee is logical atomic visibility (no caller observes or acts on a partially rewound state), not one particular SQL transaction shape.

  **Reorg decision rule.** The safety frontier for automatic revert is downstream consumption, not reorg depth. Confirmation depth is an eligibility delay (when a block may first be processed) and creates no finality; a shallow reorg can require coordinated recovery while a deep one may be locally reversible. Before any destructive change the coordinator classifies the reorg as one of: `NoProcessedImpact` (entirely above the checkpoint: no-op), `ProjectionOnly { ancestor }` (automatic `revert_to` allowed), `DownstreamConsumed { ancestor, affected_effects }` (persist a durable hard-reorg marker, halt, alarm; never partially rewind kernel tables while downstream state stands), or `AncestorUnavailableOrBeyondPolicy` (halt; the edge of the search window is a bound, never assumed to be the ancestor). Per role, the downstream-consumption boundary is: core sequencer — an orphaned event already consumed by a sealed/executed L2 batch; verifier — an input already used for a validation decision or externally emitted signature/attestation; standalone indexer — always reverts when the ancestor is known, publishing a revision signal to readers. The existing reorg detectors already approximate this soft/hard split (`core/node/via_main_node_reorg_detector/src/lib.rs:240-283`, `via_verifier/node/via_reorg_detector/src/lib.rs:243-282`); the rule formalizes it. A configured maximum automatic span may conservatively force manual recovery but never makes a downstream-consumed reorg safe.
- **F. Thin node shells.** Role/capabilities, connection pools, polling, metrics, adapter selection. No copied backfills, no message classification, no wallet transition logic, no reorg algorithms.

### 5.2 Storage model

Transaction identity is the load-bearing subtlety. Bitcoin txid does not commit to witness data (BIP141), and Via inscriptions live in witness data: two transactions can share a txid while carrying different inscription payloads (different wtxids). A raw store keyed by txid alone can interpret a canonical inclusion using an orphaned block's witness bytes. The current code additionally keys parsed messages by ntxid (fact 3.6), a third identity. The kernel uses each identity for exactly one purpose:

| Object | Identity | Purpose |
|---|---|---|
| Raw transaction variant | `(txid, wtxid)`, bytes verified against both on write and read | Prevent witness-variant aliasing |
| Block inclusion | `(block_hash, tx_index)`, carrying txid, wtxid, height | Exact placement on a branch |
| Parsed event occurrence | `(block_hash, tx_index, message_ordinal, event_kind)` | Idempotent replay of an exact plan |
| Tracked output | `OutPoint(txid, vout)` | UTXO identity, independent of witness and remine location |
| Location-derived IDs (e.g. `ViaPriorityOpId`) | `(height, tx_index, vout)` as today | Projection value only — never a cross-reorg dedup key |

- Raw variants are immutable; a transaction orphaned and re-mined keeps its bytes, orphans the old inclusion, gains a new one. A remine with the same txid but a different witness stores a second raw variant, and only the canonical variant's parse is projected. A single mutable locator row cannot represent any of this.
- "Applied exactly once" is defined as two invariants, not txid-seen-once: replaying the same plan/event occurrence is idempotent, and at every settled state exactly one domain effect corresponds to the currently canonical subject (for deposits, the deposit `OutPoint`). Location-derived fields are recalculated for a replacement inclusion — `ViaPriorityOpId` embeds height/index/vout and legitimately changes on remine.
- Byte-order conversion (the duplicated txid-to-H256 reversal in today's processors) centralized at this boundary.
- Tracked output creations and spends with value, script, and role.
- Anchored checkpoint (height, hash, kernel version) per schema family.

### 5.3 Observation closure (versioned specification)

"Store protocol transactions" is not precise enough to be a storage rule. The observer must retain:

- every transaction carrying a recognizable Via inscription candidate, including malformed or currently unsupported candidates (a future parser or incident review may need bytes that are pruned by then);
- every transaction creating an output to a tracked system script/address;
- every transaction spending a tracked output;
- every locally broadcast Via transaction, persisted with raw bytes and `sent_at_height` before broadcast;
- every referenced transaction needed by a recognized message;
- canonical inclusion metadata for all of the above.

The rule is versioned and persisted with the checkpoint. A new kernel version must not silently assume old databases contain observations an older rule never captured. Locators for all Bitcoin transactions are explicitly rejected as a substitute.

### 5.4 Failure-outcome taxonomy

Replaces `.unwrap_or(false)`, bare `continue`, and `Ok(None)` with a closed set. Adapters match exhaustively; adding an event variant must force a compile error or an explicit capability decision in all three shells.

| Outcome | Meaning | Cursor behavior |
|---|---|---|
| Valid | Event valid and projected | Advance after commit |
| Invalid input | Malformed or semantically invalid Bitcoin data | Record rejection; advance |
| Duplicate | Already applied idempotently | Advance |
| Irrelevant to role | Valid event outside this node's projection | Listed in the per-role `ProjectionReceipt`, not a plan disposition (keeps the plan hash role-neutral); advance |
| Unsupported valid event | Recognized version/type this software cannot interpret | Halt; do not advance |
| Missing required dependency | Observation coverage incomplete | Halt/retry; do not advance |
| Infrastructure failure | RPC, DB, serialization, internal error | Roll back/retry; do not advance |

The load-bearing property: unavailability is never classified as invalidity, and the cursor never advances past data the node failed to interpret.

### 5.5 Transaction discipline

Plan construction (block fetch, parsing, dependency resolution) happens outside any database transaction. The commit transaction is short: revalidate the checkpoint, apply, commit. No DB transaction is ever held across an RPC call. Several current processors start and commit their own transactions (core votable processing, verifier withdrawal/verifier processing, standalone withdrawal processing); they must be restructured to operate inside the adapter's outer transaction. This is real work the kernel extraction alone does not buy, and it is the main reason the previous 2,400-5,500 production-LOC estimate for the full program remains plausible.

No asynchronous event bus/outbox initially. One synchronous block-sized transaction is sufficient at Bitcoin throughput; a durable event journal can be introduced deliberately later if a projection measurably cannot fit the boundary.

### 5.6 Intra-block ordering and protocol context (ratified, revised from the original proposal)

Block-level commit atomicity does not by itself pin what happens *inside* a block, and the current behavior (facts 3.4, 3.5: enum-order sorting plus range reparse after wallet updates) is an accident, not a rule. Proposed protocol rule, to be ratified in the slice-0 ADR:

1. Transactions are processed in Bitcoin `tx_index` order.
2. Messages within a transaction are ordered by physical location: inputs before outputs, each by index (`MessageLocation`), never enum declaration order or map iteration order.
3. The WHOLE block is authorized against the block's input context: a rotation never re-authorizes later transactions in its own block. This is stricter than the original per-transaction proposal (which activated transitions mid-block); the stricter rule was chosen because it removes an entire class of ordering ambiguity for one block of activation latency, which Bitcoin's cadence makes irrelevant.
4. Valid transitions fold into `next_context` in ordinal order through the contract's normative `fold_context` function; every engine must use it.
5. A transaction cannot authorize itself with a wallet update it contains (follows from rule 3).
6. One rotation per role per block; a second one is rejected as `ConflictingRoleUpdate`. A second bootstrap, anywhere after the first, is `InvalidBootstrap`. Upgrade activations must strictly increase the protocol version.

This gives deterministic activation without range-wide retroactivity. The whole block still commits or rolls back as one unit; there is no externally visible sub-block state. Conversely, multi-block atomicity is too coarse: if block N is valid and N+1 halts on a missing dependency, N stays committed and the checkpoint rests exactly at N.

### 5.7 Acceptance fixture suite (the judge)

The suite now has 22 registered conformance fixtures plus the adapter-local concurrent-genesis test. The first fifteen were written before any implementation and served as the bake-off judge; family and storage-occurrence fixtures were added with the full event surface. Fixtures assert logical visibility and state invariants, never a particular SQL statement order. They run against the production stack end to end: the conformance harness in `core/lib/via_ingestion_adapter/tests/` clones per-test databases from the migrated family templates and registers the whole suite for the role-configured Postgres adapter; `three_adapter_semantic_equivalence` exercises all three roles. Consolidated from the F1-F5 sketch and the Codex fixture consultation (`research/codex-fixture-opinion.md`).

The original fifteen (the bake-off judge):

1. `deposit_plan_golden` — identical envelope/context/dependencies produce identical canonical plan bytes and hash across runs and restarts; mutating anchor, position, witness bytes, context, or version changes the hash; map insertion order and row order do not.
2. `deposit_block_commits_atomically` — one valid deposit block commits raw variant, inclusion, tracked outpoint, normalized deposit projection, header time, and anchored checkpoint together.
3. `tracked_funding_and_same_block_spend` — a non-protocol funding tx to the bridge script is retained; a later same-block spend resolves by `OutPoint` in transaction order without historical RPC.
4. `deposit_per_output_and_op_return_rules` — multiple bridge outputs follow the decided one-deposit-per-output rule (settling fact 3.7); duplicate OP_RETURN receiver encodings that agree are fine, disagreeing ones are invalid input with a durable typed rejection. The true inscription+OP_RETURN precedence fixture is deliberately NOT claimed here; it becomes mandatory with the inscription parser slice.
5. `apply_fault_matrix_and_commit_ack_loss` — fault injection at every semantic write stage (raw variants, inclusions, tracked outputs, each projection class, context, checkpoint, pre-commit) leaves no partial block; a commit that succeeds but loses its acknowledgement replays without duplicate rows or effects; at least one variant kills the process/connection rather than returning an injected `Err`.
6. `checkpoint_parent_version_and_adjacent_block_guard` — stale checkpoint, wrong parent/height, or kernel/schema-version mismatch writes nothing; a failure in block N+1 leaves committed N intact.
7. `mixed_failure_taxonomy` — valid + invalid input commits both projection and rejection record; valid + missing dependency, valid + unsupported event, or infrastructure failure commits none and does not advance; DB timeout is infrastructure failure, never invalidity; corrupt stored raw bytes are a missing/corrupt dependency, never an RPC fallback.
8. `concurrent_apply_vs_revert_serializes` — racing `apply_block`/`revert_to` from one checkpoint: exactly one wins; the loser reloads from the winner's anchor.
9. `revert_fault_matrix` — reverting a deposit/spend block is logically atomic across inclusion, tracked state, projection, context, and checkpoint; raw variants remain; revert to the current checkpoint is an idempotent no-op.
10. `exact_transaction_remine` — same txid and wtxid in a replacement location: one raw variant, two inclusion occurrences, location-derived fields recalculated, exactly one canonical deposit effect.
11. `same_txid_different_witness_remine` — same txid, different wtxid: two raw variants; the orphaned variant's parsed deposit is never reused for the canonical one.
12. `alternate_branch_spender` — different transactions spending the same tracked outpoint on competing branches leave exactly one canonical spender after reorg.
13. `deposit_reorg_safety_frontier` — projection-only impact auto-reverts for all roles; downstream-consumed impact durably halts core/verifier without partial rewind (marker survives restart); standalone reverts with a revision signal; divergence at the oldest searched height yields `AncestorUnavailableOrBeyondPolicy`, not a rewind.
14. `pruned_restart_has_zero_historical_fallback` — after process restart (fresh instances, empty caches) and source-block pruning, all supported reads (deposit raw tx, tracked `TxOut`, spend state, block time, canonical inclusion) resolve locally; the transport spy records zero forbidden historical calls (txid-only `getrawtransaction`, historical `getblock`, `getblockstats` for envelope-known time), with current-chain header checks allowlisted; deleting one required local row halts with a typed missing-dependency, never a fallback. One end-to-end variant runs against a real `txindex=0` pruned regtest node.
15. `three_adapter_semantic_equivalence` — core, verifier, and standalone commit the same role-neutral plan and checkpoint and expose equivalent normalized deposit facts (canonical outpoint, receiver, amount, anchor, position, header time), with explicit per-role receipts.

The suite was adversarially reviewed by Codex before freezing (`research/codex-fixture-review.md`: verdict "freeze after listed fixes"; all blocking fixes applied). Load-bearing consequences now encoded in the contract crate: an explicit indeterminate-commit outcome with `last_plan_hash` recovery in the checkpoint; the plan bound to its input protocol context via a hashed fingerprint CAS'd with the checkpoint; branch-anchored, subject-discriminated effect identities everywhere effects are referenced; a validated canonical wire format with explicit tags, a domain/version prefix, typed rejection codes, and a frozen golden hash vector; checked-by-construction raw variants; dependency resolution through the production `ObservationReader` with a three-state answer (present / known-absent / unavailable); and a conformance macro — the suite judges only where registered, and all three production adapters must register it in CI.

Deferred with explicit non-claims (each lands with its named slice): real process-kill/connection-drop faults (per-adapter Postgres conformance layer, slices 3-4); the end-to-end `txindex=0` pruned-regtest run (source shell, seam A); inscription+OP_RETURN encoding precedence (parser slice); unsupported-valid-event composition (first versioned event family — the current wire format has no constructible representative).

Backlog, landing with the slice that needs it (full inventory in `research/codex-fixture-opinion.md` Q5): wallet-transition ordering fixtures (the old/new/rotation/old/new five-transaction block of section 5.6) before the first event family whose validity depends on a rotated wallet; confirmation-boundary fixtures (exact heights, zero/one off-by-one cases — fact 3.8) with the source shell; bootstrap witness-commitment fixture before bootstrap (section 6); backfill/coverage/migration fixtures with slices 5-6; envelope integrity, tracked-output lifecycle edge cases, and malformed-input/fuzz boundaries incrementally from slice 2 on.

## 6. Bootstrap ladder (how nodes obtain history under pruning)

1. **Regenesis networks: pruned from day one.** A network started at a recent anchor has no deep history to replay; the observation store covers everything from its start height.
2. **Fresh nodes on an existing network: full IBD, replay, then prune.** Sync Bitcoin unpruned, replay Via observations from the start anchor, verify coverage, then enable pruning (fact 6: this direction is cheap).
3. **DB snapshots (later).** Per-schema-family snapshot with an authenticated manifest: network, schema and kernel versions, observation coverage, processed anchor height/hash, projection cursors. Restore validates the anchor against local headers and checks the next required block is at or above `pruneheight`.
4. **`getblockfrompeer` for narrow gap recovery only** (a few recent blocks), never bulk backfill (fact 4).

Bootstrap transaction: distributed as a versioned artifact of raw transaction bytes plus Merkle proof, verified against local headers. Replaces both today's bare-txid fetch and PR 379's locator config. Caveat (Codex): a normal transaction Merkle proof authenticates the txid only, and the bootstrap message lives in witness data — the artifact must additionally authenticate the wtxid through Bitcoin's witness commitment (coinbase and witness-commitment proof material included), or an equivalently verified construction. This gets its own acceptance fixture before bootstrap implementation.

## 7. What the kernel does not solve

Calling the kernel "pruned-node support" by itself would repeat PR 379's scope error. Parallel workstreams, each gated before pruning is enabled anywhere:

- runtime prune/IBD headroom guard with alerting on any historical RPC fallback;
- descriptor-wallet creation, restore, rotation, rescan readiness;
- pending transaction inclusion discovery and confirmation rollback (the sender must stop depending on 10,000-block scans; match the small pending set against each observed block instead);
- snapshot manifest/restore validation;
- coverage auditor: zero unresolved required raw transactions or tracked outputs across the replayed range.

## 8. Delivery plan

### 8.1 Vehicle (open decision)

Two positions are on the table and the difference matters:

- **Codex's recommendation** (given "refork is a later option" as context): strangler in current via-core main. Merge shared types and the shadow path as short stacked default-off PRs into main; exercise in shadow mode on the live testnet while the archival source still exists; cut over one component at a time; keep the kernel low-dependency so a future refork ports one shared unit instead of three watcher forks.
- **The refork plan** (`docs/refork/HANDOVER.md`): legacy testnet is feature-frozen as a reference network; new work lands on the refork trunk, and `via_btc_client` is the wave-1 pilot. Under this policy, kernel slices target the refork trunk and activate at regenesis, which also makes bootstrap rung 1 (pruned from day one) available immediately.

These converge on everything except which trunk receives the slices. The decision criterion: if the live legacy testnet must itself migrate to pruned operation, codex's in-place strangler sequence (sections 8.3 below) is required; if the legacy network stays on archival nodes until decommission and only the regenesis network needs pruning support, the refork trunk is the cheaper target and the migration machinery shrinks.

**DECIDED (Romano, 2026-07-10): strangler in current main.** The driver is operational cost, not the refork timeline: the current Bitcoin Core Testnet4 node on GCP is being decommissioned to save money, replaced by a new node on Hetzner, and the kernel code is to be tested and run on the current testnet rather than waiting for the refork. Operational sequence implied by the bootstrap ladder (section 6): the Hetzner node starts unpruned (testnet4 is small); via-core is pointed at it and the GCP node retired; pruning is enabled on Hetzner only after the kernel path is live and the coverage audit passes — full-to-pruned is the cheap direction (fact 2.6), so nothing is lost by waiting. The kernel crates remain zksync-free, so the eventual refork ports one shared unit, as planned.

Either way, the anti-pattern is the same: no months-long big-bang rewrite branch. Short stacked vertical slices, each deployable with the feature off, each with a deletion target for the code it supersedes. A short-lived spike branch to prove the trait/transaction shape is fine; it gets discarded, not merged.

### 8.2 Slices

- **Slice 0 (this document plus an ADR).** Settle before code: wallet-update effective ordering; dependency closure; the taxonomy above; checkpoint/transaction invariant; raw/inclusion/tracked-output keys; reorg and same-txid-remine behavior; adapter capability matrix.
- **Slice 1: salvage.** LANDED. The PR 379 keepers (section 4), no locator tables, no backfill.
- **Slice 2: extract envelope-to-plan.** LANDED, in a stronger form than planned: instead of refactoring the legacy indexer in place, a positioned panic-free parser surface plus `ViaProtocolEngine` were added beside it (legacy behavior untouched and pinned by a compatibility test). The engine covers the full event surface, not deposits only. Shadow comparison against the legacy path remains open (slice 6).
- **Slice 3: observation schema plus adapters.** LANDED, revised: the shadow schema is identical across the three DB families (copied migration files, enforced by the migration-drift test), and the "three adapters" collapsed into ONE generic role-configured adapter over three pools, which is the anti-triplication thesis applied to our own code. Role differences are projection sets and the downstream boundary, nothing else.
- **Slice 4: prove atomicity and reorg.** LANDED: all 22 registered conformance fixtures plus the adapter-local concurrent-genesis test pass against real Postgres for all three roles through the conformance harness.

  **Bake-off outcome (2026-07-10).** Two independent implementations of the deposit slice were built against the frozen contract and suite — `spike/kernel-claude` (Claude) and `spike/kernel-codex` (Codex, gpt-5.6-sol ultra), each with in-memory adapters carrying real durability semantics. Both pass all 15 fixtures of the suite as frozen at that time (it has since grown to 22 registered conformance fixtures); a cross-implementation harness then confirmed the two engines produce **byte-identical canonical plans** for identical envelopes — the deterministic-plan property validated across independent implementations. The exercise surfaced two spec defects, both fixed on `spike/kernel-base` before freezing: (#1) dependency discovery was unimplementable for a pure engine until made explicitly conservative; (#2) `message_ordinal` derivation was underspecified — two conforming engines hashed differently until the rule (lowest encoding output index) was pinned in the contract. Reference for the Postgres adapters (slice 3): Codex's implementation (richer edge handling — coverage manifests, empty-checkpoint context binding, durable halt on unknown ancestor), grafting the journal-undo revert from the Claude implementation in place of per-block snapshots (better growth profile). Reports: `research/codex-bakeoff-report.md`, `research/codex-bakeoff-blocked-round1.md`. Neither spike branch merges.
  **Bake-off round two (wire format 3).** After the contract grew to the full event surface, a second independent engine was built from the specification alone (fresh session, reference engine unreadable by instruction, independence attested) and run against the then-21-fixture suite plus a cross-engine corpus. Outcome: byte-identical canonical plans on every case after one divergence, spec finding #3: the contract's rejection code for a recognized message whose referenced transaction is authoritatively absent was underspecified (`InvalidProposal` read as proposal-only). Ruling: the code is generalized to `InvalidReference` (wire tag unchanged), and the ordering rule is pinned in the contract: authorization is judged before reference resolution, so unauthorized messages can never stall ingestion on unavailable references. The contestant engine is kept in-tree as bake-off evidence (`ingestion_engine_v2`, not wired); its one residual documented difference (halt-before-authorization on Unavailable references) is contrary to the pinned rule and is why it must never be promoted without that fix.

- **Slice 5: migrate event families.** Engine-side LANDED (all families interpreted, projected into shadow tables). Cutover of the legacy processors remains with slice 6.
  Original ordering note: Order: deposits/timestamps; tracked-output spends/withdrawals; local broadcast inclusion/confirmation; DA/proof reference chains; verifier attestations and governance; wallet rotations last, after their ordering rule is settled. Capability matrix row per family per role; production cursor never advances past an unimplemented family for that role.
- **Slice 6: backfill, canary, pruning gates.** Deterministic replay from the Via start anchor against an unpruned source; coverage audit at zero unresolved; all section 7 workstreams landed; canary one non-authoritative instance; roll out per family; run `txindex=0` unpruned as a soak with alerts on any historical fallback; rollback snapshots; only then enable pruning. Pruning is the irreversible gate (fact 6). Remove the old path after a post-cutover soak; two permanent ingestion implementations is a non-goal.

### 8.3 Shadow-mode discipline

Shadow comparison against the current implementation validates intended behavior, but the current path is not an infallible oracle: its silent skips and reorg gaps are the defects being removed. Define intentional deltas up front. Valid canonical inputs must produce equivalent protocol results; failure, missing-dependency, reorg, and pruning behavior are checked against written invariants and fault injection, not against legacy output. Storage layout and cursors are expected to differ.

### 8.4 Process guardrails

- One owner/reviewer for protocol semantics; three adapter owners do not independently reinterpret events.
- Every event-adding PR carries the core/verifier/standalone capability matrix.
- PRs sized at roughly one seam or one vertical event family; no more 2,700-line mixed PRs.
- Structural checks: no cursor update outside the approved transaction boundary; no unhandled sibling event variants; migration checksum test across families.
- **Upstream (zksync-era) discipline.** Protocol logic and contracts stay zksync-free: `via_btc_ingestion` and `via_btc_ingestion_tests` must never gain a `zksync_*` dependency (review criterion — a convenient `zksync_types` import in a kernel PR is a defect). Only the thin outer shells (adapters, seam F wiring) may touch zksync APIs, and those are budgeted as rewrite-per-trunk, not ported.
- **Upstream reference checkouts.** Two local directories, both under `~/github/vianetwork-repos/matter-labs/`:
  - `zksync-era-main` — always tracks upstream `main`; keep it current with plain `git pull`.
  - `zksync-era-v29x` — a git worktree of the same repository, permanently checked out at `pin/refork-base-core-v29.20.0` (`2a0c27ef5`), the baseline all Phase 0 refork analysis was done against. Never pull or switch branches here.
  Workflow: before implementing, refactoring, or redesigning anything that touches upstream-adjacent surfaces (DAL patterns, `node_framework`, storage init, reorg detectors), check how CURRENT upstream main does it in `zksync-era-main` before copying the old fork's pattern, and use `zksync-era-v29x` when a claim needs to be reproducible against the refork baseline. If either directory is missing, stop and warn rather than silently skipping the check.
  Moving target caveat: the v29.20.0 pin is the analysis baseline, not a promise about the final refork target. Upstream keeps releasing; when refork execution actually starts, re-evaluate the newest core version, re-pin under the same naming scheme (`pin/refork-base-<version>`), and re-check which seam mappings from the Phase 0 docs still hold.

## 9. Open questions

1. **Vehicle** — DECIDED (section 8.1): strangler in current main; live testnet migrates; new Hetzner Bitcoin node replaces the GCP node.
2. **Wallet rotation effective ordering** — DECIDED and implemented (section 5.6): whole-block authorization against the input context; transitions activate at the next block. Stricter than the first proposal, for determinism.
2a. **Deposit subject rule** (fact 3.7) — DECIDED (Romano, 2026-07-10): **per-output deposits.** Each bridge-paying output is its own deposit subject, keyed by `OutPoint(txid, vout)`, amount = that output's value. This matches the tracked-output model and makes "exactly one canonical effect per subject" well-defined across remines. Remaining sub-rule (proposed): when inscription and OP_RETURN encodings both parse for the same transaction, they must agree on the L2 receiver — agreement is fine (one deposit per output regardless of encoding count), disagreement is invalid input (record rejection, credit nothing).
3. **Standalone indexer** — DECIDED: implements the full `revert_to` contract. The generic adapter serves it with role configuration; the reorg-frontier fixture covers its always-revert behavior.
4. **Prune target sizing**: retention formula (safety margin over deepest reorg plus replay window) and the operational guard's alert thresholds.
5. **Whether PR 379 is closed with feedback or reworked into slice 1 by its author.** Nothing here has been posted to the PR.

## Appendix: evidence and provenance

- `research/codex-pruned-review.md`: Codex design review of the pruned-node problem (go/no-go gates, LOC estimate, migration sequence).
- `research/codex-pr379-review.md`: Codex PR 379 review (request changes, finding matrix).
- `research/codex-kernel-opinion.md`: Codex consultation on this kernel proposal (seams, slices, taxonomy; the backbone of sections 5 and 8).
- `research/glm-pruned-ibd-tailing.md`: GLM research on IBD-tailing and prune retention with Bitcoin Core source citations.
- `research/omp-btc-rpc-inventory.md`: GPT inventory of all BitcoinRpc call sites (classified by transaction age) and the 31 ingestion tables across the three DB families.
- `research/codex-fixture-opinion.md`: Codex consultation on the acceptance fixture suite (gpt-5.6-sol, 2026-07-10): the witness-identity correction, the intra-block ordering rule, the downstream-consumption reorg frontier, and the 15-fixture judge list consolidated into sections 5.2, 5.6, 5.7 and seam E. Consultation prompt preserved as `research/codex-fixture-consult-instructions.md`. Repo claims it made (enum-order sorting, ntxid usage, first-output/sum mismatch, zero-confirmation default) were independently re-verified against HEAD before adoption.
- `research/omp-doc-evidence.md`: evidence pack (GPT-5.6-terra, 2026-07-10, HEAD `27df0dd7caff`): re-verification of every file:line citation in section 3 (all confirmed; three had moved and section 3 carries the corrected locations), reproducible duplication metrics with verbatim commands, refreshed BitcoinRpc call-site inventory, ingestion-table listing per DB family, and PR status snapshot (PR 378 and PR 379 both open and mergeable at capture time).

Methodology note: the overlap ratios in section 3 use a strict metric (distinct exact lines shared after sort, divided by combined line count; identical files score 0.5). Earlier working figures of "86%/41% watcher similarity" from the investigation phase used a looser diff-based measure and are superseded by the reproducible numbers. Line numbers quoted inside the consultant reports reflect their respective analysis dates; where they conflict with section 3, section 3 is current.
