---
status: pending
---

# Deposit compatibility evidence

This note asks what evidence can establish that a Bitcoin interpretation change preserves historical deposits. It is research, not a new activation decision or a report that a rollout occurred. The related [carrier-selection question](conflicting-deposit-messages.md) changes a different rule.

## The accepted decoder correction has a bounded result

[ADR 0003](../adr/0003-decode-op-return-deposit-pushes.md) remains accepted. It decodes the first push after the first OP_RETURN, accepts valid nonminimal pushes of at least 20 bytes, and uses the first 20 bytes as the receiver. Later payload bytes and later instructions remain ignored. Reserved-prefix exclusions apply to the decoded payload. Exact-length-only, minimal-push-only, and trailing-data rejection are not part of that decision.

The ADR records a parser-independent testnet4 census from height 100,891 through 152,263, inclusive, at cutoff block `000000000003c6b6dcbb090eb29e8f748589559e3639ff5f5351ee4c5695c5b6`. It compared parser revisions `ce8bf0aff06e3cd5a49b42f6b2fc4839225c2d5d` and `f92f5131e7d2395235880dae0bc8594a93c56b43`, including real `ViaL1Deposit::l1_tx` conversion. The recorded result is agreement for all 240 bridge transactions and unchanged receivers and canonical hashes for 108 OP_RETURN deposit messages. This note relies on that accepted record. It does not repeat the census or independently attest to its raw artifacts.

That result closes the decoder correction's historical merge gate for that network and cutoff. It excludes other networks and independent external-node snapshots. It does not establish complete persisted-state or executed-credit reconstruction. It also does not establish the later coordinated stop, cutoff refresh, artifact replacement, and restart required by the ADR. The source tree cannot establish any deployment's parser version.

## A deposit has several identities, not one

The Via source baseline is `8a49f355bfe31720f21b8181db471243709194e5`. In [`MessageParser::parse_bridge_transaction`][parser], `TransactionWithMetadata.tx_index` and the first matching bridge output's `output_vout` accompany the message. `CommonFields.tx_id` is built with `compute_ntxid`, not simply copied from an RPC transaction-ID string. Both node-role processors reverse its raw hash bytes before their database lookup. A historical join must record the raw Bitcoin transaction and distinguish its ordinary transaction ID, normalized ID, byte order, and block position. Treating all of these as an unqualified `txid` conceals a compatibility assumption.

[`ViaL1Deposit`][conversion] carries `l2_receiver_address`, `amount`, `calldata`, `l1_block_number`, `tx_index`, and `output_vout`. `priority_id()` delegates to `ViaPriorityOpId::new` with the last three fields. The resulting priority ID is positional, not a consecutive queue counter. `l1_tx()` excludes system-space receivers and payments below the gas budget. Conversion scales satoshis by `10_000_000_000`, puts that amount in `to_mint`, uses the receiver as sender, execution destination, and refund recipient, and hashes an `L2CanonicalTransaction`. Its nonce is the priority ID. Execution value is zero and execution calldata is empty, even when the parsed inscription has nonempty `call_data`. Thus receiver, amount, and position can change canonical identity. Preserved descriptive calldata is not proof that those bytes executed.

Persistence preserves decisions across restart. The [main-node processor][main-processor] checks `transaction_exists_with_txid` before conversion. Its [DAL][main-dal] stores the normalized Bitcoin identity in `transactions.signature`, while `ON CONFLICT (hash) DO NOTHING` uses the canonical L2 transaction hash. The [verifier processor][verifier-processor] uses the same converter, but its [DAL][verifier-dal] stores `via_transactions.tx_id`, `receiver`, `value`, `calldata`, `canonical_tx_hash`, and `priority_id`, with `ON CONFLICT (tx_id) DO NOTHING`. Replaying a newer parser over an existing database does not automatically replace the old interpretation.

[`verify_op_priority_id`][verification] reads pending verifier hashes in priority order and compares them with bootloader log keys from batch pubdata. It also records success from the log value. Agreement between parsers is therefore weaker than agreement among parser output, persisted hash, batch log, and executed credit. A balance difference alone is also insufficient: gas use affects the final balance increase.

## The evidence boundary must follow state transitions

A sufficient compatibility claim needs an explicit domain and a claim-specific join:

- Historical interpretation needs the network, bootstrap transaction, bridge and governance wallet history, raw candidate payments including rejected ones, canonical block hashes and positions, and both parser revisions. A stored-deposit-only sample excludes precisely the messages that a new interpretation might add.
- Persisted agreement needs the complete converted transaction and the corresponding records from the main node, each supported external-node snapshot, verifier, and public indexer. Parser agreement cannot supply missing database evidence.
- Executed agreement needs the canonical transaction hash, its L2 block and batch, execution status, bootloader log, and resulting credit under the actual VM and fee rules. A stored row is not an execution receipt.
- Rollout agreement needs the release cutoff and proof that all parsing and writing roles stopped, old restarts were disabled, and corrected artifacts were installed before resumption. This is ADR 0003's existing requirement, not a new procedure authorized by this research.

