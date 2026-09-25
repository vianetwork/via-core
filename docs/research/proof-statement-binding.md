---
status: pending
---

# Proof-statement binding

A verified zk proof establishes a transition between two batch commitments. The verifier must ensure those
commitments describe the batch inscribed on Bitcoin and its accepted parent.
[ADR 0008](../adr/0008-bind-proofs-by-chaining-verified-commitments.md) accepts the direction below: chain
verified commitments and open the inscribed root inside each. This note records the evidence, the inspected
implementations, their engineering detail and the obligations that remain before implementation.

Research date: 2026-09-25. Via revision: `269b81cf056b2bb13294a042b93720b4f5500efe`. Discovery used Exa search;
every conclusion rests on the pinned primary sources cited below. Nothing was built, tested or executed.

## Current Via path

- The watcher links `ProofDAReference` to its `L1BatchDAReference` and stores `l1_batch_hash` (the state root)
  and `prev_l1_batch_hash` in `via_votable_transactions`. It checks root linkage between inscriptions.
- `ViaVerifier::loop_iteration` fetches both blobs and discards the inscribed root returned by
  `process_batch_da_reference`.
- Both `version_27` and `version_28` `verify_proof` take `prev_l1_batch.metadata.commitment` and
  `l1_batches[0].metadata.commitment` from the package. `generate_inputs` computes `keccak(Cprev || C) >> 32`.
- The key path follows the package's `header.protocol_version`, and the expected key hash is a constant.
- `via_votable_transactions` has no commitment column. It deduplicates on `l1_batch_hash`.

A completed `true` therefore establishes a valid transition between package-supplied commitments, or reflects the
skip branch that ADR 0007 removes. It does not identify the inscribed batch.

## Commitment structure

`L1BatchCommitment::hash` in `core/lib/types/src/commitment/mod.rs`:

```text
P  = keccak(leaf_index_be_u64 || root || zero_u64 || zero_hash)
M  = keccak(version-specific meta parameters)
C  = keccak(P || M || auxiliary_output_hash)
PI = keccak(Cprev || C) >> 32
```

The root sits inside `P`. An opening supplies the witnesses needed to show that the root is a component of `C`.
A verifier can insert the inscribed root, take the leaf index, metadata and auxiliary hash from the package
as witnesses, and recompute `C`. Under the hash and proof-system assumptions and correct circuit/key
correspondence, the checked transition is bound to that root and the retained parent commitment. This does
not establish batch number, canonicality or consumed message contents. It avoids reconstructing every
auxiliary preimage for the root check; Via's published pubdata cannot supply all those preimages.

