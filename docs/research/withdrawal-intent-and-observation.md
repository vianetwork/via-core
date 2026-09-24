# Withdrawal intent and observed Bitcoin payments

**Status:** research. [ADR 0004](../adr/0004-separate-expected-withdrawals-from-observed-payments.md)
accepts separate expected and observed records. Schema, signing, hold, and recovery decisions remain open.
**Evidence captured:** 2026-09-22 UTC.
**Question:** how should a Bitcoin withdrawal system preserve what is owed separately from what was
observed on Bitcoin, especially when the payment is observed before its request is available locally?

## Conclusion

Separate request and payment records have a directly relevant precedent in **sBTC**. Its
[withdrawal-output migration][sbtc-outputs] explicitly allows signers to record outputs for requests they
do not yet know. NBXplorer supplies a complementary distinction: a transaction output has an identity
separate from its inclusion in a particular block.

These findings informed Via's accepted choice of separate durable expected and observed records.
They do not establish a universal bridge schema or settle reconciliation, signing, or reorg policy.
Guarded placeholders remain a valid comparison, but are not the selected Via storage direction.

## Concepts used in this note

These are working distinctions for withdrawal design, not additions to the accepted domain glossary:

- **Expected withdrawal:** identity, Bitcoin payout destination, gross amount, and source facts defining
  an obligation. Observing a payment does not itself establish these facts.
- **Observed payment:** transaction/output identity, payout destination, and net amount actually present
  in a Bitcoin output. It may be recorded before the matching expected withdrawal is known.
- **Block inclusion:** evidence that a transaction appeared in a particular block. Re-inclusion does not
  create a second logical payment.
- **Authorized signing attempt:** the exact proposal, input information, signing messages, and local
  signing round that passed authorization. An observation is not a signing authorization.

## Evidence from comparable implementations

### sBTC: explicit support for observation-first arrival

At commit `ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef`:

- [`withdrawal_requests`][sbtc-base] stores requested amount, destination script, transaction origin, and
  maximum fee. Its primary key includes `request_id` and the Stacks block hash because the identifier
  alone does not distinguish forks.
- [`bitcoin_withdrawal_tx_outputs`][sbtc-outputs] has primary key `(txid, output_index)`, references the
  corresponding Bitcoin output, and records a `request_id`.
- That output table deliberately has **no foreign key to `withdrawal_requests`**. The migration states:

  > to enable signers to write the serviced withdrawal IDs even for requests they do not know about.

- A [separate migration][sbtc-arrival] removes a request-to-Stacks-block foreign key because the block
  need not already be present when the request is written.
- The [writers][sbtc-writers] use checked integer conversions and `ON CONFLICT DO NOTHING`. This proves
  insert-conflict suppression in those methods, **not** comparison of every conflicting field.

**Transferable:** preserve observed outputs independently of local request availability; distinguish
request identity, output identity, and source inclusion.

**Not transferable without justification:** Stacks serial identifiers, chain-selection logic, maximum-fee
policy, and assumptions about the number of requests represented by one output.

### NBXplorer: output identity is not block inclusion

At commit `27585a7a83b11a96adad9facf092c0d07d327ac2`, the [SQL schema][nbx-schema] separates:

| Relation | Primary key | Meaning |
| --- | --- | --- |
| `txs` | `(code, tx_id)` | Transaction |
| `outs` | `(code, tx_id, idx)` | Transaction output |
| `blks_txs` | `(code, tx_id, blk_id)` | Transaction inclusion in a block |

The schema also maintains denormalized current block fields. Its deconfirmation logic clears those
fields while using the separate inclusion relation. The [official schema guide][nbx-guide] describes
the indexer's block/match/confirmation flow.

**Transferable:** a payment and a claim about its current block location are different facts.

**Limit:** NBXplorer is an indexer. Its schema does not authorize a bridge withdrawal or prescribe Via's
confirmation threshold. Copying its descriptors, triggers, or multi-asset model is unnecessary here.

