---
status: pending
---

# Historical upgrade reconstruction

This question is how to reconstruct historical upgrade messages without changing the bytes governance approved or the transaction that executed. The choice between preserving an existing interpretation and correcting reconstruction remains open. Source code alone cannot decide that choice. This governance evidence boundary is independent of the [deposit census](deposit-compatibility-evidence.md).

## Proposal bytes, approval, and execution are separate records

The inspected Via baseline is `8a49f355bfe31720f21b8181db471243709194e5`. The [upgrade flow guide][flow] separates deployment of supporting binaries and system-contract bytecode, finalization of that bytecode's batch, and governance execution on Bitcoin. The [execution guide][execution-guide] supplies `upgradeProposalTxId` to a transaction signed by the governance wallet. Neither guide is proof of what any historical deployment actually ran.

[`SystemContractUpgradeProposalInput`][types] is the decoded proposal data. The producer [`build_system_contract_upgrade_message_script`][producer] writes the message tag, packed semantic version, bootloader hash, default-account hash, recursion-scheduler verification-key hash, and ordered `(address, hash)` pushes for `system_contracts`. The producer's current field order is evidence for that producer revision, not a reconstruction oracle for every historical producer. The parser supplies `evm_emulator_code_hash: None` on this proposal path; that field must not acquire a historical value merely because a newer VM supports it.

[`parse_system_input` and `parse_system_message`][parser] read the witness script's instructions, locate the Via marker, and dispatch the proposal.
[`parse_system_contract_upgrade_message`][proposal-parser] returns the packed version, base-system hashes, verification-key hash, and reconstructed ordered deployment list.
The exact instruction slice supplied to that function is part of the format.
Raw script length, decoded instruction count, and the number of intended deployment pairs are different measurements.
Comparing only the producer's list with a high-level parser result omits the reconstruction boundary.

[`parse_op_return_protocol_upgrade`][approval] reads the approval's proposal ID into `SystemContractUpgradeInput.proposal_tx_id` and retains input outpoints. The [indexer's governance check][authorization] compares the first referenced input output's script with the current governance wallet script. The [main watcher processor][main] then fetches that proposal transaction and reparses it with `MessageParser`, using the approval message's block height as the parser context. It does not receive a pre-approved `ProtocolUpgradeTx` directly from Bitcoin.

Consequently, evidence must keep the proposal transaction, approval transaction, governance wallet epoch, producer artifact, decoder artifact, and execution result distinct. A valid approval reference demonstrates which proposal is selected. It does not by itself demonstrate that two decoders reconstruct the same deployment list or calldata.

## The reconstruction determines a canonical transaction

[`ViaProtocolUpgrade::create_protocol_upgrade_tx`][conversion] and `get_canonical_tx_hash` both call `get_calldata`. That function ABI-encodes the ordered deployments as tuples of bytecode hash, address, `false`, zero value, and empty input, and prefixes the selector for `forceDeployOnAddresses((bytes32,address,bool,uint256,bytes)[])`. Order is preserved. The canonical `L2CanonicalTransaction` uses the protocol-upgrade type, force-deployer sender, contract-deployer destination, fixed gas fields, and `version.minor` as nonce. The emitted `ProtocolUpgradeTx` uses the same minor version as `upgrade_id`.

The proposal's full semantic version therefore has a different role from the transaction's minor-version nonce. Bootloader, default-account, and verification-key hashes also have their own persisted roles. A compatibility result needs to compare the ordered deployment list, exact calldata, canonical hash, semantic version, and the accompanying system hashes. Matching only the displayed version or proposal ID is insufficient.

The [main governance processor][main] skips versions no newer than `last_seen_protocol_version`, creates a `ProtocolUpgrade`, applies it to the latest stored protocol version, and calls `save_protocol_version_with_tx`. Its in-memory last-seen version changes after the persistence loop. The [verifier sibling][verifier] reconstructs the canonical hash using the same `ViaProtocolUpgrade`, filters proposals older than `get_sequencer_version()`, and persists version, bootloader hash, default-account hash, canonical hash, and recursion-scheduler key hash through `via_protocol_versions_dal`. These are related but not identical acceptance and storage paths.

