# Withdrawal authorization and a future re-fork

Status: research complete; whole-workstream implementation with independent research and review approved. Remaining material policies are recommendations, not accepted decisions. No live migration, deployment, publication, or signing resumption is authorized. Evidence inspected on 2026-09-23 and 2026-09-24.

The selected guarantee is full value conservation against preserved expected withdrawal facts, bound to one signing round. It is not a guarantee that those facts remain canonical at signing. A fresh Bitcoin `gettxout` admission check is not mandatory under that choice. Historical-row trust and ongoing invalidation remain deployment prerequisites.

## Why storage alone cannot authorize a signature

[Withdrawal intent and observed Bitcoin payments](withdrawal-intent-and-observation.md) compares separate observations with guarded placeholders.
[ADR 0004](../adr/0004-separate-expected-withdrawals-from-observed-payments.md) accepts separate durable expected and observed records.
Their exact schema remains open. An obligation, a payment observation, and an authorized signing attempt have different identities and writers.
Deduplicating one does not deduplicate the others.

The source baseline is Via `8a49f355bfe31720f21b8181db471243709194e5`. The architecture assigns Bitcoin parsing to `core/lib/via_btc_client`, signing to the verifier coordinator, and durable state to the verifier DAL. The [MuSig2 guide][musig-guide] describes an N-of-N key-path bridge wallet with a separate governance script path. A single signer withholding its share can therefore stop the normal withdrawal path. This note does not propose a new threshold or governance scheme.

Additional inspected revisions: Via era-contracts `a14df38896f6e7eafc45e65eefdb63bf3cacebe4`,
local newer zkSync `ff5f519b11cff863edcfa0f75af10fea113806b0`, and installed Bitcoin library
`0.32.7`. The newer zkSync checkout is not asserted to be latest upstream; contract source is not
proof of the deployed contract version.

The expected-fact path starts at `WithdrawalSession::prepare_withdrawal_session`. It selects finalized batches, obtains withdrawals through `WithdrawalClient::get_withdrawals`, and writes them with `insert_withdrawals`. The observed-payment path is `WithdrawalProcessor::process_messages` in the verifier Bitcoin watcher. It resolves a bridge transaction, records processing, and associates parsed withdrawals in a database transaction. These are separate ingestion paths despite their shared DAL owner. The main-node and verifier watchers are declared siblings in `.github/sibling-paths.yml`; a shared parser change also affects consumers outside the signing process. [Expected import][session] · [Observation owner][watcher] · [DAL owner][dal] · [Sibling map][siblings].

`via_withdrawals` carries `id`, `l2_tx_hash`, `l2_tx_log_index`, `receiver`, and `value`. `via_bridge_withdrawals` carries transaction identity and processing state. Those column names are implementation facts, not evidence that every historical value has the provenance required for authorization. The durable meaning needed by the selected guarantee is a gross expected amount and destination supplied by an authorized expected-fact import. An observed net payment must not acquire that authority merely by matching a short reference. [DAL definitions][dal].