The [watcher][watcher] reloads wallets at the persisted cursor and can shorten and reprocess a range after a wallet update. It saves the cursor after its processors succeed. The [main DAL][main-dal] can delete priority transactions above an L1 rollback height. The [verifier DAL][verifier-dal] separately deletes L1-derived rows and resets processed status above an L2 batch. These operations explain why a cursor alone cannot prove that all earlier writes or executed effects match a parser. They are not evidence that a particular reorg or recovery completed correctly.

## Comparable implementations separate interpretation from origin

The inspected **local newer zkSync** checkout is `ff5f519b11cff863edcfa0f75af10fea113806b0`, not verified latest upstream. Its [`PriorityOpsEventProcessor::process_events`][era-priority] converts protocol `NewPriorityRequest` logs into `L1Tx`, checks a contiguous serial-ID range, skips IDs below `next_expected_priority_id`, and inserts only operations admitted by the settlement contract's count. This is not Via's arbitrary Bitcoin-note trust model. The useful contract is stable conversion into `L1Tx` with explicit replay identity. Copying its consecutive-ID assumption would break Via's positional ordering.

Optimism `v1.9.5`, commit `5662448279e4fb16e073e00baeb6e458b12a59b2`, provides a closer example of separating origin and interpretation. [`UserDeposits`][op-deposits] selects successful receipts and logs from the configured deposit contract. [`UnmarshalDepositLogEvent`][op-log] checks the event selector, indexed version, ABI length, and version-specific opaque payload. It constructs `UserDepositSource` from `L1BlockHash` and `LogIndex`; version 0 determines the decoded transaction fields. A re-inclusion under another block hash is a distinct origin. Via cannot import that event schema or identity without changing its protocol, but it can require equally explicit origin and interpretation evidence. These files do not establish the complete Optimism reorg algorithm.

Ord `0.23.3`, commit `ba60f87b530c01b15f6f8645e2ed4ef52f3f9f74`, is comparable at the Bitcoin byte boundary. [`Runestone::payload` and `Runestone::decipher`][ord] distinguish absent protocol data, recognized valid data, and recognized invalid data represented as a `Cenotaph`. They decode script instructions rather than guessing a byte offset. Unlike Via's accepted first-push rule, Runes concatenate remaining pushes and reject non-push instructions after their marker. This demonstrates that push decoding and application selection are separate contracts, not that Runes semantics should replace Via's. The local source includes tests for all pushdata opcodes and invalid scripts; no tests were run for this research.

Omni Core's [`GetConsensusHash`][omni-state] at `a080849049ac441877ed2085571067afff79213e`
hashes explicitly formatted, ordered records of interpreted protocol state. Its format is documented
so independent implementations can reproduce the digest without sharing internal data types.
For example, balance records use address and property order, and sell offers use transaction-ID order.

A similarly specified digest could help compare Via role snapshots and locate disagreements.
That is a proposed diagnostic, not a selected format or a substitute for the evidence joins above.
Equal digests say nothing about omitted records, excluded fields, chain provenance, or execution
unless those facts are part of the comparison. The inspected file does not establish Omni's entire
activation or recovery procedure.

## What must survive a future re-fork

The Via-owned contract is the accepted Bitcoin interpretation, historical wallet selection, positional priority identity, unit conversion, canonical hash, and replay behavior of persisted and executed deposits. Its present owners are `via_btc_client`, both `via_btc_watch` processors, `ViaL1Deposit`, their DALs, and `via_zk_verifier`. The closest newer-zkSync integration is the `L1Tx` ingestion and storage path used by `PriorityOpsEventProcessor`, followed by version-specific VM execution. Its event source and sequential priority counter are adapters, not Via protocol rules.

A replacement adapter needs golden byte cases for all four push encodings, short and malformed first pushes, reserved prefixes, ignored trailing data, witness fields, and carrier coexistence. Historical replay must preserve the whole converted `L1Tx`, not just the receiver. Reorg cases must bind old and new block positions to the expected priority IDs and persistent state. VM, bootloader, system-contract hashes, and database schema versions belong in the evidence bundle because successful decoding alone does not fix execution meaning. This is a preservation requirement, not proof that a module extraction already works.

## A support manifest makes unknown evidence explicit