### BTCPay Server: an invoice and its payments retain separate amounts

At commit `a305e951761784e65834f03f082cb880b93b99b0`, [`InvoiceData`][btcpay-invoice] contains an amount
and a collection of payments. [`PaymentData`][btcpay-payment] has its own amount, invoice association,
`(Id, PaymentMethodId)` key, and Processing/Settled/Unaccounted statuses.

**Transferable:** retain the obligation and individual payment facts rather than changing one amount
field's meaning as processing progresses.

**Limit:** these model files alone do not prove unknown-invoice handling or complete replay/reorg
behavior. Fiat/cryptocurrency invoice accounting is not Via's Bitcoin fee-allocation contract.

### Fedimint: logical output mapping and transaction lifecycle are distinct

At v0.11.2, commit `1b8a5638e0ee3f327ba196bba6eebc344dbb1a49`, the [wallet database definitions][fedimint-db]
include `UnsignedTransactionKey(Txid)`, `PendingTransactionKey(Txid)`, and
`PegOutBitcoinTransaction(fedimint_core::OutPoint)` mapped to `WalletOutputOutcome`.

**Transferable:** distinguish the logical operation from unsigned/pending Bitcoin transactions.

**Limit:** the federation's logical outpoint is not a Bitcoin outpoint. These definitions alone do not
prove all transition rules, unknown-request handling, or suitability of its signing protocol for Via.

### Rootstock: request and signing collections have separate storage

At `d312d6723b57bf366e5d68856290c63651ef6ef1`, [`BridgeStorageProvider`][rootstock-storage]
loads and saves `ReleaseRequestQueue`, `PegoutsWaitingForConfirmations`, and
`PegoutsWaitingForSignatures` separately. The signature collection maps `Keccak256` keys to
`BtcTransaction` values. `RSKIP146` controls additional transaction-hash-aware serialization for
the request and confirmation collections.

This is a lifecycle-storage comparison, not another proof of observation-first ingestion.
The inspected storage methods distinguish requests from transactions awaiting later processing.
They do not establish unknown-request handling, all transition rules, or a Via-compatible signing
protocol. The source is commit-pinned, not tied here to a release or deployment.

### zkSync Era: logical transactions, attempts, and pending receipt

The inspected upstream revision was `ff5f519b11cff863edcfa0f75af10fea113806b0`, not a claim of latest
upstream. In [`eth_sender_dal.rs`][zksync-sender], `save_eth_tx` stores a logical transaction and
`insert_tx_history` stores attempts. `insert_pending_received_eth_tx` explicitly permits a pending
record before validation, requiring validation before an executed status.

The external-node path also uses placeholder transaction records. This is a useful counterexample to
an absolute claim that placeholders are always wrong: their safety depends on the authority granted
by the surrounding interface.

**Transferable:** recording receipt is distinct from accepting execution or granting authority.
**Limit:** Ethereum account nonces, gas, and finality semantics are not Bitcoin signing-round semantics.
No drop-in Bitcoin/MuSig2 implementation is established by this source.

## Candidate implementation of the accepted separation

### Preserve ownership and stable meanings

Keep expected withdrawals in the existing verifier-owned model. Reuse the existing bridge-transaction
parent for transaction-level identity, with a small separate output relation if existing storage cannot
retain the required observation facts. Keep gross requested amount and net paid amount distinct.

An illustrative relation named `via_bridge_withdrawal_outputs` would contain:

- Existing bridge-transaction parent identifier and actual `vout` as its composite key.
- Observed withdrawal reference, without a foreign key requiring the expected request to exist.
- Payout destination/script and `paid_sats`, always meaning net output value.

An output key should resolve to the **canonical Bitcoin transaction ID and actual output index**.
A normalized transaction identifier is not interchangeable merely because it often matches for a
particular transaction form. Verify serialization and byte order explicitly: Bitcoin display txids are
byte-reversed relative to the underlying hash-byte representation. Do not infer vout by enumerating a
filtered collection of payout outputs.