The existing wire representation is also a compatibility boundary. `L2WithdrawalMeta` retains the complete ten-byte reference in `l2_id`, with the event index decoded from its final two bytes. Do not append that decoded index to the reference as though it were an independent identity. Preserve canonical Bitcoin txid bytes and actual vout for payment observations. Re-inclusion in a different block changes inclusion evidence, not output identity. [Wire owner][wire] · [Prior identity research](withdrawal-intent-and-observation.md#candidate-implementation-of-the-accepted-separation).

## What one signing attempt must approve

The proposed boundary is a check over a fixed proposal. The coordinator has already selected requests and inputs. Authorization establishes that the complete transaction pays those requests under the existing fee rules and leaves no unexplained output or value. It also derives the exact signing messages locally. This is a proposed implementation boundary, not an accepted new API.

`UnsignedBridgeTx` and `TransactionBuilder::get_tr_sighashes` are the existing transaction and digest owners. The latter uses `TapSighashType::All` with `Prevouts::All`. `WithdrawalSession::_verify_sighashes` compares the provided messages with locally computed messages. The signer lifecycle lives separately in `ViaWithdrawalVerifier::loop_iteration`, `submit_nonce`, and `submit_partial_signature`; it cannot be replaced by a database eligibility predicate. [Transaction builder][builder] · [Session checks][session] · [Signer lifecycle][verifier].

The fee policy has one existing owner. `FeeStrategy::estimate_fee` rounds the estimated fee to a multiple of the nonzero output count. `WithdrawalFeeStrategy::apply_fee_to_outputs` divides that fee equally and can remove fee-ineligible outputs while constructing a proposal. The builder adds the full fee to net payouts when calculating total required input value, then derives bridge change. These construction rules are not permission for an authorization check to silently choose a different request set. [Fee implementation][fees] · [Builder arithmetic][builder].

For the fixed set actually authorized, conservation means that the sum of supplied input values equals the sum of all transaction outputs plus the authorized fee. Each payout additionally matches its expected gross amount minus its allocated fee. The metadata carrier, bridge change script, ordering rules, duplicate request references, input outpoints, and numeric conversions must agree with the same policy. Global equality alone does not establish correct individual payments.

There are two reasonable expressions of this check:

- Direct validation accounts for each proposed output against the fixed requests and shared fee calculation. It makes rejection reasons explicit but must not duplicate the builder's arithmetic.
- Reconstruction builds the expected result from the same fixed requests and inputs, then compares it with the proposal. It reuses output encoding but must not import fresh coin selection, chunking, or request-dropping decisions into authorization.

Neither design requires fresh coin selection.
Before commissioning work, compare any existing proposal with the required amount lookup, reconstruction, parent-output checks, and round binding.
The design choice should not create a second implementation of behavior that already has an appropriate owner.

## The signer needs a round identity

An illustrative local authorization result would bind network, wallet key set and tweak, proposal bytes, ordered prevouts, withdrawal references, fee interpretation, and locally derived messages to a round identifier. It need not become a public type or a new service. A transaction ID alone does not identify all supplied prevout values and signing context.

The intended transitions are proposal received, proposal authorized, nonce published, peer transcript fixed, partial signature created, and attempt completed or abandoned. An identical delivery may reuse the already produced response. A changed proposal or transcript must not reuse a consumed secret nonce. After a restart, the process needs either enough trusted state to return the same result safely or a fresh round that cannot collide with the abandoned one. Request holds and competing-attempt exclusion need a common authority boundary with the database. A null processed association is not, by itself, permission to sign.

The [Bitcoin Core abandonment comparison](synthetic-bridge-checks.md#proposed-proof-and-spending-contract)
shows why absence from a local mempool is not proof of permanent failure. Its wallet rules do not
select Via's observation-hold or release policy.

[BIP327][bip327] requires one-time secret-nonce use. It recommends binding known messages during nonce generation but permits preprocessing; it does not require all nonces to be generated after the message is known. [BIP341][bip341] explains why `SIGHASH_ALL` without `ANYONECANPAY` commits to input outpoints, amounts, scripts, and outputs. False supplied prevout data cannot yield a valid signature for different real prevouts under those commitments. That is not a proof of current unspentness or of canonical withdrawal origin. A parent-transaction lookup can detect incorrect prevout data earlier, but its availability policy remains separate from the accepted guarantee. [BIP174's signer role][bip174] is a useful review discipline, not a requirement to migrate Via to PSBT.

## Comparable implementations

### sBTC checks requests before constructing signing digests

At `ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef`, `BitcoinPreSignRequest::pre_validation` rejects empty packages and duplicate request IDs. `fetch_all_reports` reads withdrawal reports using `QualifiedRequestId` and rejects unknown requests. `construct_package_sighashes` and `construct_tx_sighashes` construct a transaction from reports and call `construct_digests`. `BitcoinTxValidationData` retains the transaction, signing digests, fee, and chain tip. This is a concrete precedent for turning independently loaded request facts into a fixed signing object. [sBTC validation][sbtc].

Its `BitcoinTxContext` explicitly includes `chain_tip`, `chain_tip_height`, `signer_public_key`, and `aggregate_key`. Those checks depend on Stacks state, canonical-chain selection, and a chained signer UTXO. Copying them would strengthen Via's selected guarantee without a decision. The separate observation schema is discussed in the linked storage note; neither schema nor this function alone proves sBTC's entire restart protocol.

### tBTC validates observed payment against an existing obligation

At `40a11d1dcdcf82d3962e430067cfe3f97be81483`, `RedemptionRequest` stores `requestedAmount`, `treasuryFee`, `txMaxFee`, and `requestedAt`. `processRedemptionTxOutputs` and `processNonChangeRedemptionTxOutput` require each recognized redemption output to fit between the request's redeemable amount minus maximum transaction fee and the redeemable amount. Successful handling removes the corresponding pending or timed-out redemption. `OutboundTx::processWalletOutboundTxInput` checks the wallet's main UTXO and marks it spent. [tBTC redemption][tbtc].

The useful distinction is obligation versus proof of payment, with fees preserved from request creation. Its range-based fees, Ethereum storage, SPV proof path, and single-wallet-input rule are not Via's equal-fee or MuSig2 rules. Request deletion there does not justify deleting Via's replay or payment evidence.

### CometBFT makes identical retries different from conflicting signatures

At `f4d73cd5a091a997d1f040850710b6937e650125`, `FilePVLastSignState` stores height, round, step, sign bytes, and signature. `CheckHRS` rejects regressions. `FilePV::signVote` returns the stored signature for identical sign bytes, permits a narrow timestamp-only equivalence, and rejects other conflicting data. `saveSigned` persists the result before returning it, using atomic file replacement. [CometBFT private validator][comet].

This is a useful crash-and-retry precedent. It is not a MuSig2 nonce implementation. Via must not copy its timestamp exception or consensus height keys without proving equivalence for Bitcoin signing messages.

## What newer local zkSync contributes

The inspected newer local zkSync is `ff5f519b11cff863edcfa0f75af10fea113806b0`, not verified latest upstream. `EthSenderDal::save_eth_tx` stores a logical transaction, while `insert_tx_history` stores attempts. `insert_pending_received_eth_tx` explicitly permits receipt before validation but requires validation before an executed status. That distinction supports retaining observations without granting authority. [Newer sender DAL][zk-sender].

`ConsistencyChecker` obtains the settlement transaction from the chain rather than trusting database calldata. This demonstrates independent evidence at an acceptance boundary, not a requirement for a fresh Bitcoin UTXO lookup in Via. [Newer consistency checker][zk-consistency]. `OperatorSigner` implements `EthereumSigner` using local keys or GCP KMS; its `sign_transaction` and `sign_typed_data` are key-use adapters, not business authorization or an N-of-N Bitcoin signer. No equivalent Via withdrawal session was established in those inspected modules. [Newer operator signer][zk-signer].

## Re-fork requirements and remaining choice

The Via contract that must survive consists of expected gross obligations, exact wire references, fee allocation, authorized output scripts, observation-aware duplicate prevention, and round-bound one-time nonce use. Current owners are `WithdrawalSession`, `ViaWithdrawalDal`, `WithdrawalProcessor`, `TransactionBuilder`, and `ViaWithdrawalVerifier`. These should remain recognizable responsibilities even if file ownership changes.

The zkSync adapters are batch and log extraction, protocol-version interpretation, DA retrieval, object-store keys, and DAL integration. Newer `EthSenderDal` provides a lifecycle comparison, not a replacement database schema. `EthereumSigner` cannot carry MuSig2 session semantics. The L2 withdrawal event and its ordering depend on system contracts and VM/protocol versions; a successful Rust port alone does not preserve them.

A re-fork needs preserved examples for repeated recipients, ten-byte references, nonzero fees and rounding, zero or dust net payouts, bridge-address recipients, no-change transactions, actual vout, txid byte order, false prevouts, duplicate observations, re-inclusion, competing attempts, and restarts after nonce publication or signature creation. Golden transaction and sighash bytes matter more than matching Rust names. These are evidence requirements; no new experiment or test ran for this note.

The readiness recommendation below chooses engineering defaults for these questions without treating
them as accepted policy. Parent-content availability, conservative recovery, and activation remain
explicit tradeoffs. The accepted option A and existing fee policy remain unchanged.

## Complete import and a fixed authorization boundary

Further source inspection on 2026-09-24 supports the recommendations below.
They remain proposals, not accepted schema, availability, or rollout decisions.

### Complete expected import is a different result from an empty vector

Prefer preserving expected facts once through an authoritative import, then loading those facts
at signing. Reconstructing origins from DA and L2 on every attempt is an alternative, but adds
repeated source reads and availability dependencies without establishing canonical-at-signing validity.
sBTC's [`fetch_all_reports`][sbtc] provides the narrower precedent: every qualified request must
have a stored report, and missing data aborts construction rather than silently shrinking the set.

A suggested `CompleteWithdrawalBatch` success value carries the batch/source identity,
DA content identity, protocol/decoder version, source range, completeness counts, and requests.
An empty request list is success only when complete source material establishes zero requests.
Unavailable material, incomplete extraction, and inconsistent facts need distinguishable errors.
Only successful complete import may commit the expected facts and import-complete marker together.
No valid subset should become a complete batch after another required message fails.

The import contract must bind full transaction/log origins to the relevant DA messages.
Count equality and matching amounts alone do not distinguish two equal-value events.
Full-width amount checks must precede narrowing, and the existing unit conversion must remain
explicit. Missing message hashes and out-of-range indexes are not successful empty extraction.
If the available sources cannot establish an unambiguous association, preserve that evidence limit
and keep the batch ineligible. A complete Rust result is not proof that a remote source supplied
the entire historical domain.

Expected records should retain full origin identity, gross satoshis, destination script, exact
ten-byte wire reference, and source provenance. The short reference is a lookup aid, not permission
to select the first full-origin match. Identical imports are idempotent. Conflicting imports must
not overwrite established facts. Re-inclusion evidence can change without changing the obligation.

### Reconstruct one proposal without rerunning selection

Prefer a narrow deterministic reconstruction from the fixed ordered inputs and complete selected
requests, reusing the existing fee and output encoding owners. sBTC's `construct_tx_sighashes`
loads all reports, sorts them deterministically, creates the transaction, and retains the resulting
transaction and digests. Its aggregate validation does not silently drop an invalid member.
Its chained signer input and fee rules remain non-transferable.

For Via, proposal construction may remove fee-ineligible requests before proposing a transaction.
Authorization must reject a proposed set requiring such removal. It must not invoke fresh coin
selection, split the proposal into chunks, or choose another input prefix. Shared arithmetic should
produce exactly the expected payouts, metadata, and bridge change, then compare the complete
unsigned transaction and ordered prevouts before deriving local sighashes.

Direct validation is also coherent if it shares that arithmetic and explicitly covers every
transaction field. It avoids allocating a second transaction but has more field-accounting code.
Fixed reconstruction allocates one bounded transaction and reuses encoding. Neither design needs
a second fee policy or a generic validation service.

### Parent lookup and fee availability are separate admission policies

The freshly inspected [pinned BIP341][bip341] `SigMsg` fields commit all input outpoints, amounts,
scripts, sequences, and outputs for ALL without ANYONECANPAY. False supplied amount/script data
cannot yield a valid signature for the different real prevouts. This is not current-unspentness proof.

Three policies remain viable: rely on cryptographically bound supplied prevouts; require complete
parent bytes and verify their txid and selected `TxOut`; or additionally require retrieval from the
configured node. The recommended early check is content consistency, with verified immutable parent
data cached by network and txid. Supplied full parent bytes can satisfy that check without a live RPC.
Mandatory independent retrieval adds a service-source and availability policy, not a BIP341 guarantee.
Failure to obtain material required by the selected policy defers authorization rather than proving
an invalid withdrawal. Even verified parent bytes prove neither inclusion nor current unspentness.
No policy is accepted here, and none introduces fresh `gettxout`.
A withdrawal-payment observation store does not automatically cover all bridge input parents,
including funding and change outputs. Reusing it as a parent cache requires an explicit coverage
and immutable-content contract. Do not turn the accepted expected/payment separation into a
general UTXO index merely to avoid an RPC.

The existing fee comparison in `WithdrawalSession::_verify_withdrawals` obtains `get_fee_rate(1)`
and permits a one-sat/vbyte difference. Preserve that policy. An unavailable estimate must not
become a zero or invented stale estimate. Bind the accepted rate and arithmetic to the attempt.
An identical retransmission is not a new proposal because the live estimator has moved.
Checked numeric comparisons should preserve the intended tolerance without narrowing arbitrary
fee rates. Authorization lifetime and retry rules belong to the shared signing-round contract.

The current [`get_fee_rate`](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/client/mod.rs)
can use configured external HTTP estimators when no RPC estimate is obtained. Its returned numeric
rate does not identify the source. The existing tolerance does not require coordinator and verifier
to use the same estimator. Recording source provenance may help diagnosis; requiring source equality
or rejecting the configured fallback would be a separate policy change, not preservation of the
accepted fee policy.

### Cutover cannot grant authority to uncleared legacy rows

Schema migration does not prove that an old mixed-purpose row contains an original gross amount.
Legacy rows need source-backed expected reconstruction or must remain ineligible.
Retain payment and attempt evidence through invalidation; clearing an association is not proof
that another payment is safe. [Observation and release research](withdrawal-intent-and-observation.md#holds-late-payments-and-release-evidence)
describes the distinct hold and fulfillment conclusions.

A small dependency stack can first establish complete immutable import, then cut the parser,
watcher, DAL, and eligibility readers over to separate observations. Fixed-proposal authorization
and the authenticated durable signing lifecycle consume that contract. Helper or schema PRs can
merge earlier only if they grant no new signing authority. An observation-only writer alongside
old eligibility that ignores observations is not a safe enabled intermediate version.

Historical reconciliation, invalidation/recovery policy, coordinated activation, and permission
to resume signing remain separate prerequisites. Rollback must not silently restore an unsafe
mixed writer or bypass the disabled-signing state.

Future local proof should cover incomplete versus genuinely empty import, short-reference
ambiguity, fee-ineligible members, false prevouts, estimator failure, conflicting observations,
and restart after a signature may have escaped. Historical clearance requires separately scoped
source evidence. No new runtime or migration proof ran for this research.

## Implementation-readiness recommendation

The approved execution unit is the whole withdrawal workstream, including its minimum shared signing
contract. Design expected obligations, observations and attempts as foundational requirements rather
than additions to the mixed legacy model. Read the affected owners and consumers before changing them;
propagate the target design through types, callers, tests, examples and rationale. Deliver incrementally
inside that integrated design, with one signing activation boundary. Review partitions are not separate
product scopes or independently safe releases. Unrelated proof, deposit, upgrade and monitoring choices
do not block this work merely because they share the design catalogue. Material policies below remain
explicit decisions; approval of the workstream does not silently settle them.

### Import an obligation with its origin, not just matching values

Suppose two L2 transactions withdraw the same amount to the same recipient. Their amount and address
cannot tell the importer which full transaction/log origin belongs to each DA message. Retain the
existing DA-plus-L2 source path, but make successful import mean that every required association is
accounted for. Re-fetching the same sources at every signature adds availability cost without changing
the selected trust model.

The inspected Via contract provides a concrete association path. `L2BaseToken::withdraw` divides
`msg.value` by `10_000_000_000` in `uint256`, sends the resulting message through `L1Messenger`, then
emits `Withdrawal` with the original L2 amount. `L1Messenger::sendToL1` records a service log containing
the transaction's batch position, messenger sender, calling contract, and message hash; it also emits
`L1MessageSent`. These are distinct from the ordinary withdrawal event's transaction log index.
[Token emission][token-emission] · [Messenger emission][messenger-emission].

The existing receipt type exposes full transaction and block identity, `l1_batch_tx_index`, ordinary
`logs`, and `l2_to_l1_logs`. Enumerate withdrawal logs or the selected blocks' transactions to obtain
transaction hashes before fetching receipts: a DA batch transaction position alone is not a receipt
lookup key. Within the complete selected batch, compare the serialized DA log and message hash with
the corresponding receipt log, including shard, service flag, transaction position and ordering,
expected `L1Messenger` sender and `L2BaseToken` key. Matching only position/key/value is insufficient.
Associate the ordinary withdrawal event using the contract's emission order within that transaction.
Preserve multiplicity for repeated identical messages. A message-hash map may share immutable message
bytes, but must not deduplicate obligations. Unsupported contract/protocol layouts remain ineligible
rather than guessing an order.
[Receipt fields][receipt-origin] · [DA log serialization][da-log] · [Messenger emission][messenger-emission].

The inspected log endpoint orders results by block and event index and returns a limit error for an
oversized multi-block range, rather than deliberately returning a successful truncated page. This
supports deterministic bounded-range retrieval; it does not prove that an arbitrary remote endpoint
is honest or historically complete. Receipt and DA consistency checks remain necessary, as do explicit
range completion and source-version identity. [Log query][log-query] · [Endpoint limit handling][log-limit].

Origin integrity is part of the selected authority, not harmless labeling. A changed full transaction
hash or event index changes the obligation and its ten-byte reference even when amount and destination
are equal. Matching receipt content to DA content does not independently authenticate that origin.
The authority contract must therefore trust the selected receipt source for origin within the recorded
domain, or require independently verified origin evidence. Recording provenance does not remove this
trust assumption.

Recommended result: `CompleteWithdrawalBatch`, produced only after all required checks, containing
batch/source identity, protocol version, DA content identity, covered range, and expected requests.
Use errors for unavailable, incomplete, or inconsistent evidence; add a private error enum only where
callers need different actions. A complete empty batch is valid. An incomplete nonempty subset is not.
Commit the result's facts and completion marker together using the existing verifier DAL transaction.

Use full-width division before checked conversion to satoshis. Preserve the contract's floor division;
requiring a zero remainder would change its accepted semantics. Preserve the original event amount in
source evidence and compare the full-width quotient with the DA amount before narrowing. This is a
compatibility requirement from the contract, not a new amount policy. [Token emission][token-emission].

### Keep three identities and two amounts explicit

Recommended storage in the existing verifier DAL:

| Fact | Identity and content | Writer and conflict rule |
| --- | --- | --- |
| Expected obligation | Network/L2-chain context, full L2 transaction hash and event index; destination script, gross satoshis, exact ten-byte wire reference, source provenance | Complete importer only. Equal replay is a no-op; differing facts for one origin preserve a conflict and prevent eligibility. Index short references without treating them as unique full origins. |
| Observed output | Canonical Bitcoin transaction ID and actual `vout`; script, net satoshis, exact observed reference | Bitcoin observation path. No foreign key requiring an expected request. Compare immutable content on replay; never overwrite expected facts. |
| Inclusion evidence | Transaction identity plus block hash/height and current accepted inclusion state | Chain observation/invalidation path. Re-inclusion changes evidence, not output identity or expected amount. |
| Signing attempt | Unique persisted round identity plus exact proposal, ordered prevouts, request membership, fee interpretation, wallet/key context and public signing state | Shared admission/signing owner. Equal delivery returns its recorded outcome; conflicting content cannot reuse the identity. |

Keep `WithdrawalRequest` expected-only. Extend the parsed-payment representation with actual output
position rather than convert it into a request with missing origin. Candidate DAL names
`insert_expected_withdrawals`, `record_withdrawal_observations`, and
`reconcile_withdrawal_observations` describe these separate responsibilities; they are proposed names,
not existing interfaces. Reuse the bridge-transaction parent when it can satisfy actual txid identity.
The public indexer and verifier/broadcast consumers must migrate together with shared parser types.
No generic event store or universal UTXO index is needed.

Persist the full origin as fixed-width bytes or an explicitly lossless encoding, never a display
formatter. Prove round-trip identity and derive the short reference from those same bytes. Historical
rows without recoverable full origins require source-backed reconstruction or remain ineligible.
Separate expected and observed facts do not mandate a particular new table name: an additive expected
table or a correctly rebuilt existing relation can satisfy the same clean-cutover contract.

This preserves the sBTC observation-first mechanism, not its Stacks identifiers. Its migration gives
outputs a `(txid, output_index)` key and deliberately omits the request foreign key. NBXplorer separately
stores transaction outputs and block inclusion. Neither implementation grants Via fulfillment merely
because an output was inserted. [sBTC output schema][sbtc-output-schema] ·
[Detailed storage comparison](withdrawal-intent-and-observation.md#evidence-from-comparable-implementations).

The distinction between `compute_ntxid` and `compute_txid` is settled by the installed Bitcoin library:
the former clones the transaction and clears input `script_sig` and witness before computing its ID.
The ordinary txid already excludes witness but retains `scriptSig`. They agree for transactions with
empty input scripts, not universally. Key payment observations by actual txid; do not globally change
all inscription identities to accomplish that local requirement. Historical identity reconciliation
must preserve aliases only where required for identified stored history, not as a new public API shim.
[Bitcoin 0.32.7 transaction implementation][bitcoin-txid].

### Hold uncertainty without calling it a successful payment

Recognized bridge-spend evidence may arrive before the expected obligation. Retain it, and block a
plausibly affected obligation from a second authorization until reconciliation. Verify bridge-input
provenance; arbitrary third-party metadata must not freeze requests. Unavailable provenance needs a
durable pending-evidence/replay path or a non-advancing scan boundary, not permanent classification as
an unrelated transaction. Use the existing watcher/indexer ownership rather than adding a service.

Local MuSig2 records are not a complete census of bridge payments: the wallet also has a governance
script path. Consequently N-of-N key-path participation is not a reason to ignore independent bridge
payment evidence. Excluding such evidence would require a narrower, explicitly accepted operating
domain and a safe transition around every out-of-domain spend. [Wallet spending paths][musig-guide].

Fulfillment requires complete transaction context, unambiguous reference-to-output mapping, expected
script, correct net value under the existing fee interpretation, and the selected inclusion evidence.
The absence of a local broadcast is not disqualifying if those same facts can be independently
established. Otherwise keep a hold. Underpayment, ambiguous short references, and multiple candidate
payments do not authorize automatic top-ups or aggregation. Preserve bridge-recipient, no-change,
zero-net and repeated-recipient cases in compatibility fixtures rather than inventing new fees or
output rules. [Fee owner][fees] · [Hold and fulfillment research](withdrawal-intent-and-observation.md#holds-late-payments-and-release-evidence).

Recommended first-release recovery policy: no automatic post-signature release and no new replacement
engine. Return cached public responses or rebroadcast identical finalized transaction bytes. A timeout,
mempool loss, restart, or reorg does not authorize another payment. Retiring a round releases a request
only if trusted durable state proves no share could have escaped and no independent payment evidence
holds it. This deliberately trades availability for a smaller safe recovery surface. A future
conflicting-spend release mechanism needs its own reorg-risk contract; it is not necessary to write
this conservative first implementation.

### One local admission boundary, one fixed authorization object

Use a short wallet-scoped database transaction shared by importer, observer, invalidator, and attempt
admission. Acquire its lock before eligibility reads; after waiting under READ COMMITTED, read in a new
statement. Record selected requests and inputs before releasing the lock. Network I/O and nonce
consumption stay outside retryable database closures. This serializes cooperating local writers,
including observation-before-request, but reserves neither Bitcoin UTXOs nor another verifier's DB.
[PostgreSQL locking][pg-locking] · [Atomic admission comparison][lnd-admission].

LND's concrete pattern is `PaymentControl::RegisterAttempt`: serialize `HTLCAttemptInfo`, enter
`kvdb.Batch`, load the payment, call `Registrable`, check in-flight amounts, then write the attempt.
Copy the check-and-record transaction boundary, not Lightning MPP accounting or its failure-based
repayment rules. [RegisterAttempt, lines 316–456][lnd-admission].

The following is a contiguous source quotation from that method, lines 330–346; the remainder of the
function is omitted. It shows why admission belongs **inside** the database batch, rather than in a
preflight check followed by a separate write. Source: Lightning Labs and The Lightning Network
Developers, copyright 2015–2022, [MIT license at the same revision][lnd-license]. This quotation is
evidence for the pattern, not proposed Via code.

```go
	var payment *MPPayment
	err = kvdb.Batch(p.db.Backend, func(tx kvdb.RwTx) error {
		prefetchPayment(tx, paymentHash)
		bucket, err := fetchPaymentBucketUpdate(tx, paymentHash)
		if err != nil {
			return err
		}

		payment, err = fetchPayment(bucket)
		if err != nil {
			return err
		}

		// Check if registering a new attempt is allowed.
		if err := payment.Registrable(); err != nil {
			return err
		}
```

Authorization should load the complete expected set once and reconstruct **one** transaction from the
fixed ordered inputs and requests. Reuse `WithdrawalFeeStrategy` and the builder's encoding/arithmetic
owners, extracting fixed construction from selection/chunking. Reject a fixed set that would require
dropping a request. Compare all transaction fields and stored derived fields, then compute messages
with `get_tr_sighashes`. Reuse existing candidate outpoint-order, duplicate, script and parent-content
checks where compatible; do not reimplement the same checks in another service. [Builder][builder] ·
[Fee owner][fees] · [sBTC fixed construction][sbtc].

Preserve the selector's existing prefix-sufficiency rule within the supplied input order: a proper
prefix must not already fund the required total if the proposal includes further inputs. Otherwise
extra inputs can change fees borne by requests. Check this with a running sum, not fresh coin selection
or a claim of globally optimal inputs. Preserve the current `value >= fee_per_user` boundary; changing
it to `>` is a zero-net policy change, not an arithmetic cleanup. [Input selection][input-selection] ·
[Fee owner][fees].

Choose verified complete parent bytes for early error detection; source them from existing RPC,
provided content or an immutable cache without requiring one provider. Validate txid and selected
output; unavailable required bytes defer authorization. This is an availability tradeoff above the
BIP341 signature commitment, not proof of inclusion or current unspentness. Preserve configured fee
fallback and the existing numeric tolerance; bind the accepted rate to the attempt so a retransmission
does not become new intent when the estimator moves. [BIP341][bip341] ·
[Parent and fee alternatives](#parent-lookup-and-fee-availability-are-separate-admission-policies).

The authorization result needs a unique attempt identity **and** a commitment to its content.
Recommend a cryptographically random coordinator-issued round ID, persisted by each verifier with
its wallet context and exact content before publishing a nonce. Reject reuse with conflicting content
and stale or retired rounds. A proposal hash alone identifies content, not a new round; coordinator
database durability is not a cryptographic prerequisite for this verifier-local rule. Bind network,
wallet key order/tweak, exact transaction, ordered prevouts, expected-fact snapshot, fee interpretation
and local messages. Freeze participant/input nonce ordering before signing. Cover session creation,
nonce/share mutations, and all session/transcript reads—not an arbitrary subset of routes.
Authenticated contributions bind principal, operation/target, audience, round and exact content.
The initial session read discovers a round through an authenticated response; it cannot require advance
knowledge of an unknown identifier. A versioned exact-body envelope using existing identities is the
recommended profile; a general RFC9421 framework is unnecessary. An enabled signer cannot consume
unbound contributions or unauthenticated session responses.

Recommend public-state persistence without secret-nonce persistence initially. Before consuming any
input nonce, commit the attempt's may-have-signed state under the same admission boundary. Persist the
complete public response before sending it. After a crash, return an already complete cached result or
retire the incomplete round and retain its holds. No recomputation of a missing share from a recreated
nonce; no partial batch publication. BIP327's one-time nonce requirement and CometBFT's saved-result
retry pattern inform different parts of this design. CometBFT's timestamp exception and vote-extension
re-signing do not transfer. [BIP327][bip327] · [CometBFT][comet] ·
[Shared signing research](coordinator-request-authentication.md).

sBTC provides a concrete authorization handoff: its presign path writes Bitcoin sighash decisions;
`validate_bitcoin_sign_request` subsequently loads `will_sign_bitcoin_tx_sighash` and rejects unknown
or disallowed hashes and signature types inconsistent with the approved prevout. Copy that exact
approved-input boundary, not its WSTS protocol or Stacks policy. [Persisted decisions][sbtc-decisions] ·
[Signing check][sbtc-sign-check].

Two narrower recovery alternatives are valid under stronger local invariants, but are not the selected
initial profile. Re-signing the identical authorized transaction with fresh nonces in a new round need
not create a second payment; it must preserve transaction, prevouts and authorization context and
must not become permission for changed outputs or inputs. Alternatively, persisting a complete signed
batch before **any** share can escape can avoid a separate may-have-signed write. That requires proof
over every export path, uncertain commits, old-process publication and restart behavior. The recommended
early marker costs one durable transition and can strand work whose shares never escaped, but keeps
nonce consumption outside database retries and makes conservative crash recovery easier to establish.

Restoring an old verifier database can erase both rounds and payment evidence. Random IDs, locks and
cached responses cannot detect lost history by themselves; a restored signer must remain disabled
until its trusted recovery/activation procedure establishes the required history.

### One implementation workstream, three review partitions

1. **Complete obligations and independent observations.** Change the withdrawal client/types,
   verifier DAL/migrations, shared parsed-payment representation, verifier watcher, broadcast
   bookkeeping and indexer consumers. Implement complete import, conflict-safe immutable facts,
   actual txid/vout, observation-first reconciliation and eligibility. Remove the mixed writer and
   lossy observation-to-request conversion. Include the local shared writer lock needed by admission.
2. **Fixed proposal authorization.** Extract deterministic fixed construction in `via_musig2` and
   update `WithdrawalSession` to load authoritative facts, preserve fee behavior, validate complete
   value/output equations and derive the exact signing messages. Reconcile existing candidate work
   before extraction. Do not pull unrelated parser refactoring into this change.
3. **Durable signing admission and round binding.** Wire coordinator/verifier attempts, membership,
   authenticated contribution identity, public results, retirement and restart into the existing
   signing/session owners. Make observation/invalidation races obey the same local admission boundary.
   Remove old unbound caller paths; no unsigned or old-round fallback.

These are dependency-based review partitions within one implementation, not separately commissioned
slices or enabled releases. The integrated candidate must satisfy the complete lifecycle. Helper-only
code may merge without granting authority; a partial data-path cutover must not be deployed with old
eligibility. Stop-and-cutover is recommended over mixed signer versions. Deployment, historical
backfill and signing resumption require their own authorization and proof.

The initial implementation excludes automatic post-signature release, a replacement fee engine,
persisted secret nonces, high-availability coordinator failover, a general authentication framework,
fresh `gettxout`, canonical-at-signing assurances, changes to fee economics, and a re-fork. These
exclusions are proposed scope choices, not omitted safety guarantees: uncertainty remains held or
unavailable rather than silently proceeding.

### What comes from zkSync, and what must remain Via-owned

The newer local checkout remains pinned to `ff5f519b11cff863edcfa0f75af10fea113806b0`.
`EventsDal::save_user_l2_to_l1_logs` requires transaction and within-transaction ordering;
`EthSenderDal` separates logical transactions from attempts and pending receipt from execution.
Use those concrete patterns. `OperatorSigner` remains an Ethereum key-use adapter, not a Bitcoin
obligation authorizer or MuSig2 round manager. [Ordered upstream logs][zk-log-order] ·
[Sender lifecycle][zk-sender] · [Signer][zk-signer].

Keep DA retrieval, batch/receipt extraction, protocol interpretation, DAL connection and task wiring
as the upstream-facing adapters. Expected gross obligations, ten-byte references, actual Bitcoin
outputs, fee equations, holds and one-time round behavior remain Via-owned. No inherited upstream
production change is necessary merely to name these seams. A future re-fork must replay exact
transaction/sighash fixtures plus source association and lifecycle transitions; matching Rust names
or compiling against new RPC types is insufficient.

### Finite decisions and evidence gates

Whole-workstream execution is approved. Remaining policy choices are listed separately from engineering
work and release evidence; these are not one user question per field.

| Category | Proposed answer / remaining evidence | Blocks |
| --- | --- | --- |
| Authority tradeoff | Retain complete DA-plus-L2 checked facts once; trust the selected sources for both content and full origin within their recorded protocol/history domain, or independently verify origin. No canonical-at-signing claim. | Accept before implementing the authority contract. |
| Availability tradeoff | Require verified parent content, not a particular retrieval source; defer when required content or a usable fee rate after existing fallback rules is unavailable. | Accept before implementing the admission contract. |
| Recovery tradeoff | Persist public state, retire incomplete crash rounds, keep uncertain payments held; no automatic post-signature release or replacement initially. N-of-N restart can stop progress. | Accept before implementing lifecycle behavior. |
| Engineering choices | Full-origin identity, actual outpoint, checked arithmetic, complete import, exact reconstruction, common writer locking, typed errors where acted upon, atomic public-response persistence. | Implement and prove locally; no separate user naming/schema questionnaire. |
| Existing history | Reconstruct expected facts and reconcile observed payments, legacy identities, prior signed attempts and outstanding holds for the chosen activation domain. Unknowns remain ineligible. | Activation, not implementation of fail-closed behavior. |
| Ongoing invalidation | Demonstrate that source removal/reorg, stale restoration and changed wallet context stop new authority while retaining payment/attempt evidence. | Enabled release; source implementation includes the fail-closed paths. |
| Fulfillment policy | Preserve the existing watcher cutoff for ingestion, but do not equate it with settlement finality. Recommend fulfillment only after complete payment validation and an explicitly accepted Bitcoin inclusion policy; otherwise retain a hold. No numeric depth or automatic release rule is selected here. [Watcher boundary][watcher-boundary]. | Material policy choice before enabling fulfillment; fail-closed storage and authorization can be implemented without inventing it. |
| Execution permission | One end-to-end withdrawal implementation with independent research/reviews is approved; unrelated catalogue decisions need not finish first. | Policy-dependent behavior still needs the stated choices. Live migration, publication, deployment and signing activation remain separately gated. |

Focused implementation proof must cover complete-empty versus incomplete import; repeated equal
events and ambiguous references; undersized L2 messages and missing log topics returning malformed/
incomplete errors rather than panics or complete-empty success; lossless full-origin round trips and
changed-origin rejection; full-width conversion and contract rounding; both observation/import orders; replay/conflicts; every
actual payout position; unchanged fee and transaction bytes, including redundant input-prefix rejection; mutation of
each authorization input; parent/estimator unavailability; concurrent observation/admission and competing
attempts; crashes before/after nonce consumption and response persistence; stale/retired contribution
rejection; and invalidation without repayment. Parser fixtures must include no-change, bridge recipients
and zero-net cases without silently choosing a different acceptance policy. Use disposable local fixtures,
then separately scoped historical evidence for activation. No runtime, PostgreSQL concurrency, signing
or deployment proof is claimed by this research.

Common-path cost is one complete import per batch, observation storage per actual output, short local
admission transactions, one bounded reconstructed transaction, and public transcript storage proportional
to inputs times participants. Parent fetching can share one verified parent for several input outpoints.
Do not add persistent caches or fine-grained locks before measuring need. The extra transaction allocation
is intentional encoding reuse; fresh coin selection and repeated full-source reconstruction are not.

### Source-linked implementation rationale

At governing types and critical shared functions, document the local invariant first, then the specific
external mechanism that informed it and any important difference. Use a commit-pinned GitHub URL with
the source file, symbol or line range; use a BIP/RFC section for protocol requirements. A repository
homepage alone is not evidence. One concise citation at the owner is preferable to repeating it across
callers. Keep the complete comparison here, linked from the owner when useful.

For example, the observed-output relation can explain that payments may arrive before expected
requests, cite [sBTC's output schema][sbtc-output-schema], and state that recording an output grants
neither signing authority nor fulfillment. Signing persistence can cite [LND admission][lnd-admission]
or [CometBFT saved results][comet] only for the mechanism actually reused, without importing Lightning
repayment rules or CometBFT re-signing exceptions. Preserve attribution/license obligations if source
code is copied. Comments explain durable behavior and why, not the consultation or implementation
history; types, constraints and observable tests enforce the invariant.

[token-emission]: https://github.com/vianetwork/era-contracts/blob/a14df38896f6e7eafc45e65eefdb63bf3cacebe4/system-contracts/contracts/L2BaseToken.sol#L74-L108
[messenger-emission]: https://github.com/vianetwork/era-contracts/blob/a14df38896f6e7eafc45e65eefdb63bf3cacebe4/system-contracts/contracts/L1Messenger.sol#L118-L158
[receipt-origin]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/types/src/api/mod.rs#L219-L266
[da-log]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/types/src/l2_to_l1_log.rs#L10-L59
[log-query]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/dal/src/events_web3_dal.rs#L68-L100
[log-limit]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/api_server/src/web3/namespaces/eth.rs#L830-L865
[sbtc-output-schema]: https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/migrations/0014__add_bitcoin_tx_outputs_.sql#L1-L19
[bitcoin-txid]: https://docs.rs/bitcoin/0.32.7/src/bitcoin/blockdata/transaction.rs.html
[pg-locking]: https://www.postgresql.org/docs/17/explicit-locking.html
[lnd-admission]: https://github.com/lightningnetwork/lnd/blob/d72a3aaf261e278fa4aad5be4453df2f74ab50ee/channeldb/payment_control.go#L316-L456
[zk-log-order]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/dal/src/events_dal.rs#L119-L179
[watcher-boundary]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_btc_watch/src/lib.rs#L119-L149
[lnd-license]: https://github.com/lightningnetwork/lnd/blob/d72a3aaf261e278fa4aad5be4453df2f74ab50ee/LICENSE
[input-selection]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/via_musig2/src/utxo_manager.rs#L81-L110
[sbtc-decisions]: https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/src/transaction_signer.rs#L478-L495
[sbtc-sign-check]: https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/src/transaction_signer.rs#L1218-L1245

[musig-guide]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/docs/via_guides/musig2.md
[session]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_verifier_coordinator/src/sessions/withdrawal.rs
[watcher]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_btc_watch/src/message_processors/withdrawal.rs
[dal]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/verifier_dal/src/withdrawals_dal.rs
[siblings]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/.github/sibling-paths.yml
[wire]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/indexer/withdrawal/mod.rs
[builder]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/via_musig2/src/transaction_builder.rs
[fees]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/via_musig2/src/fee.rs
[verifier]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_verifier_coordinator/src/verifier/mod.rs
[sbtc]: https://github.com/stacks-network/sbtc/blob/ee7ec0076f610e7bf96f0893df80b4a9a0dcbcef/signer/src/bitcoin/validation.rs
[tbtc]: https://github.com/keep-network/tbtc-v2/blob/40a11d1dcdcf82d3962e430067cfe3f97be81483/solidity/contracts/bridge/Redemption.sol
[comet]: https://github.com/cometbft/cometbft/blob/f4d73cd5a091a997d1f040850710b6937e650125/privval/file.go
[zk-sender]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/dal/src/eth_sender_dal.rs
[zk-consistency]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/consistency_checker/src/lib.rs
[zk-signer]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/operator_signer/src/lib.rs
[bip327]: https://github.com/bitcoin/bips/blob/eba8e50cb66d436c65c6bc8b0a175b643effe9d3/bip-0327.mediawiki
[bip341]: https://github.com/bitcoin/bips/blob/eba8e50cb66d436c65c6bc8b0a175b643effe9d3/bip-0341.mediawiki
[bip174]: https://github.com/bitcoin/bips/blob/master/bip-0174.mediawiki
