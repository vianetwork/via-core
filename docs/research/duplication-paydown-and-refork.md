# Duplication paydown and the refork

Status: research, 2026-10-20. No paydown plan, extraction, or policy is approved by this note. The
classification is a judgment about which duplicated behavior survives a refork, made under the
assumption that Via keeps Bitcoin settlement and its verifier model. The paydown policy itself is
tracked as an open decision in [issue #419](https://github.com/vianetwork/via-core/issues/419).

Last verified: 2026-10-20.

## Question

via-core has two source trees: `core/` (the main node, derived from Era) and `via_verifier/` (the
Via-owned verifier network). Many Via components exist in both. If the likely refork target,
ZKsync OS, replaces the Era code underneath, is paying down this cross-tree duplication now useful
for the refork, neutral, or wasted work?

A tempting answer is "ZKsync OS may replace the whole sibling layout, so paying down everything now
cleans code that may get rewritten anyway". This note tests that answer.

## Source revisions

| Source | Revision |
| --- | --- |
| via-core `main` | `5e837ac2911b489d953d7c96e31cd85dc23577f4` |
| Duplication measurement, jscpd v5.4.1 exact clones | `eec4bb8f59fff0c23ddc9cdfa21910882ca36be6`; no paired file differs from `5e837ac29` |
| zksync-era `main` | `ff5f519b11cff863edcfa0f75af10fea113806b0` |
| zksync-os `v0.5.x-private` | `5610fb622832acbdf4feeb0ae458a5c3436b2b6c` |
| zksync-os-server `di/fix/87680-fri-fix` | `66ba2731bc8d6ebce1c85dd0fa67054ca2855a40` |
| zksync-os-interface v0.2.0 | `c0f70563ddf78c3be2655b1c698ac8fc80a3d64e` |
| avalanchego, PR #4921 merge | `40177fc694909820e4de100d9b99cc0264c73c5b` |
| celo-org/op-geth `celo-rebase-19` | `c69d62ac387bd3f90e76ce13702cf3f781ec0faa` |
| ethereum-optimism/op-geth Bedrock squash | `c48b8aab5a7aed63710fd26d29697762f53fca52` |

The upstream state of ZKsync OS, and why `v0.5.x-private` rather than `main` is the current line, is
in [Refork target and upstream direction](refork-target-and-upstream-direction.md).

## Findings

### The answer is partly true and mostly misleading

ZKsync OS replaces Era's execution, proving, node composition, and node persistence. Extracting code
whose only purpose is to share today's Era binary bootstrap is a poor refork investment. ZKsync OS
does not replace Via's Bitcoin settlement responsibilities, and a new directory layout does not make
them disappear. Most of today's duplication is that Via behavior.

The measurement covers 41 file pairs between `core/` and `via_verifier/` with 10,142 duplicated
tokens: 39 pairs from the `node` and `lib` scans and 2 pairs between the two server binaries. Every
pair is listed in the [appendix](#appendix-every-measured-file-pair).

| Value of consolidating it now for a refork | Tokens | Share |
| --- | ---: | ---: |
| Helpful: shared Via behavior, persistence semantics, wallet tests, signing examples | 7,858 | 77.5% |
| Neutral: legacy proof formats, generic plumbing, metrics, test setup | 1,987 | 19.6% |
| Wasted, provisionally: sharing the Era binary composition and configuration loading | 297 | 2.9% |

Helpful means one owned implementation or contract would help a port. It does not mean the current
Rust code ports unchanged, or that every helper deserves a new crate. Of the helpful total, 1,484
tokens are wallet behavior tests and 209 are key-aggregation examples. If all 2,155 tokens of Via
PostgreSQL state were counted as neutral, because a future storage decision replaces those queries,
helpful would still be 5,703 tokens, or 56.2%.

Counts are sums of reported clone occurrences per file pair. Ranges overlap within and across pairs,
so they are neither unique coverage nor removable lines. File names do not classify the code: the
`eth_sender_dal.rs` match is a generic query tail, while code under `core/lib/dal` can hold Via
voting, wallet, and reorg semantics.

Corrected statement:

> ZKsync OS changes the Era execution, proof, node-framework, and storage adapters, so extracting old
> bootstrap code only to make it reusable can be wasted work. Via's Bitcoin settlement, ingestion,
> wallet, reorg, signing, and verifier responsibilities still need an owner. Consolidating shared
> behavior behind Via-owned boundaries reduces today's drift and the number of implementations to
> port. Prioritize it by current correctness risk, repeated maintenance cost, and the likelihood that
> its boundary survives. Current work touching a duplicate is a useful trigger, not a limit.

### What ZKsync OS provides in place of Era components

| Part | Evidence | Consequence for Via (inference) |
| --- | --- | --- |
| Node framework | Era injects resources through a [`WiringLayer`](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/node_framework/src/wiring_layer.rs#L23-L38). The OS server composes [typed pipeline components with channels](https://github.com/matter-labs/zksync-os-server/blob/66ba2731bc8d6ebce1c85dd0fa67054ca2855a40/lib/pipeline/src/traits.rs#L8-L32). | Main-node wiring is rewritten. A new shared Era wiring layer is not a portable boundary. A separate verifier service can keep its framework until there is a reason to change it. |
| Persistence | Era uses [PostgreSQL plus RocksDB](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/docs/src/guides/external-node/02_configuration.md#L19-L33). The OS server uses [several RocksDB databases](https://github.com/matter-labs/zksync-os-server/blob/66ba2731bc8d6ebce1c85dd0fa67054ca2855a40/docs/src/design/db.md#L1-L10); no PostgreSQL dependency was found. | Era node DAL adapters are replaced. Via's Bitcoin cursors, inscription attempts, votes, wallet history, and withdrawal obligations are not OS execution state. A separate Via store can survive; whether it stays PostgreSQL is a future choice. |
| State transition storage | Interface 0.2.0 takes [`ReadStorage`, `PreimageSource`, `TxSource` and callbacks](https://github.com/matter-labs/zksync-os-interface/blob/c0f70563ddf78c3be2655b1c698ac8fc80a3d64e/crates/interface/src/traits.rs#L10-L44). | The state transition function does not dictate a schema for Via services. The interface is execution input and output, not a settlement plugin. |
| Configuration, health, metrics | The OS server has its [own configuration system](https://github.com/matter-labs/zksync-os-server/blob/66ba2731bc8d6ebce1c85dd0fa67054ca2855a40/node/bin/src/config/mod.rs#L46-L89). | Adapt at the boundary. Tokio, Axum, and vise code is not Era-only. Sharing loops or metric prefixes only to lower clone counts does not help a refork. |
| Sequencer | Era has the [state keeper, eth_watch and eth_sender](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/docs/src/guides/architecture.md#L48-L69). The OS server has [sequencer, API and batcher subsystems](https://github.com/matter-labs/zksync-os-server/blob/66ba2731bc8d6ebce1c85dd0fa67054ca2855a40/docs/src/design/subsystems.md#L3-L20). | The state-keeper integration is rewritten. Deposit origin, amount, ordering, idempotence, and txid byte order stay independent contracts. |
| Settlement | [`SendToL1`](https://github.com/matter-labs/zksync-os-server/blob/66ba2731bc8d6ebce1c85dd0fa67054ca2855a40/lib/l1_sender/src/commands/mod.rs#L73-L91) requires Solidity calldata; the [watcher](https://github.com/matter-labs/zksync-os-server/blob/66ba2731bc8d6ebce1c85dd0fa67054ca2855a40/lib/l1_watcher/src/tx_watcher.rs#L1-L29) reads mailbox events. No Bitcoin or Celestia adapter exists. | Inscriptions, confirmations, replacement, chain scanning, attestation, and payouts remain Via-owned. Replace the settlement adapter rather than force Bitcoin through Ethereum calldata. |
| DA | The current OS line defines a [commitment scheme and pubdata content](https://github.com/matter-labs/zksync-os/blob/5610fb622832acbdf4feeb0ae458a5c3436b2b6c/zk_ee/src/common_structs/da_commitment_scheme.rs#L11-L83) with version-3 streams. | Reuse the Celestia transport and retrieval contract where compatible; adapt payload and commitment. Old Era pubdata decoders cannot read version-3 payloads. |
| Proofs | The current OS line documents [state, config and output commitments](https://github.com/matter-labs/zksync-os/blob/5610fb622832acbdf4feeb0ae458a5c3436b2b6c/docs/l1_integration.md#L71-L111). The native verifier backend is in a private repository. | The Era and Boojum proof adapter is rewritten with a new program, key, and statement binding. Versioned historical verification stays if history requires it. |
| Log encoding | Interface 0.2.0 keeps an [`L2ToL1Log` layout](https://github.com/matter-labs/zksync-os-interface/blob/c0f70563ddf78c3be2655b1c698ac8fc80a3d64e/crates/interface/src/types.rs#L129-L159). | Some field and byte contracts survive with new owners. Matching log fields do not make batch metadata or proof statements compatible. |

### The verifier service remains a Via responsibility

The [architecture guide](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/docs/via_guides/architecture.md#L38-L47)
assigns Bitcoin proof inscriptions, DA retrieval, proof verification, Bitcoin attestations, and
withdrawals to the verifier. The
[verifier composition root](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/bin/verifier_server/src/node_builder.rs#L136-L196)
wires the coordinator API, withdrawal verification, proof verification, block reversion, and reorg
detection. The upstream sources inspected provide state transition, proving, and a native verifier
wrapper, and they settle through Ethereum contracts. None provides independent Bitcoin-verifier
membership, vote inscriptions, signing rounds, or withdrawal settlement.

Inference, high confidence about responsibility and lower confidence about topology: if Via keeps
its Bitcoin settlement and trust model, those responsibilities stay Via-owned on any execution
target. The `via_verifier/` directory or binary need not survive unchanged; Via could share a library
between binaries or colocate roles, but independent verifier authority and role-specific state remain.

Cross-tree sharing already works in this repository. Verifier crates depend on
`core/lib/via_btc_client`, and both reorg detectors call `core/lib/via_reorg`
([workspace dependencies](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/Cargo.toml#L376-L404)).
A shared Via crate is an existing ownership mechanism, not a new framework.

### Verdict by class

| Class | Tokens | Verdict | Reason, and what would change it |
| --- | ---: | --- | --- |
| W: Bitcoin watch, deposits, governance, wallets | 1,895 | Helpful | One behavior owner reduces role drift and yields port contracts. Changes if an accepted protocol replaces the ingestion. |
| D: Via state in PostgreSQL | 2,155 | Helpful, conditional | One query and schema owner reduces divergent retries, vote thresholds, and wallet history. Changes if a concrete store replacement removes these queries before the benefit pays back. |
| T: Wallet behavior tests | 1,484 | Helpful | One expected wallet contract is useful when the implementation changes. Keep coverage of every role. |
| R: Bitcoin reorg orchestration | 1,180 | Helpful | Canonical-chain and cursor rules survive; ZKsync OS replay does not handle Bitcoin reorg policy. The scan helper is already shared. |
| B: Bitcoin inscription delivery | 706 | Helpful | One delivery implementation serves any target that keeps the inscriptions. Changes if the inscription protocol is retired. |
| S: Indexer and wallet bootstrap | 229 | Helpful | Restart and bootstrap requirements survive; the PostgreSQL lock code may not. |
| K: MuSig2 address examples | 209 | Helpful, low priority | Key aggregation and Taproot derivation do not depend on the execution engine. These are examples, not production signing. |
| P: Era proof, batch and log formats | 606 | Neutral | New proof adapters are rewritten. Sharing immutable versioned codecs can protect history; merging v27 and v28 into one evolving type can break decoding. |
| L: Polling loops and HTTP shutdown | 445 | Neutral | Identical loops run unrelated jobs. A generic scheduler can hide role ordering and failure behavior. |
| H: Async test harness | 385 | Neutral | Timer boilerplate is not a protocol contract. |
| M: Metrics | 339 | Neutral | A shared global can merge separate role series. Helpful only with a common measurement contract and dashboard compatibility. |
| G: DAL scaffolding | 155 | Neutral | Re-exports and generic query tails carry little contract. |
| E: Bitcoin example client config | 57 | Neutral | Matching configuration literals do not show a missing owner. |
| X: Era binaries and config loaders | 297 | Wasted, provisional | The main-node composition is rewritten against the OS server. Changes if both retained binaries need the same loader or a current fix justifies it. |

### Prior art

- **Avalanche consolidated surviving behavior and deferred the rest.** Before moving onto libevm,
  [PR #4921](https://github.com/ava-labs/avalanchego/pull/4921) (merged 2026-02-18) extracted
  the nearly identical state-sync code of Coreth and Subnet-EVM into one engine because fixes had to
  be duplicated. Pinned source shows thin adapters in
  [Coreth](https://github.com/ava-labs/avalanchego/blob/40177fc694909820e4de100d9b99cc0264c73c5b/graft/coreth/plugin/evm/sync_adapter.go#L7-L33)
  and
  [Subnet-EVM](https://github.com/ava-labs/avalanchego/blob/40177fc694909820e4de100d9b99cc0264c73c5b/graft/subnet-evm/plugin/evm/sync_adapter.go#L7-L33)
  over a [shared engine](https://github.com/ava-labs/avalanchego/blob/40177fc694909820e4de100d9b99cc0264c73c5b/graft/evm/sync/engine/client.go#L12-L69).
  The surrounding plan, [issue #4596](https://github.com/ava-labs/avalanchego/issues/4596),
  deprioritized packages that a planned execution change could make redundant, and was later closed
  as no longer the migration plan. The plan changed; the completed extraction stands.
- **Celo replaced its client and kept chain-specific behavior.** Celo moved to the OP Stack with a new
  client and preserved history
  ([launch account](https://forum.celo.org/t/returning-home-to-ethereum-the-launch-of-celo-l2-mainnet/10466),
  [migration specification](https://docs.celo.org/operate/specification/l2-migration)). Its op-geth
  keeps [fee-currency execution](https://github.com/celo-org/op-geth/blob/c69d62ac387bd3f90e76ce13702cf3f781ec0faa/core/celo_evm.go#L1-L24).
  No source shows that Celo consolidated its own duplicates first.
- **Bedrock replaced architecture and kept a small execution delta.** The
  [Bedrock proposal](https://gov.optimism.io/t/final-upgrade-1-bedrock-protocol-upgrade-v2/5548)
  separated consensus, execution, and settlement. op-geth kept specific domain changes such as the
  [deposit transaction](https://github.com/ethereum-optimism/op-geth/blob/c48b8aab5a7aed63710fd26d29697762f53fca52/core/types/deposit_tx.go#L25-L44).
  Legacy l2geth was [deleted after activation](https://github.com/ethereum-optimism/optimism/pull/5972).

Transfer to Via: framework code tied to a known replacement can be left to that replacement.
Chain-specific behavior still needs a deliberate destination, and consolidating it before the move is
an established choice. None of these projects gives an economic threshold for Via.

### Proposals for the paydown policy

These are proposals for [issue #419](https://github.com/vianetwork/via-core/issues/419), not
decisions.

- **Inventory:** keep pinned scan inputs, pair and range tokens, and each pair's class and behavior
  owner. Use clone counts as a coverage aid, not as the measure of value.
- **Priority:** first address duplicated correctness fixes, repeated cross-role changes, and
  surviving behavior with a manageable extraction. Record "replacement before payback" as an explicit
  reason to defer.
- **Exceptions:** record the owner, the behavior, intentional differences, the evidence for
  deferral, and the event that ends the exception. "ZKsync OS will rewrite it" is not a valid reason
  for Bitcoin domain behavior.
- **Completion:** both roles use the shared behavior, the obsolete copy is removed, and observable
  fixtures cover the retained role differences. Sharing storage means one owner, not necessarily one
  live database.
- **Scope:** do not create one crate for every class. Use the established Via owner or a shared crate
  for a coherent behavior, and keep node scheduling, secrets loading, and role-specific adapters in
  their services.

## Limits

- The measurement covers exact clones only. It omits semantic near-clones and code that exists only
  once, such as the Celestia client pair, coordinator authentication, and production MuSig2 signing.
- The verdicts are judgments about surviving behavior, not measured porting effort or development
  time saved.
- The current ZKsync OS line depends on private prover code, and no inspected public server branch pairs with it. The
  integration boundary of a future complete cohort can differ.
- No build, prover run, or Via runtime test was used. Avalanche, Celo, and Optimism test results are
  maintainer-reported, not rerun.

## Open questions

1. Does Via keep its current Bitcoin inscription, governance, attestation, and verifier-authority
   model on the next target, or does a protocol change replace part of it?
2. Does Via keep a separate PostgreSQL store for its own state on ZKsync OS?
3. What does extracting each helpful class cost, measured against its remaining maintenance benefit?

Facts that would reverse the overall verdict: an accepted protocol replacing Via's Bitcoin
settlement model; a supported upstream Bitcoin, Celestia, or verifier implementation that fits Via's
messages, byte order, persisted state, restart, and reorg requirements; or a complete ZKsync OS cohort
with a materially different integration boundary.

## Appendix: every measured file pair

Class letters match the [verdict table](#verdict-by-class). Links span the first through last
matching range in each file at `5e837ac29`; code between ranges is context.

| ID | Core file | Verifier file | Tokens | Class | What the matching ranges do and depend on |
| --- | --- | --- | ---: | --- | --- |
| 1 | [core/lib/dal/src/eth_sender_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/dal/src/eth_sender_dal.rs#L221-L228) | [via_verifier/lib/verifier_dal/src/via_btc_sender_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/verifier_dal/src/via_btc_sender_dal.rs#L107-L114) | 54 | G | Generic fetch/map tail and next function signature; database instrumentation, not a shared Bitcoin algorithm. |
| 2 | [core/lib/dal/src/lib.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/dal/src/lib.rs#L8-L329) | [via_verifier/lib/verifier_dal/src/lib.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/verifier_dal/src/lib.rs#L8-L106) | 101 | G | Connection/pool reexports and role-specific DAL accessor construction; zksync_db_connection markers. |
| 3 | [core/lib/dal/src/models/storage_btc_inscription_request.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/dal/src/models/storage_btc_inscription_request.rs#L8-L62) | [via_verifier/lib/verifier_dal/src/models/storage_btc_inscription_request.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/verifier_dal/src/models/storage_btc_inscription_request.rs#L9-L63) | 277 | D | Inscription request/history row fields and domain conversions; chrono, Bitcoin Txid, persisted signed transaction bytes. |
| 4 | [core/lib/dal/src/via_blocks_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/dal/src/via_blocks_dal.rs#L45-L530) | [via_verifier/lib/verifier_dal/src/via_blocks_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/verifier_dal/src/via_blocks_dal.rs#L38-L114) | 104 | D | Batch-to-inscription linking and stuck-request query tails; SQL, instrumentation, batch identifiers. |
| 5 | [core/lib/dal/src/via_btc_sender_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/dal/src/via_btc_sender_dal.rs#L64-L169) | [via_verifier/lib/verifier_dal/src/via_btc_sender_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/verifier_dal/src/via_btc_sender_dal.rs#L53-L151) | 143 | D | Inflight request selection and commit/reveal history insertion; PostgreSQL joins and persisted transaction state. |
| 6 | [core/lib/dal/src/via_indexer_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/dal/src/via_indexer_dal.rs#L42-L109) | [via_verifier/lib/verifier_dal/src/via_indexer_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/verifier_dal/src/via_indexer_dal.rs#L40-L107) | 282 | D | Initialize/read/update per-module Bitcoin cursor; PostgreSQL idempotent insertion and query instrumentation. |
| 7 | [core/lib/dal/src/via_l1_block_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/dal/src/via_l1_block_dal.rs#L6-L220) | [via_verifier/lib/verifier_dal/src/via_l1_block_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/verifier_dal/src/via_l1_block_dal.rs#L6-L219) | 754 | D | Bitcoin block history, canonical hashes, reorg metadata, priority-batch lookup; sqlx and role-specific connections. |
| 8 | [core/lib/dal/src/via_votes_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/dal/src/via_votes_dal.rs#L40-L112) | [via_verifier/lib/verifier_dal/src/via_votes_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/verifier_dal/src/via_votes_dal.rs#L273-L343) | 184 | D | Count verifier votes and compute threshold outcome; SQL plus verifier-count/threshold semantics. |
| 9 | [core/lib/dal/src/via_wallet_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/dal/src/via_wallet_dal.rs#L9-L106) | [via_verifier/lib/verifier_dal/src/via_wallet_dal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/verifier_dal/src/via_wallet_dal.rs#L9-L106) | 411 | D | Persist, load, and delete system-wallet history in transactions; Via roles and Bitcoin addresses. |
| 10 | [core/lib/prover_interface/src/outputs.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/prover_interface/src/outputs.rs#L50-L63) | [via_verifier/lib/via_verification/src/version_27/types.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/via_verification/src/version_27/types.rs#L123-L136) | 68 | P | Versioned proof object-store key and bincode serialization; Era L1BatchProofForL1/StoredObject contract. |
| 11 | [core/lib/types/src/block.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/types/src/block.rs#L35-L66) | [via_verifier/lib/via_verification/src/version_27/types.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/via_verification/src/version_27/types.rs#L45-L76) | 119 | P | Serialized Era L1BatchHeader fields, including priority operations and system hashes; old proof payload schema. |
| 12 | [core/lib/types/src/commitment/mod.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/types/src/commitment/mod.rs#L100-L121) | [via_verifier/lib/via_verification/src/version_27/types.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/via_verification/src/version_27/types.rs#L76-L97) | 106 | P | Era L1BatchMetadata fields and commitments copied into version-27 proof decoding. |
| 13 | [core/lib/types/src/commitment/mod.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/types/src/commitment/mod.rs#L110-L555) | [via_verifier/lib/via_verification/src/version_28/types.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/via_verification/src/version_28/types.rs#L83-L159) | 137 | P | Version-28 metadata and meta-parameters, including bootloader/default-AA hashes and optional roots. |
| 14 | [core/lib/types/src/l2_to_l1_log.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/types/src/l2_to_l1_log.rs#L10-L29) | [via_verifier/lib/via_verification/src/version_28/types.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/via_verification/src/version_28/types.rs#L128-L147) | 108 | P | L2-to-L1 log field layout and wrappers; serde and Era primitive types, not a complete proof verifier. |
| 15 | [core/lib/via_btc_client/examples/bootstrap.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/via_btc_client/examples/bootstrap.rs#L236-L248) | [via_verifier/lib/via_musig2/examples/key_generation_setup.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/via_musig2/examples/key_generation_setup.rs#L32-L44) | 93 | K | MuSig2 public-key aggregation and Taproot address derivation; Bitcoin/secp256k1/musig2. |
| 16 | [core/lib/via_btc_client/examples/bootstrap.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/via_btc_client/examples/bootstrap.rs#L239-L248) | [via_verifier/lib/via_musig2/examples/verify_partial_sig.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/via_musig2/examples/verify_partial_sig.rs#L64-L73) | 54 | K | Overlapping public-key aggregation/address-creation example fragment; same Bitcoin dependencies. |
| 17 | [core/lib/via_btc_client/examples/bootstrap.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/via_btc_client/examples/bootstrap.rs#L239-L248) | [via_verifier/lib/via_musig2/examples/withdrawal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/via_musig2/examples/withdrawal.rs#L75-L84) | 62 | K | Overlapping aggregation/address-creation example fragment in withdrawal example; same dependencies. |
| 18 | [core/lib/via_btc_client/examples/indexer_init_example.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/via_btc_client/examples/indexer_init_example.rs#L27-L36) | [via_verifier/lib/via_musig2/examples/withdrawal.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/via_musig2/examples/withdrawal.rs#L96-L109) | 57 | E | Bitcoin client example config construction; RPC/auth configuration and ViaBtcClientConfig, no runtime protocol algorithm. |
| 19 | [core/lib/vm_interface/src/pubdata/mod.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/lib/vm_interface/src/pubdata/mod.rs#L35-L40) | [via_verifier/lib/via_da_client/src/types.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/lib/via_da_client/src/types.rs#L54-L59) | 68 | P | Packed L2-to-L1 log prefix encoding; big-endian integer/address/hash bytes used by DA decoding. |
| 20 | [core/node/contract_verification_server/src/lib.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/contract_verification_server/src/lib.rs#L22-L30) | [via_verifier/node/via_verifier_coordinator/src/coordinator/api.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_verifier_coordinator/src/coordinator/api.rs#L39-L47) | 64 | L | Bind TCP listener and serve Axum with graceful shutdown; generic HTTP lifecycle, unrelated server domains. |
| 21 | [core/node/via_btc_sender/src/btc_inscription_aggregator.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_sender/src/btc_inscription_aggregator.rs#L28-L37) | [via_verifier/node/via_block_reverter/src/lib.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_block_reverter/src/lib.rs#L34-L44) | 60 | L | Tokio tick/stop loop and tagged DB acquisition; identical control structure across different tasks. |
| 22 | [core/node/via_btc_sender/src/btc_inscription_aggregator.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_sender/src/btc_inscription_aggregator.rs#L24-L42) | [via_verifier/node/via_btc_sender/src/btc_vote_inscription.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_sender/src/btc_vote_inscription.rs#L22-L40) | 150 | L | Same polling loop, result handling and constructor tail; batch aggregation versus verifier vote inscription. |
| 23 | [core/node/via_btc_sender/src/btc_inscription_aggregator.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_sender/src/btc_inscription_aggregator.rs#L24-L37) | [via_verifier/node/via_btc_watch/src/lib.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_watch/src/lib.rs#L131-L144) | 110 | L | Same polling loop and tagged connection; aggregator versus Bitcoin watcher. |
| 24 | [core/node/via_btc_sender/src/btc_inscription_aggregator.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_sender/src/btc_inscription_aggregator.rs#L28-L37) | [via_verifier/node/via_zk_verifier/src/lib.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_zk_verifier/src/lib.rs#L59-L68) | 61 | L | Same polling loop and tagged connection; aggregator versus proof-verification worker. |
| 25 | [core/node/via_btc_sender/src/btc_inscription_manager.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_sender/src/btc_inscription_manager.rs#L27-L207) | [via_verifier/node/via_btc_sender/src/btc_inscription_manager.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_sender/src/btc_inscription_manager.rs#L20-L183) | 706 | B | Inscription inflight slots, confirmation/stuck handling, broadcast, commit/reveal serialization; also polling/config/DAL glue. |
| 26 | [core/node/via_btc_sender/src/metrics.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_sender/src/metrics.rs#L11-L128) | [via_verifier/node/via_btc_sender/src/metrics.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_sender/src/metrics.rs#L6-L90) | 227 | M | vise labels, histograms/global registration, timestamp/confirmation helper; role metric contracts differ outside clones. |
| 27 | [core/node/via_btc_sender/src/tests/btc_inscription_aggregator_test.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_sender/src/tests/btc_inscription_aggregator_test.rs#L215-L233) | [via_verifier/node/via_btc_sender/src/tests/vote_inscription_test.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_sender/src/tests/vote_inscription_test.rs#L155-L173) | 107 | H | Timed watch-channel toggle and task setup in tests; Tokio and pool/config fixtures. |
| 28 | [core/node/via_btc_sender/src/tests/btc_inscription_manager_test.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_sender/src/tests/btc_inscription_manager_test.rs#L178-L207) | [via_verifier/node/via_btc_sender/src/tests/vote_inscription_test.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_sender/src/tests/vote_inscription_test.rs#L183-L211) | 278 | H | Overlapping timed watch-channel harness and mock run/await; not the tested inscription acceptance rules. |
| 29 | [core/node/via_btc_watch/src/lib.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_watch/src/lib.rs#L79-L206) | [via_verifier/node/via_btc_watch/src/lib.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_watch/src/lib.rs#L144-L310) | 417 | W | Confirmation/cursor bounds, reorg guard, wallet refresh, ordered scan/process/checkpoint; Bitcoin indexer plus DAL. |
| 30 | [core/node/via_btc_watch/src/message_processors/governance_upgrade.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_watch/src/message_processors/governance_upgrade.rs#L45-L59) | [via_verifier/node/via_btc_watch/src/message_processors/governance_upgrade.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_watch/src/message_processors/governance_upgrade.rs#L42-L56) | 84 | W | Fetch referenced governance proposal transaction and parse inscriptions; Bitcoin RPC and Via upgrade messages. |
| 31 | [core/node/via_btc_watch/src/message_processors/l1_to_l2.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_watch/src/message_processors/l1_to_l2.rs#L23-L92) | [via_verifier/node/via_btc_watch/src/message_processors/l1_to_l2.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_watch/src/message_processors/l1_to_l2.rs#L20-L90) | 300 | W | Deposit txid byte reversal, duplicate guard, and Via deposit to Era L1Tx conversion; OS transaction adapter must change. |
| 32 | [core/node/via_btc_watch/src/message_processors/mod.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_watch/src/message_processors/mod.rs#L23-L43) | [via_verifier/node/via_btc_watch/src/message_processors/mod.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_watch/src/message_processors/mod.rs#L45-L65) | 130 | W | Role-DAL processor contract/error conversion plus Bitcoin txid-to-H256 conversion; mixed interface and domain bytes. |
| 33 | [core/node/via_btc_watch/src/message_processors/system_wallet.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_watch/src/message_processors/system_wallet.rs#L1-L241) | [via_verifier/node/via_btc_watch/src/message_processors/system_wallet.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_watch/src/message_processors/system_wallet.rs#L1-L221) | 964 | W | Ordered wallet-update messages, proposal fetch/parser, address validation/history and wallet-role updates; Bitcoin plus SQL. |
| 34 | [core/node/via_btc_watch/src/metrics.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_watch/src/metrics.rs#L1-L5) | [via_verifier/node/via_btc_watch/src/metrics.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_watch/src/metrics.rs#L1-L5) | 58 | M | vise imports and stage label enum; metric declaration prefix only. |
| 35 | [core/node/via_btc_watch/src/test/system_wallets.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_btc_watch/src/test/system_wallets.rs#L10-L209) | [via_verifier/node/via_btc_watch/src/test/system_wallets.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_btc_watch/src/test/system_wallets.rs#L10-L209) | 1,484 | T | Sequencer/governance/bridge wallet update tests compare indexer state with DB state; reusable behavior, SQL-coupled harness. |
| 36 | [core/node/via_main_node_reorg_detector/src/lib.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_main_node_reorg_detector/src/lib.rs#L23-L220) | [via_verifier/node/via_reorg_detector/src/lib.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_reorg_detector/src/lib.rs#L42-L253) | 1,180 | R | Bootstrap/canonical checks, bounded Bitcoin fetch, height-keyed reorg comparison and state updates; shared via_reorg already owns scan helper. |
| 37 | [core/node/via_main_node_reorg_detector/src/metrics.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_main_node_reorg_detector/src/metrics.rs#L1-L5) | [via_verifier/node/via_reorg_detector/src/metrics.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_reorg_detector/src/metrics.rs#L1-L5) | 54 | M | vise imports and reorg label enum; not the reorg detection algorithm. |
| 38 | [core/node/via_node_storage_init/src/indexer.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_node_storage_init/src/indexer.rs#L27-L37) | [via_verifier/node/via_verifier_storage_init/src/indexer.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_verifier_storage_init/src/indexer.rs#L43-L53) | 84 | S | Transactional Bitcoin-watch cursor initialization with lock; Via state ownership, role-DAL adapter. |
| 39 | [core/node/via_node_storage_init/src/wallets.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/node/via_node_storage_init/src/wallets.rs#L16-L56) | [via_verifier/node/via_verifier_storage_init/src/wallets.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/node/via_verifier_storage_init/src/wallets.rs#L23-L58) | 145 | S | Load system wallets at the persisted watch cursor and reject missing initialization; SQL plus Via wallet types. |
| 40 | [core/bin/via_server/src/main.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/bin/via_server/src/main.rs#L16-L100) | [via_verifier/bin/verifier_server/src/main.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/bin/verifier_server/src/main.rs#L19-L68) | 237 | X | Binary allocator/CLI/module declarations and Era protobuf/YAML/environment wallet and secrets loaders; values not read or recorded. |
| 41 | [core/bin/via_server/src/node_builder.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/core/bin/via_server/src/node_builder.rs#L93-L106) | [via_verifier/bin/verifier_server/src/node_builder.rs](https://github.com/vianetwork/via-core/blob/5e837ac2911b489d953d7c96e31cd85dc23577f4/via_verifier/bin/verifier_server/src/node_builder.rs#L42-L55) | 60 | X | Runtime handle and SIGINT wiring/accessor tails; Era ZkStackService composition, no shared protocol algorithm. |
