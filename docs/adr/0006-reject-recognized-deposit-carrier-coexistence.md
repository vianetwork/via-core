---
status: accepted
---

# Produce no deposit when recognized deposit carriers coexist

A witness carrier and an OP_RETURN carrier can describe the same Bitcoin payment.
Choosing the first decoded message makes deposit eligibility depend on parser order.
The accepted policy rejects that choice rather than preferring either carrier.

## Decision

A recognized witness deposit carrier and an unreserved OP_RETURN deposit carrier in the same
Bitcoin transaction yield no deposit. Equal receiver bytes do not make coexistence acceptable.
The rule concerns deposit eligibility, not the rejection of unrelated system or withdrawal messages.

This is a later, scoped change to the independent-message policy in
[ADR 0003](0003-decode-op-return-deposit-pushes.md). It supersedes that independence only for recognized
deposit-carrier coexistence. ADR 0003's first-OP_RETURN selection, push decoding, reserved-prefix
exclusions, and ignored-data rules remain unchanged. The invalid-address decision in
[ADR 0002](0002-reject-invalid-deposit-address-lengths.md) does not define the coexistence boundary.

## Unresolved recognition and compatibility

This ADR does not define which malformed messages count as recognized carriers. Marker presence,
successful field decoding, and a parser returning `Some` are not interchangeable definitions.
Short pushes, reserved prefixes, malformed witness fields, unrelated witnesses, and multiple inputs
need an explicit classification specification before implementation.
The [carrier research](../research/conflicting-deposit-messages.md) records those open cases.

The supported historical domain, persisted-state reconciliation, and coordinated activation also
remain open. The [compatibility research](../research/deposit-compatibility-evidence.md) describes the
evidence needed. ADR 0003's decoder census is not evidence for this later eligibility change.

## Consequences

Carrier precedence and database insertion order cannot select a deposit under this policy.
Rejecting a deposit does not reverse its Bitcoin payment, refund the sender, or undo an existing
L2 credit. This ADR defines no recovery process.

The decision records accepted policy, not implementation or deployment. It does not authorize a
migration, a historical replay against shared state, or a release.
