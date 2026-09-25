# Proof verification and historical evidence

Status: research. Verdict and isolated development policies were accepted on 2026-09-25 in [ADR 0007](../adr/0007-record-only-completed-proof-verdicts.md); [ADR 0008](../adr/0008-bind-proofs-by-chaining-verified-commitments.md) subsequently accepted commitment chaining with root openings. Binding proof obligations, failed-opening policy, the trusted anchor, historical trust and rollout remain pending. Earlier alternatives below are historical where these ADRs supersede them. Evidence inspected on 2026-09-23 and 2026-09-24. This note is not a release approval or an operational collection procedure; restricted candidate details remain outside it.

The question is what a successful proof result establishes, and what evidence makes previously accepted or still-pending history trustworthy. A change to the verifier cannot retroactively certify a stored status, a published vote, or a dependent withdrawal.

## The statement and the status are different objects

At Via `8a49f355bfe31720f21b8181db471243709194e5`, `ProveBatchData` dispatches between `V27` and `V28`. `decode_prove_batch_data` first attempts V27 for protocol versions through 27, then attempts V28. Its comment explicitly warns that serialization can change even within a version. The versioned `ProveBatches` data carries previous-batch metadata, batches, proofs, and `should_verify`. A producer-controlled field is part of a wire format; it is not an independent attestation of what a verifier did. [Version dispatch][via-dispatch] · [Versioned verification owner][via-v28].

`ViaDataAvailabilityDispatcher` selects real or dummy proof publication using `dispatch_real_proof`. `serialize_prove_batches` preserves separate serialized components, including the trailing flag. This makes the producer, serialized blob, and consumer a joint compatibility boundary. A later policy change must not silently change historical blob interpretation. Source configuration does not prove a deployed process's effective mode. [Producer and serialization][via-producer].

The consumer's `ViaVerifier::loop_iteration` starts with a canonical-inscription-chain candidate, parses the proof reference and linked batch reference, retrieves DA blobs, decodes pubdata, and obtains the relevant protocol version. When a proof needs retrieval from the object store, the key includes batch number and semantic protocol version. The versioned verifier derives public inputs from previous and current commitments and uses a versioned verification-key loader. The caller then uses the combined batch verdict to update durable state. [Consumer and object retrieval][via-consumer] · [Cryptographic owner][via-v28].

The relevant durable identities are more specific than a batch number. `via_votable_transactions` records `l1_batch_hash`, `prev_l1_batch_hash`, `proof_reveal_tx_id`, `proof_blob_id`, `pubdata_reveal_tx_id`, and `pubdata_blob_id`. Its insertion conflict key is `l1_batch_hash`. `via_votes` deduplicates by `(votable_transaction_id, verifier_address)`. `verify_votable_transaction` updates by batch number and proof reveal txid, while a negative result also marks later batches invalid. `finalize_transaction_if_needed` depends on local verdict state and vote counts. [Vote DAL][via-votes]. These are important state transitions, but none of those keys records the full evidence needed to independently reproduce a historical cryptographic check.

The [Verifier Network guide][via-guide] describes verification, Bitcoin attestation, finalization, and withdrawal processing as separate stages. Main-node publication and verifier consumption have different owners. The sibling map also pairs the main and verifier DA clients, Bitcoin watchers, senders, and reorg detectors. A future change must trace through those owners rather than stopping at a library return value. [Sibling map][siblings].

## A verification result needs an explicit meaning

The proposed contract distinguishes four outcomes without requiring a new public enum or schema:

- Verified valid means the expected proof system checked a proof against the intended batch statement and verification key, and the required non-proof checks also passed.
- Verified invalid means a completed check established an invalid statement or proof under that policy.
- Skipped means the cryptographic check did not occur. It must not be represented as verified valid.
- Unavailable or malformed means the required evidence could not be obtained or decoded safely. That is not automatically a cryptographic negative vote.

