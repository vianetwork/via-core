# L2 contract calls through Bitcoin

This is a design proposal. The [implemented deposit path](bridging.md) does not execute the contract-call request
encoded in an inscription's `l2_contract_address` and `call_data` fields.

The earlier bridge design gave inscriptions two purposes: deposit BTC into L2 and request an L2 contract call
through Bitcoin. A withdrawal request was a proposed use of the second capability. This page preserves that intent
and identifies the decisions needed before implementation.

## Proposed contract-call flow

A user would pay the bridge address and include a receiver, a target contract, and call data in an inscription.
The payment would fund execution fees, and the remainder would go to the L2 receiver. The original proposal did
not fully specify the call value, gas limits, refund calculation, or result of a reverted call.

The main node and verifier would independently decode the request and derive the same canonical L1 transaction.
The sequencer would execute the specified target and call data on L2. The verifier would compare the executed
request with the one observed on Bitcoin.

These steps require more than parsing the existing fields. The current `ViaL1Deposit` conversion discards the
supplied contract target and uses empty call data. The resulting `L1Tx` names the deposit receiver as its sender.
The receiver field does not prove that the Bitcoin sender controls the named L2 account.

## A withdrawal request needs an inclusion rule

The proposed withdrawal flow lets a user publish a request on Bitcoin when the sequencer refuses to accept the
request through its L2 interface. Publishing the request makes it observable to Bitcoin watchers. It does not
compel the sequencer to execute it.

The original design assumed that ordered processing would force inclusion before the sequencer could continue
producing blocks. Order and inclusion are different requirements. A sequencer can preserve the order of processed
requests while delaying pending requests.

The current [`verify_op_priority_id`](../../via_verifier/node/via_zk_verifier/src/lib.rs) checks pending deposit
hashes against the logs present in a batch. A batch with no deposit logs passes this ordering check even if the
pending queue is nonempty. The check does not impose an inclusion deadline. A forced-inclusion design would need
to define when a Bitcoin request becomes eligible, when it becomes overdue, and how omission prevents further
valid progress.

## L2 execution does not guarantee Bitcoin payout

Even after an L2 withdrawal request executes, the bridge must spend BTC to the recipient. The current payout path
depends on signatures produced through the verifier coordinator.

A forced-exit design must state what happens when the sequencer stops, the required signers stop, or both stop.
Rejecting batches that omit an overdue request can stop further valid L2 progress. Rejection does not produce
bridge signatures or authorize a user to spend bridge funds.

The bridge's governance spending path also requires its authorized participants. Its existence alone does not
provide a withdrawal path that an individual user can execute without their cooperation.

## Decisions required before implementation

The proposal leaves these decisions open:

- **Caller authorization.** A receiver address in a public message is not proof of account ownership. The request
  needs a defined L2 caller, proof of authorization, and replay protection.
- **Payment and failure rules.** The bridge payment, call value, gas budget, and refund need separate definitions.
  Each amount needs a defined outcome if decoding fails, execution reverts, or available gas is insufficient.
- **Inclusion enforcement.** An inclusion rule needs confirmation requirements, a deadline, and evidence that a
  batch omits an overdue request. The rule must account for Bitcoin reorgs and unavailable request data.
- **Bitcoin settlement.** The design must state its signer assumptions and whether users can exit if those fail.
  A deadline on the sequencer does not replace the bridge wallet's spending conditions.
- **Protocol activation.** The design must specify when the new interpretation takes effect. Existing inscriptions
  can contain contract addresses and call data that current execution ignores. Reinterpreting those fields changes
  behavior, canonical transaction hashes, and replay results. Main-node and verifier rules must change together.

## Design history

This page consolidates the bridge-related proposals from three earlier design documents:

- *Bridging of native currency through Bootloader* proposed Bitcoin-originated contract calls and a future
  withdrawal request through that message path.
- *Bitcoin Inscription Standard: Implementation Spec* described the address and call-data fields, the fee and
  remainder model, and future OP_RETURN deposits.
- *Prover & Verifier Design: MVP Spec* deferred forced-withdrawal research until after the MVP and accepted
  sequencer censorship risk for that phase.

OP_RETURN deposits are now implemented. Contract-call execution and forced withdrawals remain separate from the
deposit parser's supported encodings. The historical documents do not establish a current delivery commitment.