The verifier's [`save_protocol_version`](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/verifier_dal/src/via_protocol_versions_dal.rs#L17-L83) writes `protocol_versions` keyed by the minor-version `id` and `protocol_patches` keyed by minor and patch. On an existing minor version, it updates `upgrade_tx_hash` and resets `executed` to false rather than replacing all accompanying hashes. The writes share a database transaction. A replay assessment must therefore compare the full record and its execution flag, not assume that a repeated version is either a no-op or a complete replacement.

The [state keeper][keeper] loads an upgrade transaction when the batch's protocol version changes, with a separate inherited first-shared-bridge-batch case. `process_upgrade_tx` requires it to be the first transaction and rejects failed execution. On restart with pending L2 blocks, `load_protocol_upgrade_tx` checks that the first pending transaction is an upgrade transaction and avoids injecting another copy. Reconstructing a different transaction after approval or after a partial batch is therefore not a harmless decoder cleanup. The pending transaction, persisted upgrade transaction, and eventually sealed batch need to agree.

## The evidence gate for short tails and retries

A bounded historical evidence bundle would contain, for each supported network and upgrade:

- The raw proposal and approval transactions, ordinary and normalized transaction identities, canonical Bitcoin block hashes, governance wallet epoch, and approval outpoints.
- The producer's source revision and immutable image digest, its input deployment list, and the parser revision used by each consuming role. Current source cannot establish an old image's behavior.
- The protocol-marker-relative instruction sequence, complete fixed fields, ordered tail pushes, and both candidate reconstruction results. Small tails must be explicit cases, not extrapolated from a long producer example.
- Exact canonical calldata and transaction hash, persisted main-node protocol version and upgrade transaction, verifier version/hash record, and the historical VM and system-contract hashes.
- The execution transaction, L2 block and batch, status, and deployed-address bytecode hashes. For an interrupted upgrade, the pending-batch record and restart result belong in the same join.

This is a proposed evidence standard, not a claim those historical records have been collected. The supported history, archive owner, and sufficiency thresholds remain human decisions. No production history or live database was queried for this research.

Short-tail coverage needs an empty deployment list, one pair, several pairs, an unmatched final push, malformed field lengths, and terminal script instructions. The outcome must be recorded as a reconstructed transaction, a deterministic rejection, or an unfinished operation requiring retry. A test of ABI encoding alone does not cover those parser outcomes. The existing [`ViaProtocolUpgrade` tests][conversion] check calldata construction and contract ABI equivalence; they do not establish historical proposal-to-execution agreement.

The [watcher][watcher] advances its cursor after message processors succeed. A proposal fetch or propagated database failure therefore leaves the range unfinished. A parser result of no proposal and an operational failure are not interchangeable: one may complete the range without an upgrade, while the other retries. Any corrected interpretation must specify both cases rather than silently converting one into the other. The governance wallet history and the canonical Bitcoin branch also matter on replay. This inspection did not prove a complete cross-database rollback procedure for historical upgrades, so a future proof must include approval removal and re-inclusion rather than infer safety from cursor behavior.

## What local newer zkSync actually does

The inspected **local newer zkSync** revision is `ff5f519b11cff863edcfa0f75af10fea113806b0`, not verified latest upstream. Its [`DecentralizedUpgradesEventProcessor`][era-upgrades] does not decode Bitcoin tails. It begins with `UpgradeTimestampUpdated`, decodes the old semantic version, and asks `scheduled_protocol_version` for the new version and scheduling block. It searches `diamond_cut_for_version` under the old-version key starting at that block, then uses the scheduled new-version key only as a fallback. The distinction supports modern CTMs that can rewrite a cut and legacy CTMs that keyed the cut differently.

`ProtocolUpgrade::try_from_diamond_cut` reconstructs the upgrade through the preimage interface. The processor rejects a non-newer decoded version, warns rather than fails when the cut declares a version different from the originally scheduled one, and gives `verifier_address_for_version` precedence over an embedded verifier address. Before applying each upgrade, it requires the database's latest semantic version to match the event's old version. Same-minor patch upgrades must preserve base-system-contract hashes and have no upgrade transaction. These concrete checks are stronger evidence of the intended historical lookup contract than copying a type name.

The local newer [`eth_watch` tests][era-tests] include cut-replacement scenarios and a modern CTM that re-emits only the old-version-keyed cut.
The tests were inspected, not executed.
They show that version selection needs the scheduling context and contract generation, not just the latest decoder.
They do not authorize CTM replacement semantics for a Bitcoin proposal whose approved bytes are fixed.