The distinction is consequential because a negative verdict can invalidate later batches. A transport failure, missing proof, unsupported format, or explicit skip needs a deliberate transition policy rather than a convenient boolean conversion. The same rule applies when a priority-operation check fails: a combined result must not imply that a proof check happened if control flow did not perform it.

A local `Result<bool>` can express part of this contract if `Ok(true)` requires completed verification
and `Ok(false)` means a completed negative verdict. Skip and unavailable outcomes must remain
distinguishable from both verdicts through every caller. Collapsing an error into `false` is unsafe
when that value triggers a negative vote or invalidates later batches.

A richer result can make accidental conversion harder, but that is an implementation choice.
A changed type also does not prove correct persistence or vote serialization.
The implementation comparison must cover both proof versions, caller error handling, stored verdicts,
and outgoing votes. Candidate provenance and applicability require separate evidence.

## Newer local zkSync has two different meanings of verify

The inspected newer local zkSync is `ff5f519b11cff863edcfa0f75af10fea113806b0`, not verified latest upstream.

`Processor<Readonly>::verify_proof` in `core/node/proof_data_handler/src/processor.rs` loads `L1BatchProofForL1` by `L1BatchProofForL1Key::Core((batch_number, protocol_version))`. It checks the stored proof's version and compares its serialization with the supplied bytes. That is an equality check against a stored expected object, not an independent cryptographic verification. Naming alone would lead to the wrong re-fork conclusion. [Newer read-only proof handler][zk-equality].

The cryptographic comparison is in `ProofRequestProvenHandler` and its private `verify_proof` function. It deserializes a typed proof, accepts the Fflonk branch for this proving-network path, rejects Plonk and Airbender there, and runs `fflonk::verify` with the configured verification key. It catches structurally malformed-proof panics and treats them as an error. It then compares protocol version and auxiliary outputs with locally stored batch metadata: events queue, bootloader initial content, system logs, and state-diff hash. The state-diff source changes at the Gateway protocol boundary. [Newer proof manager][zk-proof].

On success, the handler stores the proof under the batch/version key and saves its artifact metadata before marking the proving-network batch proven. On error, it records a failed verification result. That separation is directly useful for Via. The handler's Ethereum event origin, proving-network rewards, Fflonk-only scope, and Gateway metadata rules are not a replacement for Via's Bitcoin references or historical V27/V28 formats.

This newer handler also shows why error policy cannot be copied without its consumer. Any `Err` from its private `verify_proof`, including a database error or absent batch metadata, becomes `verification_result = false` before `mark_batch_as_proven`. An object-store write failure on the success path instead propagates with `?` before that status update. That is a proving-network submission policy, not a safe default for a consumer whose negative result invalidates descendant batches. Its panic containment is useful separately from its error-to-negative conversion.

`AirbenderVerifierInput::verify` is a third, non-equivalent mechanism. It executes the VM and checks accessed storage paths against an old root before returning a resulting root. It does not establish that an old Via scheduler proof was accepted under the same statement. [Newer execution verifier][zk-airbender]. A re-fork must name which guarantee it imports rather than treating all three methods as interchangeable.

## Comparable proof systems

### RISC Zero binds a proof to a program and public journal

At release v2.3.2, commit `218e3bc4a8ffcd203a9cd4e46f921bf60aa7e2bd`, `Receipt::verify_with_context` checks integrity and compares the verified claim with `ReceiptClaim::ok(image_id, journal.digest())`. `verify_integrity_with_context` has a deliberately weaker contract: it does not by itself require the expected program image or successful guest exit. `ReceiptMetadata` explicitly warns that its information is not cryptographically bound and must not drive security decisions. [RISC Zero receipt implementation][risc0].

`InnerReceipt::Fake` and `FakeReceipt::verify_integrity_with_context` are explicitly development-only. Fake receipt acceptance depends on the verifier context's dev-mode policy, with a build feature that disables dev mode. The transfer is a clear boundary between test evidence and production attestation. This is not a proposal to add a Via production bypass flag, and the source does not certify any deployed RISC Zero application's policy.

