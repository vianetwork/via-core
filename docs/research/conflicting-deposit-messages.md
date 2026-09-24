---
status: pending
---

# Conflicting deposit messages

The accepted policy is that a recognized witness carrier and an unreserved OP_RETURN carrier in the same Bitcoin transaction yield no deposit. This research scopes that policy across parsing, conversion, storage, verification, indexing, and restart. It does not ask whether to choose witness precedence or OP_RETURN precedence again. Historical support and rollout evidence depend on the separate [compatibility boundary](deposit-compatibility-evidence.md).

[ADR 0006](../adr/0006-reject-recognized-deposit-carrier-coexistence.md) records that accepted policy
and its scoped relationship to ADR 0003. The malformed-carrier recognition boundary remains open.

## Selection belongs before conversion and persistence

At Via baseline `8a49f355bfe31720f21b8181db471243709194e5`, [`MessageParser::parse_bridge_transaction`][parser] finds the first bridge output, records its index in `TransactionWithMetadata.output_vout`, and independently invokes `parse_inscription_deposit`, `parse_op_return_deposit`, and withdrawal parsing. That composition is not the accepted exclusive-carrier policy. The implementation boundary is transaction-wide classification before any deposit result reaches a consumer. The policy must suppress deposits, not indiscriminately suppress unrelated system or withdrawal messages.

The existing [`parse_op_return_deposit`][op-return] selects the first OP_RETURN output. It decodes the next instruction with `Script::instructions`, checks the active `VIA_WI` and `VIA_PROTOCOL:*` prefixes, and takes the first 20 payload bytes as `receiver_l2_address`. A later OP_RETURN does not replace a malformed or reserved first one. [ADR 0003](../adr/0003-decode-op-return-deposit-pushes.md) preserves this selection, nonminimal pushes, ignored trailing bytes, and ignored later instructions. Carrier exclusivity must not silently turn into another receiver-decoding correction.

[`parse_inscription_deposit`][witness] inspects input witnesses through `find_via_inscription_protocol`, constructs `CommonFields`, and delegates to `parse_l1_to_l2_message`.
The latter reads the receiver, contract address, and call-data pushes.
Carrier recognition is separate from successful field conversion.
The existing `Option<FullInscriptionMessage>` distinguishes a decoded message from no message, but does not explain why a recognized carrier failed.
Defining coexistence as "both decoders returned `Some`" requires proof that this matches the accepted recognition-based policy.

That distinction matters for a witness with a recognizable marker but invalid address fields, a short OP_RETURN push, a reserved OP_RETURN prefix, multiple inputs, and unrelated witness data before a Via message. These are required classification examples, not newly accepted encoding rules. The exact recognition boundary for malformed carriers needs to be made explicit in the implementation specification without reopening the rule for recognized coexistence.

## The same result must reach every role

[`BitcoinInscriptionIndexer::process_block`][indexer] sends bridge transactions through the shared parser, then `is_valid_l1_to_l2_transfer` compares the decoded amount with the total paid to the bridge. This amount check is not a carrier selector. Both carriers can describe the same bridge payment, so selection must already be resolved before value validation and conversion.

The [main-node processor][main] and [verifier processor][verifier] both construct `ViaL1Deposit` and call `l1_tx()`.
The [external-node builder][external] installs `BtcWatchLayer` with `is_main_node: false`, so external-node storage is also in scope.
[`ViaL1Deposit`][conversion] derives `priority_id` from height, transaction index, and output index.
Its canonical transaction uses the receiver, mint amount, and fixed fee fields, with empty execution calldata.
Neither priority identity nor canonical hashing should acquire an implicit preferred-carrier rule.

The main DAL stores the Bitcoin identity in `transactions.signature` but conflicts on canonical `hash`; the verifier DAL conflicts on `via_transactions.tx_id`. Both processors check Bitcoin identity before writes, and both collect messages before their insertion loops. These mechanisms provide restart behavior for previously stored deposits, not transaction-level ambiguity resolution. The accepted policy therefore cannot be implemented by relying on a database conflict or on whichever message happens to be inserted first. See the inspected [main DAL][main-dal] and [verifier DAL][verifier-dal].

