# Withdrawal lifecycle contract

This document records the design for one integrated withdrawal cutover. It extends
[ADR 0004](../adr/0004-separate-expected-withdrawals-from-observed-payments.md), which separates
expected obligations from observed Bitcoin payments. It defines intended behavior, not a claim
that the implementation is deployed or that signing may be enabled.

Implementation, migrations, tests and operator procedures must land together. This design does
not select a positive fulfillment confirmation depth or authorize any live operation.

## Facts with distinct meanings

| Fact | Meaning | Must not become |
| --- | --- | --- |
| Expected withdrawal | Checked L2 origin, recipient and gross obligation | An amount inferred from an observed output |
| Observed payment | Immutable Bitcoin transaction/output evidence, with separate inclusion history | A new obligation or implicit signing permission |
| Hold | A reason another payment must not be authorized | Proof of successful payment |
| Fulfillment | An unambiguous conforming payment with accepted inclusion and source evidence | Release of a possibly signed competing attempt |
| Signing attempt | One admitted proposal, participants, public transcript and durable risk state | A reusable secret nonce or a replacement authorization |

The full L2 transaction hash and event index identify an origin. A shortened wire reference is
not sufficient to distinguish every origin. Bitcoin output identity is `(txid, vout)`; block
inclusion is a separate fact that can change without changing the output.

## Complete expected-fact import

Import binds complete DA pubdata to the ordered L2 blocks, transactions, receipts, service logs,
messenger events and withdrawal events for the source batch. Preserve full origins and repeated
message multiplicity, not merely equal recipient/value pairs.

A complete batch with no withdrawals differs from missing DA, missing receipts or an incomplete
association. Import completion must be explicit. Missing or inconsistent evidence cannot become
a complete-empty batch or signing authority.

Classify unusable receiver bytes as nonpayable only after complete origin and content association.
Retain the original bytes, gross amount, origin and reason; do not create a payable obligation or
invent a refund policy. Failed imports remain retryable without making unrelated complete batches
unavailable.

Expected facts are immutable on replay. Identical evidence is idempotent. Conflicting evidence
is retained without replacing the original gross amount or origin, and ambiguity blocks payment.

## Authorize one fixed transaction

Use the existing shared Bitcoin transaction, fee and metadata owners. Construct exactly one
proposal containing the entire selected request set; never silently drop fee-insufficient requests
or authorize only the first transaction of a larger result.

Authorization verifies complete parent transaction bytes against their txids and referenced vouts.
Verified content can come from any provider; retrieval availability is distinct from content
validity. No mandatory fresh `gettxout` observation is added. Content verification does not reserve
an input or establish race-free canonicality at signing.

Reconstruct and compare the fixed proposal without rerunning request selection or coin selection.
Bind recipients, amounts, metadata, input ordering and identity, sequences, change, locktime and
signing messages. Preserve existing fee economics. First admission applies the existing fee
estimation policy; replay and recovery bind the durable attempt's content and fee interpretation
rather than obtaining a new price or selecting new inputs.

The network, chain, protocol, bridge script, ordered unique participant set and coordinator identity
must agree. Transaction signatures bind the complete authorized spend, including every prevout.

## Observe before or after import

Retain credible bridge-payment observations even when the expected withdrawal has not arrived.
An observation can establish a hold, but cannot create or modify expected facts. A node need not
have broadcast a payment locally to recognize that it may already have happened.

Reconcile only complete and unambiguous matches. Different candidate transactions cannot be
combined into fulfillment. Conflicting outputs, origins, references or inclusion evidence remain
held rather than silently paid or automatically retried as a new payment.

Fulfillment requires exact request-to-output association, unchanged fee/net-value construction,
current source evidence, and a uniquely canonical inclusion at an explicitly selected positive
confirmation depth. This policy is separate from the watcher's ingestion cutoff and remains
disabled until selected.

Canonical removal revokes fulfillment but retains payment risk. The same immutable transaction
can regain fulfillment after canonical re-inclusion and complete revalidation. That transition
releases neither a possibly signed attempt nor permission for a replacement payment.

Governance payments must use the shared withdrawal metadata/observation contract or an explicit
durable hold established before payout. An arbitrary unmarked payout is not automatically reconciled;
a wallet sweep does not prove a particular obligation was paid.

## Admit and retry without repeating a secret operation

Use one transactional admission boundary for expected facts, observations, reservations and source
invalidation, including races where the expected row does not yet exist. Reserve request identities
and inputs atomically. Coin selection excludes retained reservations and observed spends.