The upstream scheduler circuit at `matter-labs/zksync-protocol`
[`33d3aa0a`](https://github.com/matter-labs/zksync-protocol/blob/33d3aa0a1dd9a73175a105b09a3fd54ef3191921/crates/zkevm_circuits/src/scheduler/mod.rs#L1305-L1393),
matching Via's locked `zkevm_circuits 0.150.18`, assigns the final storage root into the committed block header.
It does not by itself certify Via's installed verification key or wrapper chain.

## Directions compared

| Direction | Assessment |
| --- | --- |
| 1. Chain verified commitments with a root opening | **Chosen.** Uses the existing public input and commitment structure. The root check needs one retained commitment per batch and fixed-size hashing; consumed-data checks add work. Historical use requires a trusted anchor, genuine supported proofs, witnesses, authenticated identity and consumed-data openings. None was exercised here. |
| 2. Recompute the commitment from published data | Needs system logs, uncompressed state diffs, event-queue and bootloader-heap preimages and blob commitments that Via does not publish. Highest witness and state burden. |
| 3. Publish the commitment on Bitcoin | Names one producer-declared commitment per batch in advance. Publication alone does not authenticate its root or consumed messages. Still needs direction 1's checks, changes the protocol and cannot cover past inscriptions. Deferred as a possible later addition for new batches. |

## Inspected implementations

| Project | Pinned source | Role for Via |
| --- | --- | --- |
| Alpen ASM | [`alpenlabs/asm@afaf9357` verification.rs](https://github.com/alpenlabs/asm/blob/afaf935727c34acf07967420fe3c4584428f5550/crates/subprotocols/checkpoint/verification/src/verification.rs#L48-L215) | **Primary model with a real proof predicate.** Builds the expected claim from its last verified tip plus posted data, then checks that predicate. |
| Citrea | [`chainwayxyz/citrea@f11527f9` da_block_handler.rs](https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/fullnode/src/da_block_handler.rs#L688-L849) | Compare-before-mark-proven and pending gap handling. Error and persistence handling is not a template. |
| Mina | [`o1-labs/mina-rust@82480cd4` verification.rs](https://github.com/o1-labs/mina-rust/blob/82480cd468f1963b73dc0b700161036411449e4c/crates/ledger/src/proofs/verification.rs#L430-L455) | The consumer supplies the expected state; results are keyed to their request. Error collapsing is not a template. |
| zkSync settlement | [`matter-labs/era-contracts@df2c3baa` Executor.sol](https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L472-L540) | Baseline Via lacks: commit-time `StoredBatchInfo`, proof inputs only from stored records. |
| RISC Zero Zeth | [`boundless-xyz/zeth@3fe7de60` cli.rs](https://github.com/boundless-xyz/zeth/blob/3fe7de60ad08b47dde876bfc874e0f6069daff1f/crates/host/src/bin/cli.rs#L77-L107) | Narrow example: the host checks the journal's block hash against the requested one. No canonicality or parent persistence. |
| RISC Zero Steel | [`boundless-xyz/steel@f6fc6297` Steel.sol](https://github.com/boundless-xyz/steel/blob/f6fc6297c938d7acc0563337da136d034c3cb67b/contracts/src/Steel.sol) | Not a fit: the chain anchor is an EVM contract. |
| Rollkit | [`rollkit/rollkit@3f67d1bd` executor.go](https://github.com/rollkit/rollkit/blob/3f67d1bd9ac813c74168f3c810763e07c218c778/block/internal/executing/executor.go) | Not a fit in the inspected execution/sync paths: no zk-proof consumer established. |
| Starknet-on-Bitcoin, Raito, ZeroSync | Exa results only | No batch-accepting consumer established. Possible light-client follow-ups. |

## Engineering detail

### Alpen ASM

- **Abstraction.** The consumer owns the statement. `construct_full_claim(last_verified_tip, new_tip, sidecar)`
  builds a `CheckpointClaim { epoch, l2_range, asm_manifests_hash, state_diff_hash, ol_logs_hash,
  terminal_header_complement_hash }`. `l2_range.start` comes from the retained tip, never the payload.
- **Control flow.** `handle_checkpoint_tx` authenticates the sequencer envelope, checks progression
  (epoch advances by one, L1 height does not regress, the L2 slot advances, the range stays within one predicate
  boundary), obtains manifest hashes, then calls `CheckpointState::advance`. `advance` validates everything before
  its first mutation; `verify_proof` passes the reconstructed SSZ claim to the active predicate.
- **Errors.** Every `InvalidCheckpointPayload` variant means log and ignore, with no state change. Missing
  auxiliary data the runtime itself requested is a panic, not an invalid checkpoint.
- **Keys.** Predicate rotations are queued with strictly increasing boundaries and promoted only when the verified
  tip reaches the boundary. The active predicate governs `verified_tip + 1`, so the payload cannot choose a key.
- **Proof condition.** The binding comparison is a zk-proof precedent only with the real proof predicate.
  Other supported predicates include signature and development behaviors; an accepting predicate is not proof.
- **Restart.** `fetch_canonical_asm_state_blocking` resolves the latest persisted state on the canonical chain,
  not the highest stored key, so orphaned states after a reorg cannot outrank the canonical tip.
- **Not shown.** The disk-commit protocol.

### Citrea

- **Identities.** `SequencerCommitment { merkle_root, index, l2_end_block_number }`, identified by SHA-256 of its
  Borsh bytes. The guest's output carries `sequencer_commitment_hashes`, state roots and the index range.
- **Control flow.** `process_zk_proof` selects a method ID by fork height and verifies the receipt.
  `process_tangerine_zk_proof` then compares each output commitment hash with the one recorded at that index
  (`verify_sequencer_commitment_hash_by_index`), checks the predecessor state root, and only then records the
  proof and sets `Proven`.
- **Lifecycle.** Proofs are `Discarded`, `Pending` or `Proven`. A missing predecessor is `Pending`, not an error.
  Pending proofs retry in `(min, max)` index order and stop at the first still-pending one. The light client
  advances its head only along a contiguous, root-consistent chain.
- **Do not copy.** On the first sighting, any non-halting proof error, including a database error, is logged
  as "skipping" and the scan cursor then advances, creating a proof-loss risk. Proof data, status and pending
  removal are separate writes in the inspected path. Crash atomicity was not established or exercised.

### Mina

- **Abstraction.** `get_message_for_next_step_proof` discards the proof-carried application state and inserts
  the expected state. `verify_block` hashes the received protocol state, which commits the previous-state hash,
  height and genesis constants.
- **Concurrency.** Verification runs on a dedicated OS thread fed by a channel. Results carry only `req_id`; the
  reducer maps them back through its job table and drops any result whose job is no longer `Pending`, so a late
  result cannot approve a different block.
- **Do not copy.** `verify_block` collapses every failure, including internal errors, to `false`, and the candidate
  reducer marks the block invalid forever. The service also offers `skip_proof_verification`.

### zkSync settlement contract

- `_commitOneBatch` checks the next batch number, system logs, the previous batch hash, mode-dependent
  pubdata conditions and the priority-operation hash, then constructs
  `StoredBatchInfo { batchNumber, batchHash = newStateRoot, …, commitment }` and stores its hash.
  Validium mode does not establish the same pubdata availability checks as rollup mode.
- `_proveBatches` requires caller-supplied previous and committed records to match the stored hashes before
  deriving public inputs. The commitment layout is the same three-part shape as Via's `L1BatchCommitment`.
- Via has no contract performing the commit step, so its verifier must own these comparisons.

## Patterns and their Via owners

The following maps mechanisms to existing owners and **candidate changes**, not implemented behavior or
an accepted schema/API. The root-opening direction is accepted; these integration details still need proof.

| Pattern | Seen in | Candidate Via owner or existing boundary |
| --- | --- | --- |
| The consumer builds the expected statement from its retained parent | Alpen, zkSync | Extend the canonical-inscription candidate query to obtain the retained commitment; persist it at the verdict boundary. The current row has no such column. A verified genesis or approved checkpoint must be selected separately. |
| Rebuild the statement from independently selected data plus witnesses | Alpen, Mina | Reuse `core/lib/types/src/commitment/mod.rs`, where `L1BatchPassThroughData` is private; expose an appropriate opening operation rather than copy hashing into `via_verification`. |
| The verifier API receives the expected statement | Mina | Change `verify_proof` to accept expected previous and current commitments rather than selecting them from the package. |
| Compare everything, then change state | Citrea, Alpen | Extend the existing verdict transaction in `loop_iteration` to persist commitment and dependent effects atomically; prove source identity is still current at commit. |
| Consumer state authorizes keys | Alpen, Citrea, Mina, Zeth | Resolve the key and format through authenticated protocol-version history and `via_protocol_versions_dal`, not package `header.protocol_version` alone. |
| Apply results only to their still-current request | Mina | Existing verdict updates match `(l1_batch_number, proof_reveal_tx_id)` and use a reorg gate. Demonstrate that these boundaries also protect the complete new evidence identity. |
| Restart from accepted canonical state | Alpen | Retain the commitment with its verdict, not a separate latest-commitment cache. Demonstrate atomic rewind for every existing reverter and source-replacement path; do not assume the current reverter already covers a new field. |

Do not adopt any bypass that returns the same result as real verification, error-to-invalid collapsing,
skip-and-advance on infrastructure errors, or split non-atomic verdict writes.

## Open questions and obligations

- **Opening mismatch meaning.** Citrea treats a mismatch as a discarded proof and waits for another. Via ties one
  proof inscription to each batch row. Whether a failed opening makes batch N invalid or leaves it unverified must be
  decided under ADR 0007's distinction between invalid and permanently unusable evidence.
- **Parent root inside the proof.** Upstream carries the previous batch hash as a system log hashed into the
  auxiliary output. An additional opening against the retained parent root may be possible; whether Via's circuit
  emits that log with the same meaning is not established.
- **Batch identity.** The commitment has no explicit batch-number field. Demonstrate a circuit constraint on height
  or sound sequential attribution from the trusted anchor. The logical evidence identity is
  `(N, root, commitment, source transactions)`, with network, authorized protocol and parent context, not root
  uniqueness alone. Its schema and API are not selected; concurrent completion and source replacement need proof.
- **Consumed pubdata.** Every pubdata item used for deposits, upgrades and withdrawals needs an opening against the
  commitment, for both proof versions, both DA backends and every commitment mode. Validium mode zeroes blob hashes.
- **Circuit and key correspondence.** Tie v27 and v28 decoding, the scheduler and wrapper artifacts, key digests and
  authorized activation history together, including byte order and the 32-bit shift.
- **Anchor and history.** The chain needs a verified genesis commitment or an explicitly approved checkpoint.
  Surviving genuine proofs and witnesses are necessary, not sufficient: authorized formats/keys, consumed-data
  openings and canonical batch attribution must also be established before historical use.
- **Persistence and rollback.** Verdict, commitment and effects persist atomically. Proof-source, deposit-source,
  vote-only and inscription-chain reorgs rewind the commitment with its verdict.

These must be shown by offline verification with genuine supported proofs before implementation or any
historical trust statement.

## Pinned source references

Every file below was read at the stated commit. Agent working reports and raw search results remain in the
untracked local research archive; this list preserves the primary sources they relied on.

- **zkSync scheduler circuit** (`zksync-protocol`, matching Via's locked `zkevm_circuits 0.150.18`):
  [storage](https://github.com/matter-labs/zksync-protocol/blob/33d3aa0a1dd9a73175a105b09a3fd54ef3191921/crates/zkevm_circuits/src/scheduler/mod.rs#L466-L513), [link](https://github.com/matter-labs/zksync-protocol/blob/33d3aa0a1dd9a73175a105b09a3fd54ef3191921/crates/zkevm_circuits/src/scheduler/mod.rs#L946-L1020), [recursion](https://github.com/matter-labs/zksync-protocol/blob/33d3aa0a1dd9a73175a105b09a3fd54ef3191921/crates/zkevm_circuits/src/scheduler/mod.rs#L1277-L1291), [header](https://github.com/matter-labs/zksync-protocol/blob/33d3aa0a1dd9a73175a105b09a3fd54ef3191921/crates/zkevm_circuits/src/scheduler/mod.rs#L1305-L1393), [hash](https://github.com/matter-labs/zksync-protocol/blob/33d3aa0a1dd9a73175a105b09a3fd54ef3191921/crates/zkevm_circuits/src/scheduler/block_header/mod.rs#L28-L190), [blobs](https://github.com/matter-labs/zksync-protocol/blob/33d3aa0a1dd9a73175a105b09a3fd54ef3191921/crates/zkevm_circuits/src/scheduler/mod.rs#L1160-L1211)
- **zkSync settlement** (`era-contracts` at Via's contracts pin):
  [commit](https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L32-L118), [store](https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L328-L338), [prove](https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L472-L540), [hash](https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L575-L633)
- **Citrea**:
  [identity](https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/sovereign-sdk/rollup-interface/src/state_machine/da.rs#L18-L39), [block](https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/sovereign-sdk/rollup-interface/src/state_machine/block.rs#L14-L80), [guest](https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/sovereign-sdk/module-system/sov-modules-stf-blueprint/src/lib.rs#L464-L722), [verify](https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/fullnode/src/da_block_handler.rs#L621-L674), [consume](https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/fullnode/src/da_block_handler.rs#L688-L849), [equal](https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/fullnode/src/da_block_handler.rs#L1009-L1042), [light](https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/light-client-prover/src/circuit/mod.rs#L116-L227)
- **Alpen** (`alpen`, `asm`, `strata-common`):
  [guest](https://github.com/alpenlabs/alpen/blob/882107abd8ec691f870dced0a680267124dd8294/crates/proof-impl/checkpoint/src/statements.rs#L89-L217), [execute](https://github.com/alpenlabs/alpen/blob/882107abd8ec691f870dced0a680267124dd8294/crates/proof-impl/checkpoint/src/statements.rs#L258-L318), [restart](https://github.com/alpenlabs/alpen/blob/882107abd8ec691f870dced0a680267124dd8294/bin/strata/src/checkpoint_reconcile.rs#L110-L138), [canonical](https://github.com/alpenlabs/alpen/blob/882107abd8ec691f870dced0a680267124dd8294/crates/storage/src/node_storage.rs#L179-L223), [handler](https://github.com/alpenlabs/asm/blob/afaf935727c34acf07967420fe3c4584428f5550/crates/subprotocols/checkpoint/subprotocol/src/handler.rs#L21-L126), [proof](https://github.com/alpenlabs/asm/blob/afaf935727c34acf07967420fe3c4584428f5550/crates/subprotocols/checkpoint/verification/src/verification.rs#L48-L215), [state](https://github.com/alpenlabs/asm/blob/afaf935727c34acf07967420fe3c4584428f5550/crates/subprotocols/checkpoint/verification/src/state.rs#L18-L205), [predicate](https://github.com/alpenlabs/strata-common/blob/4b7fd841447910cd9b29b057c02d847b91d23872/crates/predicate/src/verifiers/sp1_groth16.rs#L14-L55), [dispatch](https://github.com/alpenlabs/strata-common/blob/4b7fd841447910cd9b29b057c02d847b91d23872/crates/predicate/src/verifiers/verifier_type.rs#L67-L89)
- **Mina** (`mina-rust`):
  [verify](https://github.com/o1-labs/mina-rust/blob/82480cd468f1963b73dc0b700161036411449e4c/crates/ledger/src/proofs/verification.rs#L750-L785), [input](https://github.com/o1-labs/mina-rust/blob/82480cd468f1963b73dc0b700161036411449e4c/crates/ledger/src/proofs/verification.rs#L430-L455), [hash](https://github.com/o1-labs/mina-rust/blob/82480cd468f1963b73dc0b700161036411449e4c/crates/ledger/src/scan_state/protocol_state.rs#L14-L53), [fields](https://github.com/o1-labs/mina-rust/blob/82480cd468f1963b73dc0b700161036411449e4c/crates/ledger/src/proofs/public_input/protocol_state.rs#L14-L238), [service](https://github.com/o1-labs/mina-rust/blob/82480cd468f1963b73dc0b700161036411449e4c/crates/node/common/src/service/snarks.rs#L36-L77), [consume](https://github.com/o1-labs/mina-rust/blob/82480cd468f1963b73dc0b700161036411449e4c/crates/node/src/transition_frontier/candidate/transition_frontier_candidate_reducer.rs#L73-L172)
- **Zeth**:
  [guest](https://github.com/boundless-xyz/zeth/blob/3fe7de60ad08b47dde876bfc874e0f6069daff1f/guests/stateless-client/src/lib.rs#L21-L34), [core](https://github.com/boundless-xyz/zeth/blob/3fe7de60ad08b47dde876bfc874e0f6069daff1f/crates/core/src/lib.rs#L94-L116), [host](https://github.com/boundless-xyz/zeth/blob/3fe7de60ad08b47dde876bfc874e0f6069daff1f/crates/host/src/bin/cli.rs#L77-L107)
