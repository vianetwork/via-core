---
status: accepted
---

# Record only completed proof verdicts

At source revision `269b81cf056b2bb13294a042b93720b4f5500efe`, a verifier's batch result drives its vote,
deposit and upgrade effects, and withdrawal eligibility. A negative result also marks later batches
negative and places uncertain withdrawal holds. The versioned verifier permits a producer-controlled
`should_verify = false` to return approval without cryptographic verification. An incomplete local
deposit index and some malformed-package cases can instead return a negative result without a proof check.

## Decision

**The proof package cannot grant approval.** `should_verify` remains decodable wire data only.
The verifier attempts real verification whenever a genuine proof is obtainable, regardless of the flag.

**Incomplete evidence yields no verdict.** An unavailable proof object or an incomplete local deposit index
writes no new verdict, casts no new vote and causes no invalidation solely for missing evidence.
Existing holds remain. The verifier waits for sufficient evidence; retry does not guarantee recovery.

**A malformed package yields no verdict unless its invalidity is conclusively established.**
Missing evidence, permanently unusable packages and conclusively invalid batches remain distinct outcomes.
A permanently unusable package leaves the verifier visibly stopped at that batch until a separately decided recovery.

**Until proofs are bound to the inscribed batch, the verifier records no rejection.** A proof that does not verify
and a deposit-order disagreement each yield no verdict. Neither establishes that the inscribed batch is invalid:
the proof's public inputs come from the package, and the deposit comparison depends on this node's own index.
The verifier therefore approves a batch or stops at it, and an unapproved batch never reaches the finalization threshold.
Recording rejections again requires the binding selected in ADR 0008.

**Proof-free operation is a verifier-side development mode.** Local end-to-end runs without a prover
are required. The verifier's own configuration enables this mode, never the proof package.
It refuses non-regtest/non-development networks and stores. The producer's `SkipEveryProof` mode
remains available for local runs but cannot select the verifier's mode.

In this explicit mode, accepted development results participate in normal local voting and remain
marked `unverified-dev` across restart. They are development simulation results, not completed
cryptographic verdicts, and must not become production verification evidence. Production has no bypass.
The precise configuration, store fencing and durable representation remain implementation details.

**Finalization is unchanged.** Preserve the existing vote-count threshold and local-result presence rule,
including its handling of a negative local result. Withdrawal authority still requires both finalization
and a positive local result; the development exception does not authorize production withdrawal activation.

## Considered options

- *Honor the flag as a declaration that no proof exists.* Rejected: on the external-node data-availability path
  the serving node rewrites the flag from its current configuration, so verifiers can see different values for one batch.
- *Treat missing or malformed evidence as a negative verdict.* Rejected for incomplete evidence: a transport failure
  or indexing lag would invalidate later batches and hold withdrawals without any proof being judged.
- *A prover-side mock only.* Insufficient: the producer's `SkipEveryProof` mode already publishes proof-free packages.
  The verifier decides approval, so the development switch belongs to the verifier.
- *Require a positive local result for finalization.* Not selected for this remediation: it adds a liveness
  restriction without adding to the existing withdrawal predicate. This decision preserves the current rule.

Comparable mechanisms informed these choices; they are not Via rules.
The [Ethereum Engine API](https://github.com/ethereum/execution-apis/blob/5bcdc34a477b10af278c079525374e6a4046f291/src/engine/paris.md#L168-L184)
returns `SYNCING` for missing requisite data and distinguishes an incompletely validated `ACCEPTED`
payload from `VALID`; it can also classify specified malformed inputs as invalid.
[Bitcoin Core's `VerifyDBResult`](https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/validation.h)
separates skipped checks from corruption.
[RISC Zero](https://github.com/risc0/risc0/blob/218e3bc4a8ffcd203a9cd4e46f921bf60aa7e2bd/risc0/zkvm/src/receipt.rs)
accepts fake receipts only under the verifier's development context;
[SP1](https://github.com/succinctlabs/sp1/blob/bb91c6f64f8b0b8acd59558541fce8336e967881/crates/sdk/src/cpu/mod.rs)
illustrates why a successful return from a mock verifier is not cryptographic verification.
See [proof verification and history](../research/proof-verification-and-history.md).

## Consequences

A verifier can stall instead of rejecting. Strict verification needs retrievable genuine proofs and
sufficient statement evidence. Producer skip configuration alone does not prove that no genuine
proof is retrievable elsewhere. Proof-free local progress requires the explicit development mode.

This verdict-policy decision does not establish proof-to-batch binding. In the inspected verifier,
public inputs derive from commitments in the proof package without an independent comparison to the
intended inscribed batch's statement. Upstream zkSync checks committed batch identity in its
[settlement contract](https://github.com/matter-labs/era-contracts/blob/df2c3baabd8bf1ea7b82fb6aafa5ae550c0f9b80/l1-contracts/contracts/state-transition/chain-deps/facets/Executor.sol#L472-L530);
that is a comparison mechanism, not proof of Via's binding.

Sufficient binding is a separate required follow-up design task. It blocks historical trust claims
and withdrawal activation. This ADR selects no binding design, historical checkpoint or activation order.

The subsequent [ADR 0008](0008-bind-proofs-by-chaining-verified-commitments.md) selects commitment chaining
with root openings. It does not discharge the binding proof obligations or select an anchor, failed-opening
outcome, historical range or activation order.

The decision records accepted policy, not implementation or deployment.