Preserve wire references exactly. At Via revision `8a49f355bfe31720f21b8181db471243709194e5`,
[`L2WithdrawalMeta::from_bytes`][via-wire] stores the complete **10-byte** reference as hex in `l2_id`
and separately decodes bytes 8–9 as the event index. `to_bytes` serializes that 10-byte field. The
separate event-index field is not another independent identity component. Changing this representation
would require a separately justified compatibility decision.

Use existing `bitcoin::Amount` and checked conversions to/from database satoshis. Use `u32` for vout
with corresponding database range checks. Keep origin provenance distinct from short wire references.
If block-inclusion history is required, model it separately; a first-seen height or overwritten hash
must not be described as canonicality evidence.

### Candidate interfaces and file ownership

All names below are suggestions, not standardized names or implemented APIs.

| Responsibility | Candidate name | Existing owner |
| --- | --- | --- |
| Expected facts | Keep `WithdrawalRequest` | `via_verifier/lib/via_verifier_types/src/withdrawal.rs` |
| Parsed payment | Keep `L1Withdrawal`; preserve actual vout | `core/lib/via_btc_client/src/indexer/withdrawal/mod.rs` and `parser.rs` |
| Expected-only persistence | `insert_expected_withdrawals` | `via_verifier/lib/verifier_dal/src/withdrawals_dal.rs` |
| Observed-output persistence | `record_withdrawal_observations` | Same DAL owner |
| Matching and conflict classification | Internal `reconcile_withdrawal_observations` | Same DAL owner, with explicit transaction ownership |
| Observation ingestion | Existing withdrawal processor | `via_verifier/node/via_btc_watch/src/message_processors/withdrawal.rs` |
| Fixed-proposal authorization | `authorize_withdrawal` | `via_verifier/node/via_verifier_coordinator/src/sessions/withdrawal.rs` |
| Fee/output arithmetic | Existing fee strategy and builder | `via_verifier/lib/via_musig2/src/fee.rs` and `transaction_builder.rs` |

No new service or generic validation framework is implied. A separate relation would warrant migration
files; exact schema and migration strategy are still open. Shared parser/type changes must also account
for indexer consumers rather than treating the verifier as the only reader.

### Reconciliation must not silently grant authority

- Unknown observations can be stored without inventing expected withdrawals or blocking unrelated
  block ingestion.
- Identical replay is harmless; conflicting replay must not replace the amount, origin, or payout
  destination of an established fact.
- Repeated recipients do not imply identical withdrawals. Preserve each actual output and its metadata
  association; do not add aggregation or many-to-many allocation without a supported protocol case.
- Multiple observations for one request require an explicit conflict policy. PostgreSQL
  [does not deterministically select a source row][pg-update] when `UPDATE ... FROM` joins one target
  to several source rows.
- Matching a reference and destination is not proof of complete payment/value conservation. Recording
  evidence, preventing unsafe resubmission, and confirming fulfillment are separate transitions.
- An eligibility anti-join sees observations visible to its database snapshot. It does not by itself
  settle concurrent arrival, already-started signing, or conflicting-round behavior.
- Clearing a processed association can restore apparent eligibility. Unlink/delete behavior belongs
  with the invalidation policy, not incidental foreign-key cleanup.

Two settlement-ownership designs remain distinct: a reconciler can establish fulfillment from a validated
observation, or only a local broadcast session can set the processed association. The latter does not
eliminate the need for observation-aware eligibility. If a payment is recorded first and the matching
expected request arrives later, absence of a local broadcast record does not establish that payment is
still owed.

A bounded SQL-predicate experiment illustrates this: store the observation, insert the expected request
with a null processed association, then select pending requests using only that null association and an
amount threshold. The request is selected despite the preserved observation. This was checked in an
isolated in-memory SQLite fixture, not a running signer or PostgreSQL concurrency test.

