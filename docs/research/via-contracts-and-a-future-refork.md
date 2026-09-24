# Via contracts and a future re-fork

Status: research. No re-fork, implementation, or new architecture decision is approved by this note.
Evidence date: 2026-09-23 UTC.

## Why research the behavior before choosing the code

A future re-fork needs a record of what Via must continue to do, not just a list of patches to replay.
The important record includes accepted bytes, authorization rules, stored meanings, restart behavior,
and evidence that old transactions retain their meaning.

The nine studies linked below cover the current design questions. They are not an inventory of every
Via feature. They do not establish that cherry-picking is impossible or estimate the cost of a re-fork.
That assessment needs a separate comparison of the selected revisions and the full feature set.

For now, the useful distinction is between a Via contract and the code that connects it to zkSync.
A contract states observable behavior. An adapter supplies the database, node lifecycle, VM, or network
interfaces needed by a particular zkSync revision. Another project's implementation is evidence for an
option, not evidence that Via has the same trust assumptions.

[Proposed ADR 0005](../adr/0005-preserve-documentation-ownership-and-provenance.md) applies that distinction
to documentation ownership and upstream provenance. The proposal does not approve a re-fork or a new
documentation publication system.

## The source revisions define the comparison

The comparison uses Via commit `8a49f355bfe31720f21b8181db471243709194e5` and the local newer zkSync
checkout at `ff5f519b11cff863edcfa0f75af10fea113806b0`. The latter is not a verified claim about the latest
remote branch. Topic notes pin external sources separately and identify moving references where a pin
is unavailable.

The repository's [architecture guide](../via_guides/architecture.md) describes Via's Bitcoin settlement,
Celestia data availability, and separate verifier. Those responsibilities differ from zkSync's Ethereum
settlement. A matching Rust type name does not remove that difference.

## A concrete integration interface already exists

Via's [`WiringLayer`][via-wiring] and the newer zkSync [`WiringLayer`][upstream-wiring] have identical file
contents at the inspected checkouts. The file moved from `core/node/node_framework/src/wiring_layer.rs`
to `core/lib/node_framework/src/wiring_layer.rs`.

Both define `Input: FromContext`, `Output: IntoContext`, `layer_name`, and `wire`. The interface adds
resources and tasks when the node starts. This is a real extension interface to investigate, not a
reason to introduce a new generic plugin system.

Via already uses that interface in [`ViaBtcProofVerificationLayer`][via-verifier-layer]. Its
`ProofVerificationInput` carries `master_pool`, `da_client`, `btc_client_resource`,
`btc_indexer_resource`, and `object_store`. Its `wire` method constructs `ViaVerifier`, and
`ProofVerificationOutput` registers that verifier as a task.

The lifecycle contract matters too. In the newer zkSync [`Task`][upstream-task] interface, the default
`TaskKind::Task` stops the service when the task returns. `OneshotTask` can finish without that effect.
`UnconstrainedTask` runs without waiting for preconditions. Moving a background observer into a normal
task therefore requires a deliberate failure policy. The existence of `WiringLayer` does not establish
that an observer is isolated from signing or proof verification.

The newer [`node_builder.rs`][upstream-builder] composes layers from their owning crates, including
`zksync_da_clients::node`, `zksync_da_dispatcher::node`, and `zksync_dal::node`. Via's inspected verifier
adapter lives in the central node-framework implementation directory. The newer layout is a useful
comparison for ownership, but this note does not prescribe a repository-wide move.

File equality is narrow evidence. It does not prove that configuration, database types, task resources,
VM behavior, contracts, or protocol versions are compatible across the two repositories.

## The nine studies separate contracts from choices

The following studies cover the known questions:

- [Withdrawal authorization](withdrawal-authorization-and-refork.md) separates expected obligations,
  observed Bitcoin payments, and the exact transaction a signer authorizes. The earlier
  [storage study](withdrawal-intent-and-observation.md) supplies the detailed schema comparisons.
- [Proof verification](proof-verification-and-history.md) examines verification inputs, result handling,
  and the historical evidence needed before an implementation can be trusted with old data.
- [Coordinator authentication](coordinator-request-authentication.md) examines request identity,
  signed content, retries, and conflicts within a signing session.
- [Deposit compatibility](deposit-compatibility-evidence.md) separates accepted decoding rules from
  the evidence needed to preserve historical interpretation.
- [Conflicting deposit messages](conflicting-deposit-messages.md) examines how one accepted policy
  reaches every parser consumer without changing meaning between components.
- [Historical upgrade reconstruction](historical-upgrade-reconstruction.md) examines proposal bytes,
  version selection, and evidence of what was approved and executed.
- [Inscription monitoring](inscription-monitoring-design.md) distinguishes observations from runtime
  authority and separates unavailable data from a measured zero.
- [Synthetic bridge checks](synthetic-bridge-checks.md) examines end-to-end evidence, probe state,
  duplicate prevention, and limits on funded activity.