## Two other mechanisms preserve exact execution identity

Optimism `v1.9.5`, commit `5662448279e4fb16e073e00baeb6e458b12a59b2`, implements [`EcotoneNetworkUpgradeTransactions`][op-upgrade] as an ordered list of serialized deposit transactions. The code fixes each transaction's sender, target, gas, deployment bytecode or calldata, and an `UpgradeDepositSource` with a distinct intent string. It includes deployment transactions followed by proxy updates and activation calls. This is a concrete model for golden serialized upgrade transactions. It is not a general proposal-tail parser and does not prove Via's historical activation condition. The inspected function establishes deterministic construction, not a complete Optimism restart or reorg proof.

OpenZeppelin Contracts `v5.0.2`, commit `dbb6104ce834628e473d2173bbc9d47f81a9eec3`, binds governance operation identity to exact bytes in [`TimelockController`][timelock]. `hashOperationBatch` hashes `targets`, `values`, `payloads`, `predecessor`, and `salt`. `scheduleBatch` and `executeBatch` compute that same identity. `_timestamps` drives `Unset`, `Waiting`, `Ready`, and `Done`; execution checks readiness and predecessor completion, performs the calls, and marks completion only afterward. A failed call reverts rather than marking the operation done, allowing a later retry of the same ready operation. Successful execution cannot repeat under the same completed identity.

The transferable requirement is that approval identity and execution bytes remain bound. Via does not have this Solidity storage or EVM transaction atomicity across its Bitcoin watcher and PostgreSQL state. Adding a Timelock-style hash now would not retroactively prove what an old proposal meant. Nor should an Optimism-style fixed upgrade transaction replace evidence about Via's actual approved deployment list.

## Re-fork requirements

The Via-owned contract consists of the governance approval reference and wallet epoch, proposal byte interpretation, ordered deployment list, semantic version, canonical calldata/hash, and historical execution identity. Its current source owners are `via_btc_client`'s producer and parser, both governance processors, `ViaProtocolUpgrade`, protocol-version DALs, and the Via state keeper. The newer-zkSync interfaces closest to the adaptation point are `ProtocolUpgrade`, `ProtocolUpgradeTx`, `ProtocolUpgradePreimageOracle`, `ProtocolVersion::apply_upgrade`, and `save_protocol_version_with_tx`. They are not a ready-made Bitcoin governance implementation.

A re-fork must preserve approved bytes before adapting to newer VM fields, verification-key variants, system-contract ABIs, or schema changes. Golden evidence needs raw proposal-to-instruction decoding, ordered deployment-list-to-calldata encoding, canonical transaction hashing, version persistence, and pending-batch replay. Cross-role comparison must cover the main processor's version gate and the verifier's distinct gate. An adapter cannot silently add an emulator hash, reorder deployments, change the minor-version nonce, or regenerate old calldata with a new ABI.

No new reconstruction rule, activation height, or historical compatibility branch is selected here. Preserving current reconstruction everywhere can preserve an unintended historical result; correcting it everywhere can alter approved execution. The evidence join decides which histories are equivalent and which need an explicit governance decision. The remaining human choice is the supported history and evidence gate, followed by a justified treatment of any mismatch and a defined rejection/retry contract. Deposit census completion supplies none of those missing governance records.

## Required data must not become an empty successful result

The full producer envelope gives a stronger reconstruction boundary than a tail-length expression. [`build_basic_inscription_script`][producer-envelope] emits the public key, `OP_CHECKSIG`, `OP_FALSE`, `OP_IF`, and Via marker. `complete_inscription` appends `OP_ENDIF` after the proposal fields and pairs. A replacement contract needs to decide complete-envelope validity, instruction-decoding errors, fixed-field widths, terminal instructions, and complete pair boundaries together. This source inspection does not select replacement arithmetic or prove any historical short-tail output.

The retry contract must cover authorization before proposal retrieval, not only the proposal processors. [`is_valid_gov_upgrade`][authorization] needs the first approval input's parent transaction. Unavailable parent evidence must remain distinguishable from a completed authorization rejection through every caller. A success-only cursor update cannot supply that distinction if an earlier layer has already classified the lookup as a negative result.