The [public indexer processor][public-indexer] also uses `ViaL1Deposit`, then records `Deposit.priority_id`, `tx_id`, `receiver`, `value`, `calldata`, and `canonical_tx_hash`. Its retained inscription calldata is descriptive, not execution calldata. A rejected ambiguous payment must not remain presented as an accepted deposit because an explorer used a different selector.

[`verify_op_priority_id`][verification] compares ordered pending verifier canonical hashes with bootloader log keys. If one role persists a carrier choice and another rejects coexistence, a shared Bitcoin payment does not guarantee agreement at that comparison. The historical evidence must distinguish never-persisted ambiguous payments, persisted but unexecuted deposits, and executed deposits. Rejection after a software update cannot refund BTC or undo an executed L2 credit.

## Rejection, retry, and rollback are different outcomes

[ADR 0002](../adr/0002-reject-invalid-deposit-address-lengths.md) establishes that invalid deposit address lengths reject the affected encoding without stopping Bitcoin scans. The exclusive-carrier decision deliberately changes the eligibility of a companion deposit when both carriers are recognized. This is a scoped later policy change, not permission to alter all independent-message behavior recorded by the older ADRs.

The [main watcher][watcher] advances `update_last_processed_l1_block` after all processors succeed. Its wallet processor may shorten and replay a range when the wallet changes. The [verifier watcher][verifier-watcher] additionally rereads the cursor before advancing it, because reorg processing may have changed it. Thus deterministic ambiguity rejection should produce no deposit and allow normal block completion, while propagated RPC and database failures leave processing unfinished. A restart must not transform rejected coexistence into a single-carrier deposit because one decoder happened to run first.

Rollback remains a separate state transition. The [main DAL][main-dal] removes priority transactions above an L1 height. The [verifier DAL][verifier-dal] has both L1 deletion and L2 status-reset operations. These are reasons to include reorg and restart fixtures in later proof, not evidence that arbitrary persisted ambiguity can be repaired through a cursor rewind. No such recovery was executed here.

## Comparable selection mechanisms

The inspected **local newer zkSync** revision is `ff5f519b11cff863edcfa0f75af10fea113806b0`, not verified latest upstream. [`PriorityOpsEventProcessor`][era] has no competing Bitcoin-carrier mechanism. It decodes protocol events, enforces consecutive serial IDs, skips already-known operations, and persists the selected `L1Tx` records. Its useful comparison is one canonical event-to-transaction interpretation before storage. Its fatal event-decoding and contiguous-ID assumptions are not substitutes for Via's untrusted-note rejection or positional priority IDs.

Ord `0.23.3`, commit `ba60f87b530c01b15f6f8645e2ed4ef52f3f9f74`, makes recognition explicit.
[`Runestone::payload`][ord] looks for OP_RETURN followed by `OP_PUSHNUM_13`.
It ignores nonmatching outputs and returns the first matching carrier's valid or invalid payload.
A malformed recognized carrier becomes `Artifact::Cenotaph` rather than falling through to a later valid carrier.
Its `outputs_with_non_pushdata_opcodes_are_cenotaph` test includes an invalid first matching output and a valid second one.
This is a concrete precedent for distinguishing absent data from recognized invalid data.
It does not implement Via's two-carrier exclusivity.
Importing its first-matching-output or concatenated-push rules would contradict Via's existing rules.

Optimism `v1.9.5`, commit `5662448279e4fb16e073e00baeb6e458b12a59b2`, separates event eligibility from decoding. [`UserDeposits`][op-deposits] first restricts receipt success, emitter address, and topic, then [`UnmarshalDepositLogEvent`][op-log] checks the indexed version and opaque-data layout. The decoder reports malformed recognized events instead of guessing another layout. Its contract-owned, explicitly versioned event is not an exclusive Bitcoin carrier, but the classification boundary is comparable. The inspected functions accumulate decoding errors; they alone do not prove how every caller handles those errors or reorgs.

Neither project supplies Via's accepted policy. Their useful evidence is that selection and recognition are protocol rules that precede persistence, not properties to infer from a storage uniqueness constraint.