The design therefore needs a rule that distinguishes **holding a potentially paid request** from
**confirming successful fulfillment**. Keeping evidence non-authoritative does not mean ignoring it when
preventing repeat payment. Persisting a conflict record likewise does not automatically place the
corresponding expected request on hold.

## Alternatives and standards

**Separate durable observations** keep the meaning of each amount stable and preserve observation-first
and conflicting-payment evidence. The cost is schema, reconciliation, and migration work.

**Guarded placeholders** may avoid a separate relation initially, but require an enforced discriminator,
stable field meanings, complete eligibility checks, and a way to retain conflicting observations. Their
safety must be demonstrated at every authority boundary.

**Update-only observation handling** is smaller, but requires a proven ordering or replay mechanism that
prevents observations from being lost before expected records exist. Ordering is an invariant to prove,
not infer from typical runtime timing.

Authorization also has two viable expressions: validate a fixed proposal directly, or reconstruct the
expected outputs from the same fixed inputs and requests and compare. Neither requires fresh coin
selection. Both need one deterministic fee/output arithmetic owner; storage research does not settle
this choice or justify changes to the fee policy.

[BIP327][bip327] governs MuSig2 signing and secret-nonce use. [BIP341][bip341] defines Taproot signature
commitments. [BIP174][bip174] specifies PSBT roles and interchange. None of those documents prescribes
the database schema above. This investigation found no applicable universal withdrawal-storage RFC;
that is a bounded research finding, not proof that no relevant specification exists.

## Open questions and later evidence

Before selecting an implementation, resolve:

1. What minimum facts and exact schema implement the accepted separation?
2. Exact request/output association, canonical txid representation, and source-inclusion provenance?
3. What holds an ambiguous or conflicting request, and what evidence permits settlement or re-authorization?
4. Which transaction/round boundaries make concurrent import, observation, and signing safe?
5. What historical-data and ongoing invalidation requirements are prerequisites for rollout?

A later candidate should demonstrate both arrival orders, identical/conflicting replay, unknown requests,
multiple outputs to one destination, competing payments, numeric boundaries, atomic updates, concurrent
arrival, exact fee behavior, and block removal/re-inclusion where in scope. Source research is not that
runtime proof. Only the isolated SQL-predicate experiment above was run; no signer, chain, PostgreSQL
concurrency, or deployment checks were performed for this note.

## Holds, late payments, and release evidence

Further primary-source inspection on 2026-09-24 supports the following recommendations.
They do not select Via's confirmation threshold or recovery policy. No new experiment ran.

### Absence is weaker evidence than a conflicting confirmed spend

Bitcoin Core at `f490f5562d4b20857ef8d042c050763795fd43da` makes this distinction explicit.
[`CWallet::AbandonTransaction`][core-wallet] requires zero chain depth and absence from its
mempool before setting `TxStateInactive{abandoned=true}`. Its comments state that abandonment
can reverse when the transaction returns to the mempool, confirms, or becomes conflicted.
`MarkConflicted` instead records a conflicting block hash and height.
`transactionRemovedFromMempool` does not treat the removal callback alone as that block proof.

The [NBXplorer implementation][nbx-schema] provides a complementary example.
`blks_confirmed_update_txs` clears current block fields and sets `mempool=true` on deconfirmation.
It retains transaction outputs and inclusion relations. This provisional mempool flag is indexer
state, not proof that every peer currently holds the transaction. `save_matches` records
`replaced_by`, and `txs_denormalize` propagates replacement state to descendants.

sBTC goes further in [`is_withdrawal_inflight` and `is_withdrawal_active`][sbtc-read].
The first walks proposed transactions in `bitcoin_tx_sighashes` starting from the current
signer UTXO. The second finds the request's last considered height and a later confirmed signer
sweep. It keeps the request active while that later sweep is absent or insufficiently confirmed.
That release argument depends on sBTC's chained signer UTXO. A wallet with unrelated available
inputs cannot infer the same exclusion merely because some later bridge transaction confirmed.