The recommended contract separates immutable malformed bytes, unsupported interpretation, and unavailable evidence. A complete empty deployment array is not automatically an absent proposal: base-system and verification-key fields still exist. Complete one-pair and multiple-pair arrays retain their order. An unmatched push, invalid field width, or truncated push is a deterministic format outcome once exact transaction bytes are available. A failed parent or proposal lookup is unavailable data and should leave the range unfinished. An authorized reference to malformed or unsupported content should stop for diagnosis rather than disappear as an ordinary non-proposal.

These are recommendations, not accepted parser semantics. Skipping invalid approvals could be a deliberate governance policy, but it must not be an accidental consequence of `Option::None`. Retrying immutable malformed bytes forever has the opposite problem: it repeats work without identifying the required human disposition. A narrow shared parsing result and propagated transport errors fit the existing owners better than a new upgrade service.

## Execution recognition has its own evidence limits

A historical comparison must retain the pending transaction itself and compare its canonical identity with the persisted and reconstructed upgrade. Recognizing the transaction's upgrade type alone cannot establish equality of the approved execution bytes.

The verifier's [`get_in_progress_upgrade_tx_hash`][verifier-dal] and [`verify_upgrade_tx_hash`][verifier-execution] connect the pending upgrade record to pubdata evidence and execution recognition. The evidence join must retain the selected version, expected hash, matching log, verification outcome, and reorg context. These records do not independently prove deployed bytecode or historical proof validity. Upgrade clearance therefore depends on the [proof-result and historical-trust work](proof-verification-and-history.md), while remaining independent of deposit census completion.

The main [`save_protocol_version_with_tx`][main-dal] atomically inserts the system transaction and protocol version. This does not establish atomicity across the main database, verifier database, watcher cursor, and executed batch. Neither a fresh decoder nor replay through the verifier's existing conflict update is an authorized recovery procedure.

## Concrete activation and failure contracts in other chains

Cosmos SDK `v0.50.11`, resolved and fetched at `eb1a8e88a4ddf77bc2fe235fc07c57016b7386f0`, separates a scheduled plan, handler availability, and completion. [`Plan.ValidateBasic`][cosmos-plan] requires a nonempty `Name` and positive `Height`, and rejects the deprecated time-based and embedded-client-state fields. `ShouldExecute` checks whether the height has arrived. [`PreBlocker`][cosmos-preblock] writes upgrade information and stops if a due plan has no registered handler. It also rejects an early handler and checks the last completed handler on startup.

[`Keeper.ScheduleUpgrade`][cosmos-keeper] rejects reuse of a completed name. `ApplyUpgrade` calls the name-indexed handler with the module version map, stores the returned versions, increments the protocol version, clears the plan and related IBC state, and records completion. A handler error returns before completion marking. This is a concrete model for stopping at an unsupported upgrade rather than treating it as absent. A plan name does not bind Via's approved calldata, and Cosmos's explicit skip-height option supplies no authority to skip a Via approval.

Optimism at `5662448279e4fb16e073e00baeb6e458b12a59b2` retains old decoder behavior through [`L1BlockInfoFromBytes`][op-info]. It selects Interop, Ecotone, or Bedrock by configuration and L2 block time, not by trying decoders until one succeeds. `isEcotoneButNotFirstBlock` excludes the activation block itself, which still uses the old L1-info format. The Bedrock ordered fields are selector, number, time, base fee, block hash, sequence number, batcher address, overhead, and scalar. Ecotone uses selector, base-fee scalar, blob-base-fee scalar, sequence number, time, number, base fee, blob base fee, block hash, and batcher address. Both decoders check exact length and selector.

[`PreparePayloadAttributes`][op-attributes] appends upgrade transactions after the L1-info transaction and derived deposits. It returns temporary errors for retrieval failures, reset errors for conflicting L1 origins, and critical errors for required derivation or upgrade-construction failures. The transferable distinction is between retryable retrieval, chain-context reset, and deterministic inability to derive required state. Its configured fork time and ordered activation transactions are not evidence for a Via Bitcoin activation height.

The local newer zkSync processor supplies another useful limit. Missing upgrade preimages and missing scheduling or cut data are errors. The old-version prerequisite must match the database before application. Its old-version-keyed cut lookup and new-version fallback begin at the scheduling block. These compatibility rules reflect actual contract generations and replacement authority. They do not justify guessing a Via decoder from tail length or allowing current source to replace historical approval meaning.

## Recommended scope and future proof