The file's `mangled_version_info_should_error` test checks a metadata mismatch. It is a relevant negative example, not proof that Via's historic versions are compatible. No external or local tests were run for this note.

### SP1 binds version, verification key, and public values

At release v5.2.2, commit `bb91c6f64f8b0b8acd59558541fce8336e967881`, `Prover::verify` calls `verify_proof` with `SP1ProofWithPublicValues`, an expected `SP1VerifyingKey`, and the current circuit version. It rejects `sp1_version` mismatch. Core and compressed proof branches compare the committed public-value digest with the bundle's public values before invoking the corresponding proof verifier. Plonk and Groth16 branches pass the verification key and public values to their specialized verifiers. [SP1 verifier][sp1].

`SP1VerificationError` distinguishes `InvalidPublicValues`, `VersionMismatch`, and proof-system-specific failures. The useful lesson is that a proof is accepted for a statement and version, not merely because a proof object parses. SP1's proof variants, hash compatibility rules, and circuit artifacts differ from Via's scheduler proof and commitments. The inspected function alone is not a full malformed-input or historical-migration audit.

The concrete prover matters too. At the same pin, `CpuProver::mock()` sets `mock: true`. Its `Prover::verify` override calls `mock_verify` rather than the normal verifier. The mock checks public-input consistency for Plonk and Groth16 and returns `Ok(())` for the other variants. Thus even a typed `Result<(), SP1VerificationError>` does not establish cryptographic verification without the process-mode identity. The normal Core branch also uses `proof.last().unwrap()`, so the error enum is not proof that arbitrary malformed inputs cannot panic. [SP1 CPU and mock implementation][sp1-cpu].

These two projects are genuine comparisons for statement binding and development policy. They are not precedents for Via's Bitcoin vote threshold, reorg handling, or SQL recovery. No universal proof-result migration standard was established by the inspected sources.

### Ethereum distinguishes missing validation data from invalid payloads

The Engine API Paris specification at `5bcdc34a477b10af278c079525374e6a4046f291` defines `PayloadStatusV1` with `status`, `latestValidHash`, and `validationError`. `engine_newPayloadV1` returns `SYNCING` when requisite data is missing. It can return `ACCEPTED` for a noncanonical payload whose basic checks pass but whose execution has not been fully validated. Neither means `VALID`. Completed invalidity identifies the latest valid ancestor when known. Unrelated processing errors use a JSON-RPC error object. [Engine API specification][engine-paris].

This is a direct comparison for P1 and P3. A consumer must preserve the difference between missing evidence and a completed negative result. It also shows a limit to a blanket rule about malformed input: zero-length transactions and an incorrect block hash are specified invalidity conditions in this protocol. Via must decide whether a malformed proof package means an invalid batch statement or merely no usable proof. The proposed conservative policy leaves the batch without a verdict unless the verification contract establishes a negative result. A local decode failure or unsupported verifier is not evidence that the referenced state transition is false.

The Engine API's branch identity is useful for later-batch handling. A descendant blocked by an invalid parent has not necessarily had its own proof checked. Historical evidence must retain that distinction even if existing storage uses one negative status for both. This comparison does not import Ethereum fork choice, optimistic execution, or a new public result enum into Via.

### Bitcoin Core separates result reasons and incomplete re-checks

At Bitcoin Core v27.0, `d82283950f5ff3b2116e705f931c6e89e5fdd0be`,
[`ValidationState`](https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/consensus/validation.h)
stores `M_VALID`, `M_INVALID`, or `M_ERROR` separately from a typed reason. A runtime error remains
an error when `Invalid()` is subsequently called. Its default `M_VALID` is accumulator state,
not an attestation that cryptographic verification ran. Missing inputs also have a validation reason;
the spelling `INVALID` alone cannot determine Via's vote policy. The useful pattern is one explicit
mapping from outcome and reason to consumer consequences.