## Re-fork contract and implementation boundary

The Via contract that must survive is transaction-wide no-deposit output for recognized coexistence, with ADR 0003's OP_RETURN interpretation otherwise unchanged. Current owners are the shared `MessageParser` and `BitcoinInscriptionIndexer`, both watcher processors, the public indexer, their DALs, and verifier log comparison. The closest newer-zkSync integration remains its `L1Tx` ingestion path. Carrier classification belongs on the Bitcoin side of that adapter, before any VM-specific canonical transaction is persisted.

A coherent handoff includes byte-level classification cases and complete consumer replay. Cases need witness-only, OP_RETURN-only, neither, equal-receiver coexistence, different-receiver coexistence, active reserved prefixes, malformed recognized carriers, multiple inputs, and multiple outputs. The two coexistence cases must both yield no deposit. Equality of receiver bytes is not a reason to accept two recognized carriers. Conversion cases must retain amount, priority ID, and canonical hash for unaffected deposits. Restart and rollback cases must cover empty databases and retained records in each role, including external nodes and the public indexer. These are proposed proof requirements, not claims of tests already run.

The same work must reconcile any existing broader parser changes before selecting extraction or replacement. This note makes no claim that an unreviewed candidate is active or that moving the parser behind a new interface is already safe. Database schema, historical snapshots, VM fee constants, and canonical transaction hashing are dependencies of the cutover even if most need no code change.

## Recognition needs a byte boundary, not a successful deposit

The fresh comparison distinguishes two different Ord mechanisms at the same pinned revision.
`Runestone::decipher` returns `Option<Artifact>`, where `Artifact::Runestone` and
`Artifact::Cenotaph` distinguish valid and recognized-invalid data. Its private
`Payload::Valid(Vec<u8>)` and `Payload::Invalid(Flaw)` preserve errors after the exact
`OP_RETURN OP_PUSHNUM_13` marker. An invalid instruction before that marker does not establish
Runes ownership. An invalid instruction after it does. `Flaw::InvalidScript`, `Flaw::Opcode`,
and `Flaw::Varint` remain distinct.

Ord's [inscription envelopes][ord-envelope] do not use that same taxonomy.
`RawEnvelope` is `Envelope<Vec<Vec<u8>>>`; `ParsedEnvelope` is `Envelope<Inscription>`.
The envelope records `input`, `offset`, `pushnum`, and `stutter`. Conversion preserves
`duplicate_field`, `incomplete_field`, and `unrecognized_even_field`. However, a missing
`OP_ENDIF` returns no envelope, and a script decode error causes `from_transaction` to discard
that input's envelopes. Thus "Ord preserves recognized malformed carriers" is accurate for
Runes, but not a general statement about every inscription envelope. Via should specify its
own truncation boundary rather than copy either behavior without a policy decision.