The recommended evidence unit is one declared network and bootstrap history with a canonical
cutoff hash, not "all deposits" without a domain. Each supported domain needs its Bitcoin genesis
or network identifier, bootstrap transaction and starting height, historical wallet changes,
L2 chain identity, source and artifact revisions, and included snapshot identities. A snapshot
also needs its L1 cursor and block hash, L2 block and batch boundaries, schema and protocol
versions, and a statement of which earlier raw data it omits. The human decision is whether
each domain is supported, deliberately excluded, or awaiting evidence. An unavailable snapshot
cannot be represented as an empty matching history.

Within each domain, evidence should join raw transaction bytes and ordinary Bitcoin txid to
`compute_ntxid`, the database byte order, canonical block hash, height, transaction index, and
bridge-output index. It then joins parsed receiver and amount to `ViaPriorityOpId`, the canonical
transaction and hash, and each role's stored record. Finally it joins that hash to L2 inclusion,
batch pubdata, bootloader status, and the actual execution effects. Preserve original records
and their source identifiers alongside each comparison. Normalizing away a duplicate or an
unexpected rejection before the join hides the discrepancy being investigated.

Optimism's pinned [`UserDepositSource::SourceHash`][op-source] is a useful contrast.
It hashes `L1BlockHash` and `LogIndex`, then hashes that digest with
`UserDepositSourceDomain = 0`. Upgrade deposits use a separate domain and `Intent`.
This makes origin explicit even when decoded fields match. It does not authorize replacing
Via's priority identity or adding a block hash to its canonical transaction. Via needs those
origin fields in evidence because its positional identity alone does not establish canonical
inclusion or snapshot provenance.

The smallest useful stop rule distinguishes disagreement from missing evidence. Stop a
compatibility claim for a changed receiver, amount, priority ID, canonical hash, eligibility,
unexpected duplicate, missing expected role row, unknown artifact revision, incomplete raw
candidate set, or an unjoined execution result. A source RPC outage is unavailable evidence,
not a malformed transaction. A deterministic rejected encoding is not an infrastructure failure.
These release stops do not imply that ordinary Bitcoin scanning should stop on arbitrary
malformed user notes.

## Activation preserves history or explicitly changes it

Optimism's pinned [`PreparePayloadAttributes`][op-attributes] makes the distinction concrete.
Receipt-fetch failure returns `NewTemporaryError`; an L1 parent or epoch-hash mismatch returns
`NewResetError`; a deposit derivation error returns `NewCriticalError`. The caller does not
silently use the partial deposit list returned with a decoding error by `DeriveDeposits`.
That strict treatment fits contract-emitted logs. Via's untrusted Bitcoin notes still need
deterministic rejection without wedging the watcher. The transferable rule is to preserve the
distinction between malformed source data, unavailable source data, and inconsistent chain origin.

The same Optimism function derives `nextL2Time` from the L2 parent and configured block time,
checks `IsEcotoneActivationBlock` and `IsFjordActivationBlock`, and appends deterministic upgrade
transactions after user deposits and any completion transaction. Activation depends on chain
state and configuration, not the time a process happened to restart. Via should likewise avoid
a parser rule selected by process start time or database emptiness.

There are two viable Via compatibility strategies. Applying the corrected eligibility rule to
every supported replay is the simpler choice when a complete comparison establishes no changed
persisted or executed history. If affected historical deposits exist, a per-network activation
boundary can preserve the former interpretation before that boundary and use the accepted
coexistence rule after it. The boundary must be reproducible from agreed chain context, and old
and new snapshots must select the same interpretation for the same input. This retains two
historical semantics and increases replay obligations. Neither source comparison chooses a
height nor authorizes that history-preservation policy.

An affected executed deposit cannot be corrected with `ON CONFLICT`, a cursor rewind, or an
automatic row overwrite. Pending rows also need their actual mempool, inclusion, and snapshot
state checked before correction. Preserve the evidence and stop until an explicit recovery
decision specifies the state transition. No source-only answer can establish whether such a
case exists in a supported deployment.