[`VerifyDBResult`](https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/validation.h)
distinguishes `SUCCESS`, `CORRUPTED_BLOCK_DB`, `INTERRUPTED`, `SKIPPED_L3_CHECKS`, and
`SKIPPED_MISSING_BLOCKS`. This supports reporting covered results separately from interrupted or
incomplete historical checks. It does not justify treating a skipped Via proof as cleared. A private
reason type is useful if the selected retry or evidence consumer needs structured classification;
the comparison does not require a public enum, new retry framework, or new database schema.

### Optimism rechecks claim eligibility before withdrawal finalization

At Optimism `b1e7c63bb2ffea46771c302bcb05f72ba1a7bf61`, `AnchorStateRegistry.isGameClaimValid` requires a proper game, a respected game type, completed finality delay, and `GameStatus.DEFENDER_WINS`. `isGameProper` alone is insufficient. It checks factory and registry identity, blacklist, retirement, and pause state. `isGameRespected` reads `wasRespectedGameTypeWhenCreated`, rather than treating the current configured game type as historical evidence. [Anchor registry][op-anchor].

`OptimismPortal2` stores `ProvenWithdrawal { disputeGameProxy, timestamp }` under withdrawal hash and proof submitter. `checkWithdrawal` distinguishes an unproven withdrawal from one whose root claim is currently ineligible. It checks maturity and calls `isGameClaimValid` again. Re-proving replaces the submitter's proof reference and resets its timer. Finalization consumes a separate `finalizedWithdrawals` replay marker. [Withdrawal consumer][op-portal].

The transferable P3/P4 rule is to retain the proof's identity and acceptance context separately from downstream completion. A prior accepted reference does not remove the need for a recovery decision when its trust basis changes. Optimism's Guardian-controlled blacklist and retirement timestamp are explicit authorities; they are not authority to rewrite Via's SQL rows or retract Bitcoin attestations. Nor does this comparison reopen Via's accepted withdrawal guarantee as canonical-at-signing verification.

## Result representation and development policy choices

For P1, retain `Result<bool>` if every caller interprets `Ok(true)` as completed cryptographic success, `Ok(false)` as completed cryptographic failure, and `Err` as no cryptographic verdict. Keep the combined batch-policy verdict distinct in naming and evidence. A false priority-operation result does not itself say whether cryptographic verification ran. An unavailable indexing prerequisite also needs a different policy from a proven priority-order mismatch.

A private typed error can distinguish `Skipped`, `Unavailable`, `Malformed`, and `Unsupported` when retry policy or diagnostics need those reasons. This need not change historical wire structs or introduce a new SQL enum. A wider outcome enum helps only if consumers exhaustively preserve it. Converting every non-success variant to `false` recreates the invalidation problem; keeping distinctions only in logs loses them after restart.

Completing cryptographic verification and then combining it with an incomplete priority-data boolean
still produces the wrong durable meaning. The caller must establish prerequisite completeness before
using a negative batch-policy result. A shortage of locally indexed deposits leaves the check pending;
a conclusive priority-order mismatch is a separate policy case. A library correction alone cannot
establish those consumer transitions.

The user selected the current finalization rule unchanged in ADR 0007. Requiring a positive local
result before network-threshold finalization is not part of this remediation.

For P2, the user selected proof-free end-to-end development through the verifier's own configuration,
restricted to regtest/development networks and stores. Producer `SkipEveryProof` remains usable locally
but cannot enable verifier development mode. Accepted development results vote normally locally and
remain marked `unverified-dev` across restart; they must not become production verification evidence.
Production has no bypass. RISC Zero's verifier-owned development context and SP1's mock-verifier
distinction inform this contract without supplying Via's exact configuration or persistence design.

The accepted policies do not choose a framework, storage table or new wire flag. Their implementation must preserve the development/result distinction through consumers and restart. The existing proof clones and verification work remain separate performance concerns; a type change alone does not remove an allocation, key-file read or object-store request.

## Implementation references