For Via, keep request holds through mempool loss, local abandonment, restart, unconfirmed
replacement, and reorg. A timeout can notify an owner but cannot establish that payment is impossible.
After a signature may have escaped, a new attempt needs evidence that it cannot pay alongside
any prior candidate. A confirmed conflicting input spend can provide such evidence under an
explicit reorg policy. Fresh inputs and a new attempt ID do not. Preserve the supporting conflict
evidence and restore the hold if that evidence loses its accepted inclusion.
Restoring a hold after a deeper reorg cannot revoke a second signature already exported.
Any confirmation-based release therefore retains an explicit residual reorg risk.

[BIP125][bip125] describes replacement as a transaction spending one or more of the same inputs.
A second payout using unrelated inputs is not mutually exclusive with the first payment.
The cited opt-in policy is not a claim about every current node's relay rules, and acceptance
into a mempool is not proof that a replacement will confirm.

### A hold is not fulfillment

tBTC at `40a11d1dcdcf82d3962e430067cfe3f97be81483` retains late-payment recognition.
[`notifyRedemptionTimeout`][tbtc-redemption] moves a request into `timedOutRedemptions`.
`processNonChangeRedemptionTxOutput` later checks the stored fee-adjusted amount range and
removes the timed-out request after a valid payment. Its refund, slashing, wallet-state changes,
and one-pending-request-per-wallet/script rule are not Via policy. The transferable point is that
a local timeout does not erase evidence needed to recognize a later Bitcoin payment.

The proposed Via reconciler should hold a plausible request when it sees a relevant recognized
bridge payment, even if the amount is wrong or the expected import has not arrived.
Arbitrary third-party metadata must not gain that power without bridge-spend provenance.
Import must consult those observations before making the new expected row eligible.

Fulfillment needs an unambiguous request-to-actual-vout mapping, the expected recipient, the
correct net amount under the unchanged fee rules, and accepted inclusion evidence. A locally
authorized transaction can supply the complete amount context. An observation without local
broadcast history can also qualify if the reconciler independently establishes the same facts.
Missing peer requests or fee context leaves a hold, not a guessed settlement.

For example, a 9,000-sat output does not fulfill a 10,000-sat gross request with a 100-sat fee share.
It also does not permit an automatic second payment or top-up. Multiple outputs for one request
remain a conflict unless an existing protocol rule defines their allocation. Repeated recipients
for distinct requests remain separate output mappings. Identical replay is idempotent.

### Admission must share the observation writer's boundary

PostgreSQL's [row-lock and advisory-lock contracts][pg-locking] do not make an eligibility anti-join
atomic with a separate observation insert. A row lock cannot lock an expected row that does not exist.
A short wallet-scoped transaction lock shared by import, observation reconciliation, invalidation,
and attempt admission is a simple initial design for a single bridge wallet.
Acquire it before reading current eligibility. With READ COMMITTED, use a new statement for the
reads after a potentially waiting lock acquisition. Persist the attempt membership atomically,
then release the lock before network or signing work.

Before a signature share can escape, the signer needs a durable may-have-signed transition for
the same still-admitted attempt. Observation committed first blocks that transition. A transition
committed first means a later observation cannot recall a share, so it records a conflict and keeps
the request held. This orders local authority, not unseen Bitcoin events or separate verifier databases.
It does not reserve UTXOs. The detailed round and nonce contract belongs to signing authorization.