Geth v1.14.12, `293a300d64be3d9a1c2cc92c26fcff4089deadcd`, makes activation drift explicit.
[`ChainConfig.CheckCompatible`](https://github.com/ethereum/go-ethereum/blob/293a300d64be3d9a1c2cc92c26fcff4089deadcd/params/config.go)
compares stored and new fork boundaries against the current head. `ConfigCompatError` carries a
block or timestamp rewind target. The
[`genesis setup`](https://github.com/ethereum/go-ethereum/blob/293a300d64be3d9a1c2cc92c26fcff4089deadcd/core/genesis.go)
reads persisted configuration; the
[`blockchain initializer`](https://github.com/ethereum/go-ethereum/blob/293a300d64be3d9a1c2cc92c26fcff4089deadcd/core/blockchain.go)
rewinds affected canonical state before writing the replacement configuration.

The transferable requirement is a reproducible, durable rule identity compared with retained state.
Whether Via stores that beside the cursor or in another existing owner remains a design choice.
Geth's rewind is not a model for simply lowering a Via cursor after an L2 credit has executed.
Prefer refusal on incompatible rule/state identity until the chosen recovery authority acts.
Deposit and governance boundaries may share a comparison pattern without sharing a database row
or the same historical policy.

Coordinated release follows the existing ADR 0003 stop requirement. The proposed order is to
agree on the recognition and supported-history contracts, produce immutable compatible artifacts
for every writer and reader, stop all supported roles and disable old restarts, bind retained
state and the refreshed evidence cutoff, perform only an approved reconciliation if needed,
install matching artifacts and configuration, and resume under observed role agreement.
Main nodes, external nodes, verifier watchers and comparison code, and the public indexer are
in scope. A snapshot published under the new rule must state that rule and its origin.
Release artifact availability is not proof that the stop or activation occurred.

## Reusable local proof is narrower than historical clearance

The local Git objects used by the accepted census remain available. At
`ce8bf0aff06e3cd5a49b42f6b2fc4839225c2d5d`, the OP_RETURN parser uses a total-script-length offset.
At `f92f5131e7d2395235880dae0bc8594a93c56b43`, it uses
`instructions().nth(1)?.ok()?` and `push_bytes()`. The inspected published heads for the length
fix and push correction show those same respective mechanisms. This source check supports
reusing the recorded comparison contract, not expanding its historical result.

The broader ingestion candidate's [checkpoint contract][ingestion-contract] records `height`,
`hash`, `kernel_version`, `observation_rule_version`, `context_hash`, and `last_plan_hash`.
These are useful names for the facts a replay manifest needs. Its carrier and identity
semantics differ from accepted Via behavior, as the [carrier note](conflicting-deposit-messages.md)
explains. A checkpoint type alone proves neither atomic storage nor cross-role compatibility.
Do not adopt its shadow-storage framework merely to attach a parser version to evidence.

Future local proof should compare the entire semantic `L1Tx`, excluding wall-clock receipt
metadata, then exercise each role with empty and retained state. Distinguishing cases include
an unrelated witness before a valid deposit, malformed recognized companions, equal-receiver
coexistence, reserved prefixes with malformed bodies, ignored trailing OP_RETURN instructions,
wallet rotation, restart after partial processing, and re-inclusion at a new block position.
Each proof must observe deposits reaching storage and verifier comparison, not just parser
return values. Static inspection in this research executed none of those scenarios.

[op-source]: https://github.com/ethereum-optimism/optimism/blob/5662448279e4fb16e073e00baeb6e458b12a59b2/op-node/rollup/derive/deposit_source.go
[op-attributes]: https://github.com/ethereum-optimism/optimism/blob/5662448279e4fb16e073e00baeb6e458b12a59b2/op-node/rollup/derive/attributes.go
[ingestion-contract]: https://github.com/vianetwork/via-core/blob/212d115166a1ce030dc1ae8011b4fde393227068/core/lib/via_btc_ingestion/src/lib.rs

## Judgment and remaining choice

Keep the closed receiver-decoder census closed at its stated boundary. Do not reuse it as clearance for exclusive carriers or for networks and snapshots it did not cover. Reject both a stored-row-only census and a parser-only claim of executed compatibility.

The remaining human choice is which additional network histories, bootstrap states, external-node snapshots, and execution records the next change must support, who owns each evidence join, and what discrepancies stop release. Collection beyond existing artifacts requires separate authorization. No live collection, replay execution, rollout verification, or validation command was performed here.

[parser]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/indexer/parser.rs#L119-L163
[conversion]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/types/src/l1/via_l1.rs#L28-L123
[main-processor]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/via_btc_watch/src/message_processors/l1_to_l2.rs
[main-dal]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/dal/src/via_transactions_dal.rs
[verifier-processor]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_btc_watch/src/message_processors/l1_to_l2.rs
[verifier-dal]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/verifier_dal/src/via_transactions_dal.rs
[verification]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_zk_verifier/src/lib.rs#L305-L362
[watcher]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/via_btc_watch/src/lib.rs#L149-L210
[era-priority]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_watch/src/event_processors/priority_ops.rs#L40-L115
[op-deposits]: https://github.com/ethereum-optimism/optimism/blob/5662448279e4fb16e073e00baeb6e458b12a59b2/op-node/rollup/derive/deposits.go
[op-log]: https://github.com/ethereum-optimism/optimism/blob/5662448279e4fb16e073e00baeb6e458b12a59b2/op-node/rollup/derive/deposit_log.go
[ord]: https://github.com/ordinals/ord/blob/ba60f87b530c01b15f6f8645e2ed4ef52f3f9f74/crates/ordinals/src/runestone.rs
[omni-state]: https://github.com/OmniLayer/omnicore/blob/a080849049ac441877ed2085571067afff79213e/src/omnicore/consensushash.cpp#L99-L205