The ADR 0007 implementation takes each mechanism below from a pinned source, or rejects it.
Until proofs are bound to the inscribed batch, the verifier records no rejections: it approves a batch or stops at it.

| Mechanism | Via owner | Pinned source | Taken | Why |
| --- | --- | --- | --- | --- |
| Skip flag chooses only what the producer submits | `verify_proof` (v27, v28) | [zkSync `prove_batches.rs` L46-136](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/l1_contract_interface/src/i_executor/methods/prove_batches.rs#L46-L136) | Adopted | Serving nodes rewrite the flag on the external-node DA path, so the verifier never reads it. |
| Skip only an absent proof, verify any present one | `verify_batch_proof` | [zkSync `TestnetVerifier.sol` L14-33](https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/TestnetVerifier.sol#L14-L33) | Adapted | Via's absence is a local observation, so development mode is fenced to regtest. A proof that arrives later is not re-checked. |
| Development switch owned by the verifier | `proof_verification_dev_mode` | [RISC Zero `receipt.rs` L784-792](https://github.com/risc0/risc0/blob/218e3bc4a8ffcd203a9cd4e46f921bf60aa7e2bd/risc0/zkvm/src/receipt.rs#L784-L792) | Adopted | The package cannot choose its own verification mode. |
| Unproven results stay a distinct kind | `unverified_dev`, `mark_unverified_dev` | [RISC Zero `receipt.rs` L438-448](https://github.com/risc0/risc0/blob/218e3bc4a8ffcd203a9cd4e46f921bf60aa7e2bd/risc0/zkvm/src/receipt.rs#L438-L448) | Adopted | A strict reader must always tell a development approval apart. The marker write re-checks the designation. |
| Mock verifier that skips cryptography | none | [SP1 `cpu/mod.rs` L194-241](https://github.com/succinctlabs/sp1/blob/bb91c6f64f8b0b8acd59558541fce8336e967881/crates/sdk/src/cpu/mod.rs#L194-L241) | Rejected | A present proof is always verified, even in development mode. |
| Designation stored in the governed database | `via_verifier_store_mode`, `ensure_proof_verification_mode` | [geth `accessors_metadata.go` L57-82](https://github.com/ethereum/go-ethereum/blob/920c07774c65ebb3023536f85df642c44478b540/core/rawdb/accessors_metadata.go#L57-L82), [`genesis.go` L384-392](https://github.com/ethereum/go-ethereum/blob/920c07774c65ebb3023536f85df642c44478b540/core/genesis.go#L384-L392) | Adapted | A copied or restored store keeps its designation. Geth compares stored and configured genesis. Via checks the node's genesis against the configured network, then stores it. |
| Refuse chain data for another network | storage init | [Bitcoin Core `chainstate.cpp` L72-74](https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/node/chainstate.cpp#L72-L74) | Adopted | An unparseable network name falls back to regtest. Regtest deployments share one genesis and are not distinguished. |
| Missing data is not a verdict | package decode | [Engine API `paris.md` L175-179](https://github.com/ethereum/execution-apis/blob/5bcdc34a477b10af278c079525374e6a4046f291/src/engine/paris.md#L175-L179) | Adapted | `SYNCING` is adopted. Its rule that some malformed payloads are `INVALID` is rejected, because a serving node can rebuild a package. |
| Lacking data versus unreadable data | lookup result mapping, `NoVerdictReason` | [Bitcoin Core `validation.cpp` L4451-4462](https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/validation.cpp#L4451-L4462), [`validation.h` L388-394](https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/validation.h#L388-L394) | Adopted | Operators must tell evidence still to arrive from a batch stuck until someone acts. |
| A failed proof says nothing about the payload | `Ok(false)` gives `proof_failed` | [Lighthouse `proof_engine/src/lib.rs` L24-30](https://github.com/sigp/lighthouse/blob/de4ef4002115b6f94a42868114cabd66f7273328/beacon_node/proof_engine/src/lib.rs#L24-L30) | Adopted | The proof's commitments come from the package unbound. |
| Invalidity only against a verified parent state | `verify_op_priority_id` | [Bitcoin Core `validation.cpp` L2330-2332](https://github.com/bitcoin/bitcoin/blob/e8e7e91a1144c378dff4da2e2a562eb0f3f2e1d6/src/validation.cpp#L2330-L2332) | Adopted | The local deposit index is not a verified state, so a mismatch gives `deposit_mismatch`. |
| Advance past a failed proof | `no_verdict` | [Citrea `da_block_handler.rs` L299-351](https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/fullnode/src/da_block_handler.rs#L299-L351) | Rejected | Via writes nothing and retries the same batch, so no proof is dropped. |
| One batch per proof | `check_shape` | [zkSync `Executor.sol` L517-521](https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L517-L521) | Adapted | The proof may be omitted from the package and fetched from the store. |
| Package identity against independent state | `verify_batch_proof` | [zkSync `Executor.sol` L484-500](https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L484-L500) | Adapted | The inscription carries no commitment, so Via compares the batch number, roots and protocol version. The commitments stay unbound. |
| A minor version's patches | `semantic_versions_of_minor` | [zkSync `protocol_versions_dal.rs` L320-347](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/dal/src/protocol_versions_dal.rs#L320-L347) | Adapted | The verification-key filter is dropped because Via keeps one key per minor. A proof made with another key fails and records nothing. |
| Proof lookup across allowed versions | `find_wrapped_proof` | [zkSync `aggregator.rs` L1030-1050](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_sender/src/aggregator.rs#L1030-L1050) | Adapted | Store errors are returned instead of panicking, and the pre-versioning key is added for patch 0. |
| Per-attempt object-store timeout | `retries.rs` | [zkSync `5c9d3347`](https://github.com/matter-labs/zksync-era/blob/5c9d3347afc23fcbb0bd4669cbc0bbb7eca3592f/core/lib/object_store/src/retries.rs) | Adopted verbatim | A hung remote request no longer blocks the verifier. The optional local mirror is not covered. |
| Stage watermarks for stalls | `last_valid_l1_batch` | [zkSync `da_dispatcher/src/metrics.rs` L25-28](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/da_dispatcher/src/metrics.rs#L25-L28) | Adopted | A clearable current-batch gauge is rejected, because every early return must clear it. |
| Descendant invalidation | none | [reth `tree/mod.rs` L3285-3337](https://github.com/paradigmxyz/reth/blob/4630cc588430fcf3b2846e27100a5d10cff9f949/crates/engine/tree/src/tree/mod.rs#L3285-L3337) | Deferred | Invalidating later batches needs conclusive evidence, which binding supplies. |

## Historical evidence is a separate rollout gate

An acceptable bounded history assessment needs a joined evidence chain, not a report that an API said finalized. The proposed evidence contract is:

1. Process identity establishes the network, deployed publisher and verifier source/image provenance, startup-effective proof mode, relevant protocol range, and the exact databases and object stores those processes use.
2. A bounded store manifest establishes read-only collection provenance, schema version, range or high-water mark, and counts for accepted, pending, invalid, and unknown records. Accepted and pending ranges are separate questions. A sampled range does not clear all history.
3. Per-batch evidence associates batch and parent hashes, proof and pubdata references, exact serialized object hashes, protocol version, verification-key identity, existing verdicts, finalization state, and queued or published votes. Missing objects remain explicit rows in the evidence manifest.
4. Re-verification results identify the exact verifier revision and genuine proof bytes used, with changed-commitment and invalid-proof controls. A valid proof for some batch does not clear a different accepted batch or a different parent commitment.
5. A recovery decision accounts for downstream deposits, upgrades, withdrawals, and already published attestations. New code cannot retract Bitcoin history by changing a database row.

A supplied SQL or object-store export can satisfy this contract only when its collection scope and process-to-store binding are known. Bounded operator-produced results can instead establish identity, catalog, high-water marks, and scoped rows without disclosing credentials. An empty collection would establish neither historical clearance nor present access failure. These are generic evidence requirements, not a statement that any export exists. No live access or collection was attempted here.

Immutable references create a liveness constraint. If a historical reference points to a dummy or unavailable object, changing verifier policy does not make a genuine proof appear. Object replacement, republishing, a trusted checkpoint, and replay are different recovery choices with different authority requirements. This note chooses none of them.

Candidate readiness and rollout readiness must remain independent. Source reconciliation, private review, and disclosure preparation can proceed while operational history is unknown. Rollout cannot be cleared by those activities. Conversely, historical uncertainty is not a reason to discard a recoverable candidate and duplicate implementation work.

## Re-fork requirements and remaining choice

The surviving Via contract is verification of the intended batch statement before a cryptographically meaningful successful attestation, with explicit treatment of skips and missing evidence. Bitcoin inscription linkage, DA provenance, previous/current commitments, and vote identity remain Via responsibilities. The current owners are `via_verification`, `ViaVerifier`, `ViaVotesDal`, the DA dispatcher, and the Bitcoin attestation sender.

The replaceable adapters are versioned proof decoding, scheduler-proof types, verification-key loading, public-input derivation, protocol metadata, and object-store serialization. Newer `TypedL1BatchProofForL1` and the proof manager are closer comparisons than the read-only proof handler, but their supported variants do not prove support for Via history. The database's semantic protocol version and the circuit's verification-key identity must agree with the relevant system-contract and VM era.

Before a re-fork can claim compatibility, it needs genuine historical V27/V28 blobs, their exact bytes and object hashes, expected key identities, successful verification results, altered-commitment negatives, unsupported-version and cardinality behavior, and persisted verdict/vote replay. Restart, reorg, unavailable-object, and already-finalized cases need separate evidence. A decoder that accepts old bytes without reproducing the intended statement is insufficient.

The remaining human choice is the accepted historical trust boundary and recovery policy, including whether pending immutable references can obtain genuine proofs. Candidate extraction and disclosure remain separate decisions. This research ran no proof verification, SQL queries, builds, tests, or deployment checks.

[via-dispatch]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/via_verification/src/lib.rs
[via-v28]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/via_verification/src/version_28/mod.rs
[via-producer]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/via_da_dispatcher/src/da_dispatcher.rs
[via-consumer]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_zk_verifier/src/lib.rs
[via-votes]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/verifier_dal/src/via_votes_dal.rs
[via-guide]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/README.md
[siblings]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/.github/sibling-paths.yml
[zk-equality]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/proof_data_handler/src/processor.rs#L380-L403
[zk-proof]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_proof_manager/src/watcher/events/proof_request_proven.rs
[zk-airbender]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/airbender_verifier/src/lib.rs
[risc0]: https://github.com/risc0/risc0/blob/218e3bc4a8ffcd203a9cd4e46f921bf60aa7e2bd/risc0/zkvm/src/receipt.rs
[sp1]: https://github.com/succinctlabs/sp1/blob/bb91c6f64f8b0b8acd59558541fce8336e967881/crates/sdk/src/prover.rs
[sp1-cpu]: https://github.com/succinctlabs/sp1/blob/bb91c6f64f8b0b8acd59558541fce8336e967881/crates/sdk/src/cpu/mod.rs
[engine-paris]: https://github.com/ethereum/execution-apis/blob/5bcdc34a477b10af278c079525374e6a4046f291/src/engine/paris.md
[op-anchor]: https://github.com/ethereum-optimism/optimism/blob/b1e7c63bb2ffea46771c302bcb05f72ba1a7bf61/packages/contracts-bedrock/src/dispute/AnchorStateRegistry.sol
[op-portal]: https://github.com/ethereum-optimism/optimism/blob/b1e7c63bb2ffea46771c302bcb05f72ba1a7bf61/packages/contracts-bedrock/src/L1/OptimismPortal2.sol
