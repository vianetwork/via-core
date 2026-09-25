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

The [storage research](../research/withdrawal-intent-and-observation.md) compares the alternatives.
The [authorization research](../research/withdrawal-authorization-and-refork.md) explains why storage
separation alone cannot authorize a signature.

## Alternatives and consequences

Guarded placeholders can avoid separate storage initially, but every reader must distinguish an
expected obligation from an incomplete observation. Update-only observation handling depends on
ordering or replay that prevents an early payment from being lost. Separate records preserve both
facts without that arrival-order requirement, at the cost of reconciliation and migration work.

This decision selected the separation, not an exact schema, table count, or API. At acceptance it
left the following choices open:

- The authoritative expected-fact source and parent-output provenance at signing.
- The exact authorization algorithm and concurrent signing admission rules.
- Evidence sufficient to hold or fulfill a request, and conditions for releasing a hold.
- Reorg invalidation, restart behavior, historical-row trust, and recovery.

The existing fee policy and selected signing guarantee remain unchanged. This ADR does not add a
fresh `gettxout` requirement or claim that preserved facts remain canonical at signing.
It records an accepted planning decision, not implementation, migration, deployment, or permission
to resume signing.

## Subsequent implementation

At source revision `269b81cf056b2bb13294a042b93720b4f5500efe`, withdrawal authorization,
authenticated signing, durable holds, fulfillment and reorg/restart handling are implemented.
The [verifier contract](../../via_verifier/README.md) owns those details and supersedes the
historical open list above where specified. Historical preparation and trust, a positive
fulfillment depth and activation evidence remain separate gates. This status update changes
neither this ADR's storage decision nor permission to migrate or resume signing.
