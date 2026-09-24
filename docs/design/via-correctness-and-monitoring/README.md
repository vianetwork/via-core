---
status: pending-design
---

# Via correctness and monitoring: design and rationale

This is the permanent repository-local record of the bridge and monitoring design: what the research
established, which alternatives matter, what is recommended, and which decisions have been accepted.
It remains useful after implementation because it preserves why a contract was chosen. It is not a
release plan or a claim that the described behavior is deployed.

The linked research notes retain detailed source traces and pinned comparison implementations. ADRs
own accepted consequential decisions. This record connects them through the situations the design must
handle; it does not replace their evidence or create a second task tracker.

## Status, ownership, and evidence

The accepted boundaries below remain fixed. All 41 identified choices in the topic tables remain
**pending**; a recommendation is not acceptance. Their identifiers provide traceability, not priority
or implementation order. Reading or updating this record authorizes no migration, funded probe,
shared-state operation, deployment, or signing resumption.

Maintain disclosure-safe design reasoning here. Keep restricted findings, private candidate reviews,
operational artifacts, and their supporting evidence in the approved private version-controlled store.
The immutable earlier private capture is historical evidence. The private repository's live entry
point still describes itself as maintained; retiring that claim is a pending documentation handoff,
not a completed migration. This repository-local record owns the disclosure-safe design status;
reconcile any disagreement before a dependent decision. New restricted evidence belongs in private
supplements rather than a second live full record. Promote material here only after disclosure
review. This document contains no private store address, access details, or incident-specific diagnosis.

The source research compares Via revision `8a49f355bfe31720f21b8181db471243709194e5` with a newer
local zkSync revision, `ff5f519b11cff863edcfa0f75af10fea113806b0`. That is not a claim about the latest
upstream or deployed code. Recheck the relevant owners when implementing a choice. A comparison
project demonstrates a mechanism, not that its trust assumptions or policy transfer to Via.

## Find the relevant contract