- [Independent monitoring](independent-monitoring-failure-detection.md) examines detection of a failed
  monitoring host without relying on that host to report its own failure.

## The questions share evidence, not one implementation

The design recommendation is to keep obligations, observations, authorization, and operator evidence
separate in the research. Combining them under one status such as "processed" hides which fact a
reader can rely on. That recommendation does not select a new Via schema.

Several questions meet at concrete behaviors:

- A valid request signature identifies authorized request content. It does not by itself prove that a
  withdrawal is owed or that the proposed Bitcoin transaction conserves value.
- A historically valid proof needs the right verification inputs and protocol version. A stored result
  is a separate claim whose origin and meaning need evidence.
- Deposit and upgrade compatibility both require historical bytes and version context. One deposit
  census cannot establish what an upgrade proposal authorized.
- An inscription observer can report stalled progress. An end-to-end probe can exercise the bridge.
  An independent heartbeat receiver can detect missing monitor reports. None establishes the other
  two properties by itself.

These are dependencies to check when comparing designs. They are not instructions to combine the
implementations or block every topic on the completion of every other topic.

## The comparisons rule out several shortcuts

The studies identify useful mechanisms without treating another chain as a complete Via design:

- sBTC derives signing digests from independently loaded request reports.
  tBTC retains requested amounts and fee limits before checking a later payment.
  These mechanisms support a distinction between obligations and payment evidence, not a universal
  bridge schema. See [withdrawal authorization](withdrawal-authorization-and-refork.md).
- The newer zkSync read-only proof handler checks supplied bytes against a stored proof object.
  Its proof manager instead performs cryptographic verification and checks the batch statement.
  RISC Zero and SP1 supply further examples of explicit program, version, and public-input binding.
  A method named `verify_proof` is not enough to identify its guarantee.
  See [proof verification](proof-verification-and-history.md).
- Botocore binds HTTP operation fields, Matrix propagates the authenticated principal, and CometBFT
  retains a signing result for an identical retry while rejecting conflicting sign bytes.
  No single example supplies the whole Via request and MuSig2 round contract.
  See [coordinator authentication](coordinator-request-authentication.md).
- Ord distinguishes absent protocol data from recognized invalid data.
  That supports explicit carrier classification, not copying Runes' push-concatenation rules into Via.
  See [conflicting deposit messages](conflicting-deposit-messages.md).
- Optimism constructs fixed serialized upgrade transactions, while OpenZeppelin binds scheduled
  operations to exact targets, values, and payloads.
  Both make execution identity explicit. Neither reconstructs the meaning of an old Via proposal.
  See [historical upgrades](historical-upgrade-reconstruction.md).
- Matter Labs' `era-watchdog` provides periodic deposit and withdrawal flows, but its inspected
  withdrawal-finalization flow simulates rather than sends finalization.
  Optimism's withdrawal test supplies a stronger fee-adjusted value assertion.
  Neither can be adopted as a Bitcoin payout check without adapting the assertion.
  See [synthetic bridge checks](synthetic-bridge-checks.md).
- Healthchecks separates heartbeat receipt from an expiry worker.
  Uptime Kuma preserves the age of the last push rather than inventing a successful heartbeat on each
  receiver iteration. Their inspected HTTP methods differ, so transport compatibility needs checking.
  See [independent monitoring](independent-monitoring-failure-detection.md).

## Evidence that remains useful after the code moves

The useful long-term record is a set of contracts with source-backed examples. For each topic, the
research distinguishes the following material:

- Exact serialized bytes, identifiers, amounts, and hash inputs whose meaning must survive.
- Accepted and rejected examples, including malformed input, repeats, and conflicting messages.
- State transitions after restart, retry, reorg, or out-of-order observation.
- The source that owns the behavior and every consumer that depends on it.
- Dependencies on a VM version, verification key, contract version, database layout, or node interface.
- Evidence still missing and choices that require a human decision.

Variable and function names help the next reader find the implementation. Those names are not the
contract. Copying a familiar name such as `request_id` without its chain, fork, or output identity would
lose the reason the original implementation used that field.

## Research does not make a decision

Research belongs here. An ADR records an accepted consequential choice and links to the evidence
rather than copying it. Existing ADRs retain their authority unless an explicit later decision replaces
them. Provider agreement is useful feedback, but inspected code and applicable standards support the
claims.

Code inspection does not establish deployment state. Historical database access, funded probes,
production fault injection, and rollout remain separate work requiring authorization. Missing evidence
stays missing rather than becoming a default assumption that the system is safe or unsafe.

[via-wiring]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/node_framework/src/wiring_layer.rs
[upstream-wiring]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/node_framework/src/wiring_layer.rs
[via-verifier-layer]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/node_framework/src/implementations/layers/via_zk_verification.rs
[upstream-task]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/node_framework/src/task/mod.rs
[upstream-builder]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/bin/zksync_server/src/node_builder.rs
