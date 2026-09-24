---
status: accepted
---

# Keep expected withdrawals separate from observed Bitcoin payments

A node can observe a Bitcoin payment before it knows the corresponding withdrawal request.
The request's gross amount and the payment's net output value describe different facts.
Using one record for both can let a payment observation become the source of payment authority.

## Decision

Preserve expected withdrawal facts and observed Bitcoin payments as distinct durable records.
Keep the expected identity, payout destination, and gross amount. Retain the observed output identity,
payout destination, amount, and chain evidence separately, including observations whose expected
request is not yet known locally.

Reconcile those records without allowing an observation to create or overwrite expected payment
facts. A net payment must not replace the gross request amount. Recording an observation does not
itself establish fulfillment or authorize another payment.

The [withdrawal lifecycle design](../design/withdrawal-lifecycle.md) describes the integrated
contract built on this separation. Storage separation alone cannot authorize a signature.

## Alternatives and consequences

Guarded placeholders can avoid separate storage initially, but every reader must distinguish an
expected obligation from an incomplete observation. Update-only observation handling depends on
ordering or replay that prevents an early payment from being lost. Separate records preserve both
facts without that arrival-order requirement, at the cost of reconciliation and migration work.

This decision selects the separation, not an exact schema, table count, or API. The following
choices are governed by the linked lifecycle design rather than by this ADR alone:

- The authoritative expected-fact source and parent-output provenance at signing.
- The exact authorization algorithm and concurrent signing admission rules.
- Evidence sufficient to hold or fulfill a request, and conditions for releasing a hold.
- Reorg invalidation, restart behavior, historical-row trust, and recovery.

The existing fee policy and selected signing guarantee remain unchanged. This ADR does not add a
fresh `gettxout` requirement or claim that preserved facts remain canonical at signing.
It records an accepted design decision, not implementation, migration, deployment, or permission
to resume signing.