| Before changing | Read |
| --- | --- |
| Withdrawal import, observation, reconciliation, or transaction authorization | [Pay the intended obligation once](#pay-the-intended-obligation-once) |
| Coordinator authentication, signing rounds, retries, or secret nonce handling | [Retry signing without repeating a secret operation](#retry-signing-without-repeating-a-secret-operation) |
| Proof results, stored verdicts, votes, or historical trust | [Say what verification actually established](#say-what-verification-actually-established) |
| Deposit decoding, carrier selection, conversion, or historical replay | [Make one deposit decision from one transaction](#make-one-deposit-decision-from-one-transaction) |
| Governance proposal decoding, approval, upgrade execution, or restart | [Execute the approved upgrade](#execute-the-approved-upgrade) |
| Inscription observations, exported metrics, dashboards, or alerts | [Observe inscriptions without delaying the sender](#observe-inscriptions-without-delaying-the-sender) |
| End-to-end bridge probes or their spending lifecycle | [Measure real bridge completion with bounded spending](#measure-real-bridge-completion-with-bounded-spending) |
| Heartbeats, outside monitoring, maintenance, or notification delivery | [Detect failure of monitoring itself](#detect-failure-of-monitoring-itself) |

## Accepted boundaries

- **Observation cannot create a withdrawal obligation.** [ADR 0004](../../adr/0004-separate-expected-withdrawals-from-observed-payments.md)
  keeps expected gross amount, destination, and identity separate from observed payment value and
  chain evidence, including payment-first arrival. Reconciliation does not itself authorize payment.
  This does not select the schema, signing algorithm, hold-release policy, or current-chain assurance.
- **Invalid deposit address lengths reject the affected encoding without stopping scans.**
  [ADR 0002](../../adr/0002-reject-invalid-deposit-address-lengths.md) defines that boundary; it does
  not define every malformed-carrier coexistence case.
- **Preserve the accepted OP_RETURN interpretation.** [ADR 0003](../../adr/0003-decode-op-return-deposit-pushes.md)
  owns first-push decoding, reserved prefixes, and ignored trailing data. Its historical census has
  a bounded scope, not blanket clearance for later policy changes.
- **Recognized competing deposit carriers yield no deposit.**
  [ADR 0006](../../adr/0006-reject-recognized-deposit-carrier-coexistence.md) rejects recognized witness
  plus unreserved OP_RETURN coexistence even when receivers match. It changes deposit eligibility,
  not unrelated withdrawal or system messages. Malformed-message recognition remains unresolved.
- **Observation cannot delay inscription processing.**
  [ADR 0001](../../adr/0001-isolate-btc-inscription-observability.md) requires separately owned bounded
  execution and resources, current and fresh observations, and unavailable evidence distinct from
  healthy zero. It does not choose a metric representation or prove activation.

These are accepted planning contracts, not assertions about current implementation. Preserve the
existing withdrawal fee policy and the selected signing guarantee; ADR 0004 adds neither a mandatory
fresh `gettxout` call nor a guarantee that preserved facts remain canonical when signing begins.

## Pay the intended obligation once

Suppose a request owes a recipient 10,000 satoshis before the existing fee allocation. A watcher sees
a Bitcoin output before the request importer catches up. The output's value cannot tell the importer
what the original gross request was. Conversely, not yet knowing the request cannot make the output
irrelevant to preventing a second payment. These are illustrative amounts, not fee parameters.

An **expected withdrawal** is the retained obligation. An **observed payment** is evidence of an
actual output. Its **outpoint** is the ordinary Bitcoin transaction ID and output index; its block
inclusion is a different fact. A **hold** prevents another authorization while uncertainty remains.
**Fulfillment** says the right recipient received the right value under the selected settlement policy.
A hold is not fulfillment, and a missing local transaction is not proof that it can never confirm.

The [storage research](../../research/withdrawal-intent-and-observation.md) establishes a concrete
precedent: sBTC stores withdrawal outputs without requiring the corresponding request to exist yet.
NBXplorer separates output identity from block inclusion. LND checks payment admission and records
an attempt within one database batch. These mechanisms support separate facts and atomic local
admission; they do not reserve Bitcoin inputs against other spenders or select Via's confirmation policy.
The [authorization research](../../research/withdrawal-authorization-and-refork.md) explains sBTC's
request-backed transaction construction and tBTC's checking of observed payments against obligations.
Their fee rules and wallet-input assumptions are not interchangeable with Via's.

The recommendation is to preserve complete expected facts, reconcile independent observations, and
authorize one fixed transaction using the existing fee and encoding owners. This costs durable
reconciliation and admission state, but avoids using a payment observation as the amount authority.
Reconstructing the transaction and directly checking every field are both viable; rerunning coin or
request selection during authorization changes the proposal rather than verifies it.

The [implementation-readiness recommendation](../../research/withdrawal-authorization-and-refork.md#implementation-readiness-recommendation)
owns the integrated proposal, source evidence and finite policy inventory. Whole-workstream
implementation with independent research/reviews is approved; material policies remain open.
Live migration, publication, deployment and signing activation remain separately gated.

| Choice | Alternatives and recommendation; reason it remains open |
| --- | --- |
| W1 — Expected-fact authority | Retain checked request facts once, or reconstruct them at every signing attempt. Prefer a complete authoritative import with explicit completion evidence, including genuine empty batches. The source, supported history, and invalidation policy must be selected. |
| W2 — Obligation and output identity | Reuse existing transaction ownership with an explicit output relation rather than overload one amount column. Preserve full request origin, exact wire reference, actual txid/vout, and separate inclusion evidence. Exact keys and conflict handling need agreement. |
| W3 — Hold versus fulfillment | Reconcile independently observed payments, or retain session-owned settlement while making admission consult those observations. Prefer holds for credible unresolved payment evidence and fulfillment only for an unambiguous correct payout. Select provenance, fee context, and confirmation requirements. |
| W4 — Safe retry after uncertainty | Retain holds until an explicit exclusion policy is satisfied, or authorize recovery with stated residual risk. Prefer retaining evidence; timeout, restart, or local mempool absence alone cannot release it. A deep reorg can invalidate exclusion evidence. |
| W5 — Concurrent admission | A shared wallet-scoped admission transaction is simpler than fine-grained locking, which must also cover absent expected rows. Prefer the shared boundary for import, reconciliation, invalidation, and admission; persist possibly signed attempts before shares escape. Select locking and recovery ownership. |
| W6 — Fixed transaction authorization | Deterministic reconstruction reuses fee and encoding code but constructs another bounded transaction. Direct field/equation checks avoid that construction but require complete accounting. Prefer reconstruction without reselection, dropped requests, or opportunistic reordering. |
| W7 — Parent and fee evidence | Supplied prevouts, verified parent bytes, and live RPC retrieval have different early-detection and availability costs. Prefer verified content without requiring one retrieval source. Preserve fee-estimation policy; define unavailable outcomes rather than inventing a zero fee. |
| W8 — History and cutover | Prefer coordinated cutover unless an actual availability requirement justifies a proven mixed-version contract. Reconstruct legacy expected facts from authorized sources or keep them ineligible. Storage migration alone does not establish signing readiness. |

**Proof needed before implementation is accepted:** both arrival orders, identical and conflicting
replays, ambiguous references, repeated recipients at different output positions, txid byte order,
underpayment, late confirmation, two competing attempts, interrupted signing, and reorg invalidation.
Exercise exact recipient, fee, change, metadata, and input mutations against the same fixed proposal.
No local database lock or content check supplies current-chain certainty by itself.

## Retry signing without repeating a secret operation

A signer sends its contribution and loses the response. The coordinator may already have it. Retrying
must not mean using the same one-time secret again. **MuSig2** combines participants' partial signatures;
a **secret nonce** is one-use signing material, not an HTTP retry identifier. A **round** identifies one
attempt, while its **transcript** fixes participants, public nonces, messages, and input context.

The [authentication and signing research](../../research/coordinator-request-authentication.md)
separates request authenticity from signing history. AWS SigV4 and Matrix show explicit operation and
audience binding. The installed musig2 comparison accepts an identical occupied-slot contribution but
rejects a changed one; consuming an in-memory round is not persistence across a process crash.
CometBFT saves its signed result before returning it and distinguishes identical retries from conflicts.
Its consensus-specific timestamp exception is not a safe default for Bitcoin signing messages.

Prefer an explicit signed operation and durable round lifecycle using the existing client, middleware,
handlers, and signer owners. A cryptographically valid HTTP request does not make a signing retry safe;
a durable signing round does not authenticate the HTTP request.

| Choice | Alternatives and recommendation; reason it remains open |
| --- | --- |
| S1 — Signed request meaning | Choose an exact-byte versioned envelope, a complete RFC 9421 profile, or specified canonical encoding. Prefer an envelope binding operation, audience, network, round, and transmitted body, with the authenticated principal propagated to handlers. Proxy and encoding semantics still need selection. |
| S2 — Durable round identity | Persist a wallet epoch and sequence, or an equivalently fenced random identifier. Prefer allocation by the existing coordinator with the fixed proposal and participant context. A proposal hash identifies content, not the attempt or authority after restoring an old database. |
| S3 — Crash persistence | Prefer retaining public results before returning them and retiring an uncertain round rather than recreating its secret operation. Crash recovery, partial multi-input results, storage failure, and rollback protection must be specified; an in-memory consuming API is insufficient. |
| S4 — Retransmission and retention | Return retained results for identical submissions, reject conflicts, and validate a complete batch before mutation. Cache expiry may discard a response but must not make a retired identifier reusable. Choose result retention separately from replay protection. |
| S5 — Invalid contribution | Distinguish unauthenticated malformed traffic from an authenticated invalid contribution to the frozen round. Prefer aborting that exact round while preserving payment evidence; a delayed failure must not reset its successor. Select attribution and recovery rules. |
| S6 — Coherent activation | Transport binding and durable signing are separate boundaries. Migrate each boundary's real consumers together and retire incompatible active rounds before activating the combined lifecycle. Select version, key-set, and recovery ownership rather than adding parallel APIs. |

**Proof needed:** mutate request operation, audience, principal, body, round, and key order; retry
identical and conflicting batches; fail the last element; interrupt before and after retaining shares;
restore stale state; and deliver a delayed failure after a successor round begins. Keep irreversible
signing outside automatically retried database closures.

## Say what verification actually established

A stored approval and a newly installed verifier answer different questions. The approval is a
historical fact about a previous process. The new binary determines future behavior. Neither alone
establishes which historical proof bytes were checked against which batch statement and key.

The [proof research](../../research/proof-verification-and-history.md) distinguishes completed valid
verification, completed invalid verification, a skipped check, and unavailable or malformed evidence.
A combined batch-policy result must also say whether non-proof prerequisites were complete. RISC Zero
checks the expected program and public journal; SP1 binds the verification key, public values, and
circuit version. Ethereum's Engine API distinguishes incomplete processing from `VALID`. These
comparisons explain result meaning; they do not select Via's consensus or historical recovery policy.

Prefer preserving these meanings through callers, persistence, votes, and restart. A richer enum is
useful only if consumers retain its distinctions. A smaller `Result<bool>` can be adequate when a
successful boolean means a completed verdict and errors remain non-verdicts throughout the path.

| Choice | Alternatives and recommendation; reason it remains open |
| --- | --- |
| P1 — Durable verdict meaning | Retain the existing result shape with explicit errors, or introduce a richer outcome where consumers need it. Prefer the smallest representation that preserves completed checks, missing prerequisites, and non-verdicts through storage and outgoing votes. Consensus-threshold policy is a separate choice. |
| P2 — Development execution | Remove proof-free execution or isolate it in a development path without production storage or vote publication. Prefer no production bypass. If simulation remains necessary, its results must remain identifiable after restart. |
| P3 — Historical trust | Re-verify an explicitly bounded history, or adopt a separately approved trusted boundary with stated limits. Prefer joined proof, batch, key, process, and stored-result evidence. Missing objects and unsupported history remain unknown, not cleared. |
| P4 — Recovery and activation | Keep source correction, historical disposition, and activation separately authorized. Re-verification, checkpointing, replay, and republishing have different consequences. Prefer preserving evidence and holding affected activation until the selected recovery policy is satisfied. |

**Proof needed:** genuine supported proof formats, changed commitments, invalid proofs, unavailable
objects, incomplete prerequisites, persisted verdict and vote replay, restart, and already-finalized
cases. Clearing a database row does not retract an attestation already published on Bitcoin.

## Make one deposit decision from one transaction

One Bitcoin payment can carry both a witness deposit instruction and an OP_RETURN receiver. Even
matching receivers do not resolve the ambiguity: ADR 0006 says recognized coexistence produces no
deposit. A malformed companion raises a separate question—when does a message count as a carrier?
Successful receiver conversion cannot silently answer that classification question.

The [carrier research](../../research/conflicting-deposit-messages.md) uses Ord to distinguish a
recognized malformed protocol artifact from absence. That mechanism is useful; its byte rules are not
Via's rules. The [compatibility research](../../research/deposit-compatibility-evidence.md) follows raw
Bitcoin input through conversion, persistence, priority identity, and execution. A parser comparison
alone cannot establish that historical L2 credits remain the same.

Prefer one transaction-wide classification in the shared parser before conversion or persistence,
with the same result reaching main-node, verifier, and indexer consumers. Keep output identity,
transaction position, ordinary txid, and any normalized identity explicit rather than joining them by
similar-looking strings. Preserve the accepted OP_RETURN rules outside the scoped coexistence change.

| Choice | Alternatives and recommendation; reason it remains open |
| --- | --- |
| D1 — Supported historical domain | A stored-row census is narrower than a raw-input-to-execution comparison. Prefer an explicit network, bootstrap range, canonical cutoff, wallet history, and identity mapping. Select which histories must be supported and who can supply missing artifacts. |
| D2 — Recognition boundary | Define byte-level recognition separately from valid decoding. Prefer an explicit matrix for reserved prefixes, short pushes, malformed witnesses, and multiple inputs rather than treating `Some` or marker presence as the specification. Exact cases remain undecided. |
| D3 — One result for all roles | Shared transaction-wide selection avoids independent consumer precedence and first-writer behavior. Prefer that existing owner, with conversion and storage consuming its result. Define rejection versus unavailable-data retry consistently across roles. |
| D4 — Historical disagreement | Preserve old interpretation where evidence requires it, or authorize a scoped correction and recovery. Prefer retaining raw and derived evidence and blocking unsupported activation. Rejecting a deposit now neither refunds BTC nor undoes an existing L2 credit. |
| D5 — Candidate reuse | Reuse checked parsing mechanics only where their byte policy matches the accepted contract. Prefer a minimal shared-policy change over wholesale replacement or importing unrelated withdrawal behavior. Candidate identity and compatibility must be demonstrated first. |

**Proof needed:** the full recognition matrix, equal and unequal receivers, reserved prefixes, ignored
trailing data, multiple inputs and outputs, scan continuation, replay, and reorg removal/re-inclusion.
Compare canonical converted transactions, stored priority identities, and executed credits—not only
whether two parsers returned a value.

## Execute the approved upgrade

Governance approves an ordered deployment list and particular system code. A node reconstructs an
upgrade transaction, starts a batch, and restarts. Success means continuing the same approved operation,
not merely finding an upgrade-labelled transaction or the same displayed protocol version.

The [upgrade research](../../research/historical-upgrade-reconstruction.md) separates approved proposal
bytes, authorization evidence, canonical execution bytes, persisted version state, and actual execution.
OpenZeppelin's Timelock binds ordered call payloads to an operation identity and records completion only
after execution. Optimism constructs deterministic ordered upgrade transactions and selects historical
formats using explicit chain context. Cosmos stops at a due unsupported upgrade rather than silently
considering it absent. None provides atomicity across Via's Bitcoin watcher, separate databases, and VM.

Prefer exact proposal-to-execution evidence and a narrow shared reconstruction boundary. Missing
required data should remain distinguishable from malformed immutable content. Retrying an unavailable
parent can make progress later; repeatedly decoding the same malformed bytes needs a deliberate
operator or governance disposition, not an accidental infinite retry.

| Choice | Alternatives and recommendation; reason it remains open |
| --- | --- |
| U1 — Approved execution identity | Compare proposal/approval identities alone, or join them to ordered fields, system hashes, calldata, canonical transaction identity, and execution. Prefer the complete join. A version number alone is not sufficient evidence. |
| U2 — Retry, reject, or stop | Distinguish unavailable data, deterministic malformed content, unsupported interpretation, and completed rejection. Prefer retry for retrieval failures and an explicit stop/disposition for authorized content that cannot be interpreted. Choose governance policy separately from parser convenience. |
| U3 — Historical interpretation | Use one decoder proven equivalent across supported history, or narrowly bounded historical decoders where evidence requires them. Prefer the simplest evidence-backed option. Startup time and decoder trial order cannot define an approval's meaning. |
| U4 — Coherent implementation | Keep producer, shared parser, authorization lookup, canonical conversion, both governance consumers, and persistence in the compatibility analysis. Prefer reusing the existing owners and only needed candidate mechanics. Separate source readiness from historical reconciliation and activation. |

**Proof needed:** producer-built empty, one-pair, multi-pair, malformed, truncated, and terminal-boundary
cases; unavailable parents and proposals; exact calldata and hashes; conflicting replay; pending-batch
restart; and approval removal/re-inclusion. Any historical activation boundary needs before/at/after
cases. Deposit census completion does not establish governance-history compatibility.

## Observe inscriptions without delaying the sender

Bitcoin RPC stops answering while the sender still has work to do. Monitoring must expose uncertainty
without becoming the reason submission, confirmation, voting, or shutdown stops. Yesterday's count
served by a fresh scrape is not a fresh observation.

[ADR 0001](../../adr/0001-isolate-btc-inscription-observability.md) already selects isolation. The
[observation research](../../research/inscription-monitoring-design.md) distinguishes request identity
from submission attempts, and acquisition time from scrape time. Prometheus recommends exporting event
timestamps rather than repeatedly recomputing age. Newer zkSync separates counts from block identities;
its health-check machinery does not itself provide the required execution isolation.

Prefer one shared bounded observer/classifier with role-owned database adapters, independent client
resources, and a coherent snapshot. A timeout can reject a late result without interrupting a blocked
synchronous call. Separate connections bound client ownership, not database-server CPU or query cost.

| Choice | Alternatives and recommendation; reason it remains open |
| --- | --- |
| I1 — Execution ownership | Prefer a bounded private worker and independently owned runtime, transport, and connection capacity. Choose a subprocess only if forced resource reclamation is required. Neither option permits replacement workers to accumulate while a call is stuck. |
| I2 — Measurement meaning | Omit unavailable values with explicit status, or retain values that every consumer correctly gates. Prefer one coherent optional snapshot with acquisition, expiry, completeness, counts, and optional identity metadata. A failed observation cannot publish healthy zero. |
| I3 — Consumer migration | Prepare consumers first with explicit unavailable behavior, or release producer and consumers together. Prefer a single agreed metric contract covering alerts, dashboard copies, and runbooks. Select owners and release ordering rather than preserving misleading aliases. |
| I4 — Activation evidence | Prefer activation only after both sender lifecycles and effective consumers satisfy the contract; inert consumer preparation can precede it. A source patch or old test report does not prove the running observation path. |

**Proof needed:** blocked RPC and SQL, bounded pending data, zero-result recovery, stale and late results,
superseded attempts, threshold equality, reorg pause, shutdown, and caller-runtime drop. Consumer checks
must cover two targets with different freshness, missing series, future timestamps, and incomplete
positive evidence. A fresh positive overdue observation remains useful even when other data is missing.

## Measure real bridge completion with bounded spending

An HTTP endpoint can be healthy while a user's deposit never credits or a withdrawal never pays. A
**synthetic bridge probe** performs a controlled deposit, verifies L2 credit, requests withdrawal, and
checks the Bitcoin payout. It spends real funds when activated. A small principal does not bound
repeated fees or prevent a retry from creating a second transfer.

The [probe research](../../research/synthetic-bridge-checks.md) separates protocol assertions from
scheduling and custody. Bitcoin Core's PSBT flow can prepare signed transaction bytes before broadcast;
LND separates a logical payment from attempts. These mechanisms support durable reconciliation, not
automatic permission to refill, replace, or repeat a payment after a timeout.

Prefer an independently run client, a dedicated low-balance wallet and isolated L2 account, and one
outstanding cycle. Fix the intended value and fee policy before observing the payout. A receipt-only
check misses actual recipient value; a balance-only check can be confused by unrelated transfers.

| Choice | Alternatives and recommendation; reason it remains open |
| --- | --- |
| B1 — Source and operational owner | Keep the protocol adapter near Via and package it independently, or use an existing separately owned service repository. Prefer separation from sender execution. Repository, maintainer, network, and custody require explicit selection. |
| B2 — Successful cycle evidence | Derive progress from durable facts or maintain guarded status transitions with reconciliation. Prefer exact correlation across deposit output, L2 credit, withdrawal event, and fee-adjusted payout plus selected confirmations. Neither receipts nor balances alone suffice. |
| B3 — Spending admission | Prefer one outstanding cycle, explicit principal and cumulative fee/loss limits, and no automatic refill or replacement initially. Concurrent cycles and automated funding need a separately approved admission policy. A fee-rate cap is not a total-spend cap. |
| B4 — Unknown outcome and reorg | Reconcile retained identities and, when authorized, rebroadcast identical bytes; replacements require explicit lineage and budget. Prefer preserving unknown outcomes over restarting the funded cycle. Reorgs retract completion until the selected evidence returns. |
| B5 — Incident and activation owner | Use an accountable existing incident route or a dedicated one. Prefer non-funded preparation until custody, budgets, deadlines, and funded execution are approved. Passive monitoring and heartbeats remain complementary controls. |

**Proof needed:** value and fee assertions, lost broadcast responses, restart after submission,
replacement lineage, reorg after tentative success, insufficient funds, and long settlement delays.
A live demonstration additionally requires authorized funding and observed end-to-end completion;
source inspection and disposable tests cannot establish that activation.

## Detect failure of monitoring itself

The monitoring host loses power. Its local alerts disappear with it. A **heartbeat** is a periodically
expected signal; an outside **receiver** stores that expectation and declares it missing using its own
clock. Independence concerns shared failure dependencies, not whether the receiver has another name.

The [outside-monitoring research](../../research/independent-monitoring-failure-detection.md) compares
Healthchecks' expected interval and grace with Uptime Kuma's stored-push model. Receipt, expiry, and
human notification are distinct events. A successful HTTP response can acknowledge a request without
arming the intended check. Late retries can extend receiver-observed health after the producer fails.

Prefer a heartbeat from the actual rule-evaluation and alert-delivery chain to a receiver outside the
intended failure domain. A host cron ping has narrower coverage. This heartbeat still does not prove
that every ordinary paging receiver works; delivery canaries cover a different path.

| Choice | Alternatives and recommendation; reason it remains open |
| --- | --- |
| M1 — Independent receiver | Prefer a compatible managed receiver unless a separately operated self-hosted receiver has an accountable owner and sufficient failure independence. Receiver storage, expiry processing, network, credentials, and human delivery all matter; no vendor is selected. |
| M2 — Receipt and timing | Use measured arrival-time semantics with explicit late-retry risk, or add a persisted authenticated freshness protocol if its cost is justified. Prefer honest timing bounds. Interval plus grace is not an outage-to-human guarantee without bounds on retries, expiry processing, and delivery. |
| M3 — Coverage | Prefer heartbeats through rule evaluation and alert delivery; document host-only or transport-only checks as narrower controls. Add complementary probes where needed rather than claiming the heartbeat proves bridge value transfer or every notification route. |
| M4 — Maintenance and recovery | Use bounded automatic maintenance expiry or accountable manual resume. Prefer a declared deadline and verification that the check is armed again. Acknowledging an incident and observing recovery are different events. |
| M5 — Activation and interruption | Reconcile configuration while inactive, then separately approve provisioning, activation, and a bounded interruption exercise. Prefer evidence of outside expiry, human receipt, and recovery. Source configuration alone proves none of those events. |

**Proof needed:** effective route-to-check identity, ignored-but-acknowledged requests, late delivery,
maintenance, token rotation, receiver restart, and an authorized interruption followed by outside
expiry, human notification, and recovery. Timing claims must name the delays actually bounded.

## Preserve the reasoning as decisions become implementation

Resolve one coherent contract at a time. For each accepted choice, retain the scenario, selected
alternative, reason, rejected alternatives worth remembering, and remaining limits. Link an accepted
ADR when the tradeoff warrants one; do not promote a proposed ADR merely because this record links it.
Update the relevant table entry's status without deleting the reasoning that led to the choice.

Research notes own detailed primary-source evidence. This record owns the cross-topic explanation and
current disclosure-safe design status. ADRs own accepted consequential decisions. The planning tracker
owns sequencing and responsibility. Implemented guides and source own runtime instructions; deployment
and recovery records establish what was actually activated. A future re-fork must preserve the contracts,
not necessarily today's file layout; see [the integration research](../../research/via-contracts-and-a-future-refork.md).

When implementation lands, link the concrete revision and proof for the chosen behavior here. Record
activation separately. Keep restricted evidence private until approved for disclosure, and preserve
previous research captures as historical evidence rather than editing several competing live records.