Babylon at `dc8848f2afa9e7827fea990a8e194b3ab5f993b3` provides another two-stage comparison in
[`identifiable_staking.go`](https://github.com/babylonlabs-io/babylon/blob/dc8848f2afa9e7827fea990a8e194b3ab5f993b3/btcstaking/identifiable_staking.go).
`IsPossibleV0StakingTx` requires at least two outputs, exact OP_RETURN shape and length, the expected
four-byte tag, version zero, and only one matching output. `ParseV0StakingTx` additionally decodes
fields, reconstructs the staking script, and requires a matching staking output. Nonmatching
OP_RETURNs do not count as duplicates. Recognition is cheaper than full validation, but already
validates shape and length; it does not preserve every malformed tagged carrier. This supports
separating recognition from acceptance, not importing Babylon's exact boundary into Via.

At the Via baseline, `find_via_inscription_protocol` finds any instruction pushing the exact
`via_inscription_protocol` bytes. The bridge witness path then calls `parse_l1_to_l2_message`
without checking the next push is `L1ToL2Message`. It also uses `?` inside the input loop and
`filter_map(Result::ok)` over script instructions. Consequently, an unrelated first witness
can end the search, and script decode failures are not retained as classification evidence.
The producer's `build_l1_to_l2_message_script` writes the message-kind push before the receiver,
contract, and calldata. Protocol-family ownership and deposit-kind ownership therefore need
separate definitions. Recognizing every protocol-family marker as a deposit would also count
system and governance messages.

The recommended specification, still pending acceptance, recognizes a witness deposit when
instruction decoding reaches the exact protocol marker followed immediately by the exact
deposit-kind push in the supported witness script position. Once that pair is reached,
invalid receiver or contract lengths, missing fields, and a later script error remain
recognized-invalid. A raw substring in a signature or another push is not a marker. Unrelated
inputs do not stop the search. A missing or truncated deposit-kind push does not establish
deposit ownership under this recommendation. Requiring a complete valid envelope instead is
a viable narrower alternative, but permits a malformed companion to stop counting toward
coexistence. Requiring only the family marker is broader and needs an explicit rule for every
non-deposit message kind. Neither alternative follows from ADR 0006 alone.

The untagged OP_RETURN format needs a different boundary. A complete first push with no active
reserved prefix can be classified as an unreserved carrier even if it contains fewer than
20 bytes. That lets short carriers remain recognized-invalid without changing ADR 0003's
receiver grammar. A bare OP_RETURN, non-push first instruction, or truncated first push has
no decoded payload on which to perform reserved-prefix classification. Two choices remain:
count all such first OP_RETURN outputs conservatively for coexistence, or count only complete
unreserved pushes. The former also suppresses a valid witness next to unrelated malformed
OP_RETURN data. The latter has a sharper instruction-level boundary, but permits that witness
to survive. Recommend the complete-push boundary unless historical evidence and the intended
anti-ambiguity policy require the broader one. This is a policy recommendation, not a new
interpretation of accepted text.

The five current exclusions are exact leading prefixes `VIA_WI`, `VIA_PROTOCOL:UPGRADE`,
`VIA_PROTOCOL:SEQ`, `VIA_PROTOCOL:BRI`, and `VIA_PROTOCOL:GOV`. A complete matching prefix remains
reserved even if its own message is malformed. A partial prefix such as `VIA_W`, or an unknown
`VIA_PROTOCOL:` suffix, is not one of those exclusions. Silently reserving the whole namespace,
retired prefixes, or byte fragments would change the accepted decoder contract. One shared
prefix classifier should own these exact exclusions. Protocol changes to that list require
their own compatibility reasoning.

Recognition precedes both the indexer's bridge-total check and `ViaL1Deposit::is_valid_deposit`.
A recognized witness with a system-space receiver still conflicts with a recognized unreserved
OP_RETURN carrier even though conversion would reject the witness alone. Equal and different
receivers both yield no deposit. Additional OP_RETURN outputs remain ignored under ADR 0003.
The witness scan must inspect enough inputs to establish transaction-wide coexistence, without
silently adopting a new multiple-witness winner policy. Multiple witness-only messages need an
explicit same-carrier rule if the new classifier exposes cases the old early return concealed.

## Candidate reuse has concrete policy limits

The inspected public parser proposal at
[`2d27424a4a7c3202d302895753eb64bc5b6eef75`][parser-candidate] is not an implementation of ADR 0006.
`parse_bridge_transaction` retains a successfully decoded witness and suppresses the OP_RETURN
deposit only when that witness exists. Its `first_op_return_carrier` returns
`OpReturnCarrier::One` or `Two` only after inspecting the remaining instructions. The bridge
deposit branch accepts `One`, which rejects trailing instructions that ADR 0003 ignores.
`ReservedFromDeposit::RetiredReserved` also adds a reservation not present in the accepted
active-prefix list. Useful separations include borrowed payload slices and explicit
message-kind checks, but their selection rules cannot be imported unchanged.

The separate ingestion proposal at
[`212d115166a1ce030dc1ae8011b4fde393227068`][ingestion-candidate] has a transaction-wide
`finish_deposits` owner, but it rejects only when the set of decoded receivers has a size other
than one. It then selects an inscription before an output and emits `DepositEncoding::Both`
for equal-receiver coexistence. It also emits a `DepositObserved` for each matching bridge
output using `compute_txid()`. Those are material differences from the accepted coexistence
policy and the current normalized, first-bridge-output identity. Its positioned events can
inform diagnostic evidence; adopting its engine or storage architecture is not necessary
for the bounded carrier change.

A small internal result separating absent, recognized-invalid, and decoded carriers is useful
because the existing `Option<FullInscriptionMessage>` loses the distinction. It need not become
a new public persistence format. Classify borrowed script data once, determine eligibility,
and construct the existing deposit message only after selection. This avoids creating and
cloning two `tx_outputs` vectors for a payment that must be rejected. Classification requires
no additional RPC, database query, or lock. Error reasons and input/output positions are enough
for deterministic fixtures and evidence; an event framework or new table needs a separate use.

[ord-envelope]: https://github.com/ordinals/ord/blob/ba60f87b530c01b15f6f8645e2ed4ef52f3f9f74/src/inscriptions/envelope.rs
[parser-candidate]: https://github.com/vianetwork/via-core/blob/2d27424a4a7c3202d302895753eb64bc5b6eef75/core/lib/via_btc_client/src/indexer/parser.rs
[ingestion-candidate]: https://github.com/vianetwork/via-core/blob/212d115166a1ce030dc1ae8011b4fde393227068/core/lib/via_btc_client/src/ingestion_engine_v2.rs

The complete-push recommendation includes an empty push as recognized-invalid. This is observable
in existing fixtures: `inscription_addresses_require_exactly_twenty_bytes` expects a valid
OP_RETURN companion to survive an invalid inscription, and its valid-witness case uses an empty
OP_RETURN push. Those expectations belong to the older independent-message policy. The selected
recognition rule must deliberately update the companion expectations while retaining the exact
address-length contract. Excluding an empty push, unlike other short pushes, is another possible
policy but needs a stated reason. It must not emerge accidentally from a decoder's `None`.

## Judgment and remaining choice

Use the shared transaction parser as the policy owner and require every consumer to receive its single result. Reject witness-first, OP_RETURN-first, receiver-equality exceptions, and database-first-writer selection as implementations of the accepted decision. Preserve unrelated withdrawal and system parsing.

The open choices are the exact malformed-carrier recognition boundary, the supported historical and snapshot domain, the owner of persisted/executed reconciliation, and the evidence that permits a coordinated release. They are not a new vote on exclusivity. No source, deployment, database, or live-network changes, and no tests or validation commands, were performed for this note.

[parser]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/indexer/parser.rs#L119-L163
[op-return]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/indexer/parser.rs#L780-L840
[witness]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/indexer/parser.rs#L734-L778
[indexer]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/indexer/mod.rs#L146-L164
[main]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/via_btc_watch/src/message_processors/l1_to_l2.rs
[verifier]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_btc_watch/src/message_processors/l1_to_l2.rs
[external]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/bin/via_external_node/src/node_builder.rs#L590-L608
[conversion]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/types/src/l1/via_l1.rs
[main-dal]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/dal/src/via_transactions_dal.rs
[verifier-dal]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/verifier_dal/src/via_transactions_dal.rs
[public-indexer]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_indexer/node/indexer/src/message_processors/deposit.rs
[verification]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_zk_verifier/src/lib.rs#L305-L362
[watcher]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/via_btc_watch/src/lib.rs#L149-L210
[verifier-watcher]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_btc_watch/src/lib.rs#L204-L221
[era]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_watch/src/event_processors/priority_ops.rs
[ord]: https://github.com/ordinals/ord/blob/ba60f87b530c01b15f6f8645e2ed4ef52f3f9f74/crates/ordinals/src/runestone.rs
[op-deposits]: https://github.com/ethereum-optimism/optimism/blob/5662448279e4fb16e073e00baeb6e458b12a59b2/op-node/rollup/derive/deposits.go
[op-log]: https://github.com/ethereum-optimism/optimism/blob/5662448279e4fb16e073e00baeb6e458b12a59b2/op-node/rollup/derive/deposit_log.go