Persist coordinator exposure before publishing a proposal. Retain dedicated signer ownership;
losing the ownership connection stops signing. Authenticated exchanges bind exact body bytes,
principal, audience, method, target, challenge, round and proposal content. A participant cannot
submit another participant's slot. Bound network requests independently of authentication age.

A live attempt keeps one secret nonce per input. Persist the complete public nonce batch before
export. An uncertain acknowledgment retries the identical batch and verifies durable equality;
it does not regenerate a nonce. Validate the complete public transcript before marking possible
signing, then commit `may_have_signed` before consuming secret nonces. Persist the complete public
signature result before export.

On restart, incomplete attempts retire without reconstructing secret nonces. Only locally unsigned,
unexposed attempts may release their reservations. Exposed or possibly signed attempts remain held.
Signed recovery uses durable public results; finalized recovery rebroadcasts identical transaction
bytes until accepted canonical inclusion. A local inclusion-less observation is not a reason to
suppress that exact rebroadcast.

An unfinished live round does not expire by age. This prevents slow peers from causing repeated
exposure of fresh requests and inputs. N-of-N progress can stop after a participant loses its nonce,
even if it returns. Restoring a signed but unfinishable coordinator round does not solve that loss.
Offline reconciliation is required; no automatic abandonment, repricing or replacement protocol
is selected.

Timeout, restart, mempool absence, local abandonment, reorg, new input selection and a new round ID
are not evidence that an earlier signature can no longer produce a payment. They cannot release
exposed or signed risk.

## Invalidate source evidence without losing canonical recovery

Retain scanned Bitcoin proof inclusion and individual vote inclusions, plus their maximum source
height/hash. Validate observed hashes against the locally accepted chain under the source-invalidation
gate. Unknown provenance cannot be upgraded by duplicate proof replay or by a later vote.

The maximum anchor detects a suffix reorg, but is not sufficient to restore the proof and votes.
Separate the first invalidated withdrawal-source batch from the first deleted proof/deposit-dependent
batch. A vote-only reorg retains canonical proofs and pre-cut votes while pruning only suffix votes.
Recompute source coverage without upgrading previously unknown coverage. Reevaluate sufficient
retained quorum immediately using the retained verifier set; do not wait for another inscription.

Preserve proof-driven rejection and rejected competing forks during vote pruning. Reorg recovery
must neither choose an arbitrary rejected sibling nor violate the single canonical batch slot.
Removed proofs and deposit dependencies still require inclusive proof-chain removal and reassignment
of surviving deposits. Re-mined votes attach to retained canonical proof rows.

Exact source reimport can restore the same unsigned obligation after revalidation. It clears only
that source's uncertainty, not observation, conflict, legacy, exposure or signing-risk holds.
These are local accepted-chain guarantees, not instantaneous knowledge of the global Bitcoin tip.

## Ownership and cutover

Extend existing Via owners rather than maintaining a second withdrawal implementation:

- Shared Bitcoin scanning owns inclusion metadata and explicit scan modes. The main watcher ignores
  withdrawal observations, the public indexer uses best-effort recognition, and the verifier requires
  complete payment provenance.
- Shared withdrawal and MuSig2 owners perform complete import and fixed monetary construction.
- The verifier DAL owns durable facts, attempts, reservations and transactional invalidation.
- Node adapters own HTTP, polling, signer lifetime and reorg scheduling.

The storage and authenticated exchange changes require a coordinated cutover, not an implicit
mixed-version compatibility layer. Fence legacy withdrawal writes. Preserve old records as forensic
history and negative quarantine; old amounts and paid flags do not become authorization data.
An automatic rollback that discards durable risk evidence is not safe.

Before activation, reconstruct and reconcile complete history, legacy signing/payment risk, unknown
source provenance and batches in flight during migration. There is no activation-start-batch exemption.
Required historical-parent retrieval is a separate availability dependency from supplied signing
parents. Wallet/domain changes fail closed and require explicit reconciliation rather than clearing
identity records. Operational procedures must account for receipt-provider drift, history-dependent
storage/query costs and nonce-loss recovery limits.

## Required evidence

The implementation review must exercise both import/observation orders, identical and conflicting
replays, origin/reference collisions, nonpayable evidence, exact transaction mutations, admission
races, nonce persistence failures, public-only recovery, and canonical removal/re-inclusion.
Reorg proof includes retained pre-cut votes, a sufficient quorum without a replacement vote,
competing rejected siblings, and proof-forced rejection.

Report Bitcoin policy/mining proof separately from DA/L2 import and database/HTTP recovery proof.
Local tests do not establish production history coverage, current deployment, a fulfillment policy,
or permission to resume signing.
