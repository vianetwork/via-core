---
status: accepted
---

# Bind proofs to inscribed batches by chaining verified commitments

A verified proof establishes a transition between two batch commitments. At source revision
`269b81cf056b2bb13294a042b93720b4f5500efe`, the verifier takes both commitments from the proof package
and no independent comparison to the intended inscribed batch was found at that boundary. This is a
source-level binding gap, not a demonstrated proof-substitution execution.
[ADR 0007](0007-record-only-completed-proof-verdicts.md) requires binding before historical trust claims
or withdrawal activation.

## Decision

The verifier owns the statement it verifies. For batch N it uses the commitment it accepted for batch N−1
as the previous commitment, and it rebuilds the current commitment around the state root inscribed for N.

A commitment is `keccak(P || M || A)`, where `P`, `M` and `A` are the pass-through, metadata and auxiliary
hashes. The pass-through preimage contains the leaf index and state root plus the zeroed second shard.
An opening supplies the witnesses needed to show that the inscribed root is a component of the commitment.
The verifier inserts that root, uses the package's leaf index, metadata and auxiliary hash as witnesses,
and verifies against `keccak(C[N−1] || C[N]) >> 32`. Under the hash and proof-system assumptions and
correct circuit/key correspondence, this binds the checked transition to that root and retained parent
commitment. It does not by itself establish batch number, canonicality or consumed message contents.
The accepted commitment is persisted with the verdict and becomes the next parent.

The metadata hash covers the bootloader, default-account and EVM-emulator code hashes the batch executed with.
A correct verification key does not authorize that code. The verifier must check these hashes against the code
authorized for the batch's protocol version, not take them from the package. Blind equality with the previous
batch would wrongly forbid upgrades. zkSync's settlement contract likewise reads them from authorized storage
([`Executor.sol` L601-611](https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L601-L611)),
while the scheduler circuit takes them as witnesses
([`scheduler/mod.rs` L148-183](https://github.com/matter-labs/zksync-protocol/blob/33d3aa0a1dd9a73175a105b09a3fd54ef3191921/crates/zkevm_circuits/src/scheduler/mod.rs#L148-L183)).

Pubdata consumed for deposits, upgrades and withdrawals must be opened against the same commitment.
A root match alone does not authenticate those messages.

The Bitcoin inscription format is unchanged.

## Considered options

- *Recompute the entire commitment from published data.* Rejected as the primary mechanism: Via's published
  pubdata lacks several commitment inputs, including system logs, uncompressed state diffs and the event-queue
  and bootloader-heap preimages. Supplying them requires additional witnesses and state.
- *Publish the commitment on Bitcoin beside the root.* Deferred. It names one producer-declared commitment
  for the batch in advance instead of attributing it through root opening and sequence alone. Publication
  does not itself prove the root or consumed messages belong to that commitment. It changes the protocol,
  cannot cover past inscriptions, and still requires the checks above. It may be added later for new batches.
- *Check the package's commitment against its own fields.* Rejected: this confirms the sequencer's serialization,
  not the inscribed batch.

## Precedents

These consumers construct or compare the expected statement against independently selected state or data
before accepting a proof. Their mechanisms informed this decision; their trust models are not Via rules.

- [Alpen ASM](https://github.com/alpenlabs/asm/blob/afaf935727c34acf07967420fe3c4584428f5550/crates/subprotocols/checkpoint/verification/src/verification.rs#L48-L215)
  builds the claim from its last verified tip and posted payload, then checks the active predicate. With
  its real proof predicate, rather than an accepting development predicate, this is the closest model.
- [Citrea](https://github.com/chainwayxyz/citrea/blob/f11527f94344d5dc4576ccb9589d5713fb8f7238/crates/fullnode/src/da_block_handler.rs#L688-L849)
  compares the proof's commitment identities with those seen on its data-availability layer before marking batches proven.
- [Mina](https://github.com/o1-labs/mina-rust/blob/82480cd468f1963b73dc0b700161036411449e4c/crates/ledger/src/proofs/verification.rs#L430-L455)
  replaces the proof-carried state with the expected state when building public inputs.
- [zkSync settlement](https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L472-L540)
  derives public inputs only from batch records stored at commit time.

The [proof-statement binding research](../research/proof-statement-binding.md) records all inspected projects and
their engineering detail.

## Consequences

The following must be demonstrated offline before implementation or historical use:

- the rebuilt commitment matches the v27 and v28 circuits, verification keys and commitment modes;
- batch identity `(N, root, C, sources)` and sequential attribution hold without an explicit batch-number
  field in the commitment; this is an evidence contract, not a selected database schema;
- the previous-root system log has the claimed meaning in Via's supported circuits and can be opened
  against the retained parent root, or another demonstrated check establishes that relation;
- every consumed pubdata item opens against the commitment;
- restart, reorg and inscription-chain replacement rewind the retained commitment with the verdict.
- verdict, commitment and effects persist atomically with the intended proof and batch source identity;
- the metadata witness's code hashes match the authorized code for each batch's protocol version;
- a retained commitment is keyed by batch identity, because `via_votable_transactions.l1_batch_hash` is unique
  and two batches sharing a state root could not both be recorded;
- development approvals (`unverified_dev`) and rows from before the store designation are never eligible parents.

A failed opening yields no verdict under ADR 0007 and must not reuse the descendant invalidation cascade.
A mismatched package is not conclusive evidence that the batch itself is invalid. Whether a bound proof
that fails can reject the batch is decided when this binding is implemented.

The chain needs a trusted first commitment: a verified genesis or an explicitly approved checkpoint.
That choice, the historical range and the activation order remain separate decisions.

The decision records accepted design direction, not implementation or deployment.