LND at `d72a3aaf261e278fa4aad5be4453df2f74ab50ee` provides a concrete admission comparison:
[`RegisterAttempt`](https://github.com/lightningnetwork/lnd/blob/d72a3aaf261e278fa4aad5be4453df2f74ab50ee/channeldb/payment_control.go)
checks payment eligibility, counts in-flight amounts, and writes the attempt within one `kvdb.Batch`.
Its Lightning amount accounting is not Via's Bitcoin-input policy, but the admission/write boundary
is useful. Via's attempt should retain selected outpoints and transaction identity so restart can
restore local in-flight context before selecting again. A node may already exclude known mempool
spends; the durable record covers context the local process would otherwise forget. This prevents
conflicting local admission, not every external spend or two payments from different inputs.

Future proof must cover both arrival orders, concurrent admission and observation, a restart
after the may-have-signed marker, and removal of a block supporting a prior release.
No database concurrency or Bitcoin replay proof was performed in this research pass.

## Evidence boundary

Exa searches supplied discovery leads. The central schema claims above were checked against official
repository files at the cited revisions; search excerpts and model agreement are not treated as proof.
The inspected files support only the mechanisms described here, not a complete audit of those systems.
Raw consultations, private findings, and execution metadata are not part of this durable note.

[ADR 0004](../adr/0004-separate-expected-withdrawals-from-observed-payments.md) records the accepted
separation. This document remains research; its candidate schema and reconciliation rules are not
an implementation contract.

[sbtc-base]: https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/migrations/0003__create_tables.sql
[sbtc-outputs]: https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/migrations/0014__add_bitcoin_tx_outputs_.sql
[sbtc-arrival]: https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/migrations/0013__consolidate_withdrawal_tables.sql
[sbtc-writers]: https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/src/storage/postgres/write.rs
[nbx-schema]: https://github.com/btcpayserver/NBXplorer/blob/27585a7a83b11a96adad9facf092c0d07d327ac2/NBXplorer/DBScripts/FullSchema.sql
[nbx-guide]: https://docs.btcpayserver.org/NBXplorer/Postgres-Schema/
[btcpay-invoice]: https://github.com/btcpayserver/btcpayserver/blob/a305e951761784e65834f03f082cb880b93b99b0/BTCPayServer.Data/Data/InvoiceData.cs
[btcpay-payment]: https://github.com/btcpayserver/btcpayserver/blob/a305e951761784e65834f03f082cb880b93b99b0/BTCPayServer.Data/Data/PaymentData.cs
[fedimint-db]: https://github.com/fedimint/fedimint/blob/1b8a5638e0ee3f327ba196bba6eebc344dbb1a49/modules/fedimint-wallet-server/src/db.rs
[zksync-sender]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/dal/src/eth_sender_dal.rs
[via-wire]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/indexer/withdrawal/mod.rs
[pg-update]: https://www.postgresql.org/docs/current/sql-update.html
[bip327]: https://github.com/bitcoin/bips/blob/eba8e50cb66d436c65c6bc8b0a175b643effe9d3/bip-0327.mediawiki
[bip341]: https://github.com/bitcoin/bips/blob/master/bip-0341.mediawiki
[bip174]: https://github.com/bitcoin/bips/blob/master/bip-0174.mediawiki
[rootstock-storage]: https://github.com/rsksmart/rskj/blob/d312d6723b57bf366e5d68856290c63651ef6ef1/rskj-core/src/main/java/co/rsk/peg/BridgeStorageProvider.java#L168-L324
[core-wallet]: https://github.com/bitcoin/bitcoin/blob/f490f5562d4b20857ef8d042c050763795fd43da/src/wallet/wallet.cpp
[sbtc-read]: https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/src/storage/postgres/read.rs
[tbtc-redemption]: https://github.com/keep-network/tbtc-v2/blob/40a11d1dcdcf82d3962e430067cfe3f97be81483/solidity/contracts/bridge/Redemption.sol
[pg-locking]: https://www.postgresql.org/docs/17/explicit-locking.html
[bip125]: https://github.com/bitcoin/bips/blob/eba8e50cb66d436c65c6bc8b0a175b643effe9d3/bip-0125.mediawiki