The smallest coherent change keeps the current producer, shared parser, `ViaProtocolUpgrade`, both governance processors, authorization lookup, and existing DALs as owners. Existing candidate source must be recovered before replacement work. Checked field decoding and complete-pair handling may be reusable; unrelated carrier or withdrawal behavior must not enter the upgrade change with those helpers. Preserve the producer's ordered fields unless evidence and an explicit decision require a new format.

The remaining alternatives are a single decoder proven equivalent across supported history, or narrowly bounded historical decoding where actual evidence requires distinct results. Neither is selected here. A historical boundary needs an authoritative network and chain context or an explicit approved manifest. Software startup time, fetch order, and an arbitrary payload-version threshold are not substitutes. A mismatch should preserve raw and derived records and stop affected activation or replay. Correcting an already executed result requires a separate governance disposition.

Future local proof should cover producer-built empty, one-pair, multi-pair, unmatched, malformed-width, truncated, missing-terminal, and extra-terminal cases. It must distinguish authorization-parent unavailability from proposal unavailability and deterministic malformed content through both consumers. It should compare exact calldata and canonical hashes, same-version replay and conflict writes, pending-batch restart, approval removal and re-inclusion, and blocks before, at, and after the selected boundary. These scenarios are proposed proof obligations, not executed tests or a claim that a cross-database rollback already exists.

History scope, archive ownership, artifact-to-role mapping, sufficiency thresholds, discrepancy authority, and activation remain open human or operational decisions. A source-level fix alone supplies none of the missing historical records. Shared parser changes may need coordination with deposit work, but deposit census completion is not a governance prerequisite.

No implementation, build, test, validation command, migration, or deployment was performed for this note.

[flow]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/docs/via_guides/gov/protocol-upgrade/1.upgrade-flow.md
[execution-guide]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/docs/via_guides/gov/protocol-upgrade/3.upgrade-execution.md
[types]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/types.rs
[producer]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/inscriber/script_builder.rs#L308-L336
[parser]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/indexer/parser.rs#L167-L278
[proposal-parser]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/indexer/parser.rs#L628-L677
[approval]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/indexer/parser.rs#L943-L995
[authorization]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/indexer/mod.rs#L334-L391
[main]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/via_btc_watch/src/message_processors/governance_upgrade.rs
[verifier]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_btc_watch/src/message_processors/governance_upgrade.rs
[conversion]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/types/src/via_protocol_upgrade.rs
[keeper]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/via_state_keeper/src/keeper.rs#L264-L308
[watcher]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/via_btc_watch/src/lib.rs#L175-L210
[era-upgrades]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_watch/src/event_processors/decentralized_upgrades.rs
[era-tests]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_watch/src/tests/mod.rs#L269-L383
[op-upgrade]: https://github.com/ethereum-optimism/optimism/blob/5662448279e4fb16e073e00baeb6e458b12a59b2/op-node/rollup/derive/ecotone_upgrade_transactions.go
[timelock]: https://github.com/OpenZeppelin/openzeppelin-contracts/blob/dbb6104ce834628e473d2173bbc9d47f81a9eec3/contracts/governance/TimelockController.sol
[producer-envelope]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/inscriber/script_builder.rs#L91-L149
[verifier-dal]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/verifier_dal/src/via_protocol_versions_dal.rs#L233-L272
[verifier-execution]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_zk_verifier/src/lib.rs#L137-L303
[main-dal]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/dal/src/protocol_versions_dal.rs#L98-L121
[cosmos-plan]: https://github.com/cosmos/cosmos-sdk/blob/eb1a8e88a4ddf77bc2fe235fc07c57016b7386f0/x/upgrade/types/plan.go
[cosmos-preblock]: https://github.com/cosmos/cosmos-sdk/blob/eb1a8e88a4ddf77bc2fe235fc07c57016b7386f0/x/upgrade/abci.go
[cosmos-keeper]: https://github.com/cosmos/cosmos-sdk/blob/eb1a8e88a4ddf77bc2fe235fc07c57016b7386f0/x/upgrade/keeper/keeper.go
[op-info]: https://github.com/ethereum-optimism/optimism/blob/5662448279e4fb16e073e00baeb6e458b12a59b2/op-node/rollup/derive/l1_block_info.go
[op-attributes]: https://github.com/ethereum-optimism/optimism/blob/5662448279e4fb16e073e00baeb6e458b12a59b2/op-node/rollup/derive/attributes.go
