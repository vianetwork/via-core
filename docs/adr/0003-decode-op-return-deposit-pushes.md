---
status: accepted
---

# Decode OP_RETURN deposit pushes while preserving ignored data

A Bitcoin script stores data in a push instruction. The shortest form for 1 to 75 payload bytes uses one opcode
byte that is also the length. For 76 to 255 bytes, the shortest form is `OP_PUSHDATA1`, with a separate length
byte. Larger payloads need `OP_PUSHDATA2` or `OP_PUSHDATA4`.

A 20-byte receiver can use any of these four forms. The script interpreter checks for a minimal push only when
the push executes and `SCRIPT_VERIFY_MINIMALDATA` is enabled. An OP_RETURN output cannot be spent. Any attempt
to execute its script fails at OP_RETURN, before the data push. The minimal-push check therefore does not reject
these longer forms.

Bitcoin Core v27.0's OP_RETURN policy permits all four 20-byte encodings if the transaction also meets its other
policy checks. Relay depends on the node's version and configuration. See the [output classifier][bitcoin-solver],
the [script interpreter][bitcoin-interpreter], and the [standardness checks][bitcoin-policy].

Until this decision, the parser did not read the push instruction. It started at byte 2, or at byte 3 when the
whole script was longer than 75 bytes, on the assumption that a longer script meant `OP_PUSHDATA1`. That threshold
is off by three for the shortest encoding. `OP_PUSHDATA1` becomes the shortest form at 76 bytes of data, or 79
bytes of script. A direct push of 74 or 75 bytes makes a script of 76 or 77 bytes, so the rule skipped the first
receiver byte and decoded an address shifted by one. With no later instructions, a PUSHDATA1 push of 20 to 72
bytes made a script of at most 75 bytes, so the rule read the length byte as the first receiver byte.
PUSHDATA2 and PUSHDATA4 never matched either offset.

An affected note that passes the other deposit checks fails silently. The parser returns a different 20-byte
address without a decode error. Every node role that decodes the same note with this parser version returns
the same address. The receiver determines the deposit's canonical transaction hash and L2 destination, so the
decoder must follow the push's declared boundary.

## Decision

The shared `MessageParser` decodes the first data push after the first OP_RETURN output's initial opcode.
It accepts any valid push encoding with at least 20 payload bytes and takes the first 20 as the receiver.
The [encoding reference](../../core/lib/via_btc_client/DEV.md#deposit-decoding) defines the byte-level rules.

Bytes after the receiver inside the push, and instructions after the push, stay ignored. They supply no execution
field today. Giving them a meaning later changes what executes and therefore the canonical hash, so it would need
its own decision and its own activation. Active reserved-prefix exclusions apply to the decoded payload.
First-OP_RETURN selection, output counts, and independent inscription and withdrawal decoding remain unchanged.

Requiring exactly 20 payload bytes would also reject longer deposits that the previous parser read correctly.
Requiring minimal encoding or forbidding later instructions would introduce further rejections. These restrictions
can detect some malformed notes, but rejection neither reverses the Bitcoin payment nor refunds it.
This decision keeps those allowances. Each restriction is a protocol choice with its own cost in history, and none
of them is needed to correct the receiver. The normal producer continues to emit `6a14<receiver20>`.

## Consequences

Four kinds of note decode differently after this decision:

- an otherwise eligible push whose old offset was wrong now yields the receiver the sender wrote;
- a payload that starts with an active reserved prefix is excluded even when the old offset skipped past the prefix;
- a truncated first push, a first push shorter than 20 bytes, or a non-push first instruction yields no deposit,
  even when the script held enough raw bytes for the old rule;
- a small set of direct pushes that the old rule rejected now yield a deposit.

For a single complete push with no later instructions, affected forms include direct pushes of 74 or 75 bytes,
PUSHDATA1 pushes of 20 to 72 bytes, and PUSHDATA2 or PUSHDATA4 pushes of at least 20 bytes. PUSHDATA1 pushes of
73 bytes or more already had the correct offset. Later instructions can move the old script-length threshold
even when the first push stays the same.

`parse_op_return_withdrawal` keeps the length-based offset. Its only producer is the verifier's transaction builder,
and that builder emits the short form. Replacing that offset is a separate change.

A rejected OP_RETURN note does not reject an independently valid inscription in the same transaction. A bridge
payment that produces no accepted deposit receives no L2 credit. Rejection does not reverse or refund the Bitcoin
payment. This decision defines no recovery process.

## Compatibility gate

Correct decoding can change a receiver, change a reserved-prefix exclusion, or reject a malformed first push
that previously supplied enough raw bytes. Unlike the bounds checks in
[ADR 0002](0002-reject-invalid-deposit-address-lengths.md), this correction can change canonical transaction hashes.

A parser-independent census must cover the supported history and bootstrap state before merge or release.
Complete coverage with no decoder differences permits correction of that history. A difference requires explicit
Bitcoin, parser, persisted-state, and execution identity joins, or a justified transition boundary.
Historical agreement does not make concurrent use of the old and new decoders safe.

### Current testnet4 census

The September 13, 2026 census covers the current testnet4 network from height 100,891 through 152,263, inclusive.
The cutoff block is `000000000003c6b6dcbb090eb29e8f748589559e3639ff5f5351ee4c5695c5b6`.
The bridge is `tb1ppsy8j80jtns42rkpdsfcv25qfschqejxmk6datkvu236eekr4fms06wnz0`.

The census starts with every bridge transaction, including payments that never became stored deposits.
The public address index and Bitcoin Core's existing watch wallet contain the same 240 unique transaction IDs,
with matching block hashes and heights. Governance history and retained wallet records identify no other bridge
in this interval. The configured bootstrap decodes identically under both parser versions. Retained bootstrap
metadata identifies the same bridge and start height.

An independent byte decoder checks the old offset against the first push's declared boundary. A Rust replay uses
the exact parser files from base `ce8bf0aff06e3cd5a49b42f6b2fc4839225c2d5d` and
reviewed head `f92f5131e7d2395235880dae0bc8594a93c56b43`, with the real `ViaL1Deposit::l1_tx` conversion.
All 240 transactions produce identical complete parser results. All 108 OP_RETURN deposit messages retain their
receivers and canonical hashes. No deposit message appears or disappears because of this correction.

This result closes the historical merge gate for current testnet4 through the cutoff. The correction needs no
activation branch if the network follows the coordinated rollout below. Other networks and independent
external-node snapshots were not audited and are outside this conclusion.

The census establishes agreement between the two decoders. It does not establish that a fresh rebuild reproduces
every stored identity or executed credit. A source revert cannot undo executed credits or persisted identities.

### Coordinated testnet4 rollout

Testnet4 uses a coordinated stop for this correction. Every external node participates. A rolling upgrade could
let an old process store the shifted receiver while a new process stores the corrected receiver for the same payment.
The main node and verifier skip deposits they have already stored, so a later upgrade does not repair that difference.
A census before the stop leaves this window open. A height gate would also require every node to upgrade before
that height, because an old binary does not know the gate exists.

The release owner must complete these steps in order:

1. Stop all Via node roles that parse, persist, execute, or verify Bitcoin deposits. Include the main node, every
   external node, all verifiers and coordinators, and the public indexer. Wait for all old processes and their database
   writes to finish. Prevent controllers and supervisors from restarting an old process.
2. After the full stop, record the Bitcoin tip height and block hash as the release cutoff. Refresh the census through
   that cutoff, including bridge and bootstrap history. Use raw bridge transactions, not just saved scanner cursors,
   because a stopped process can have stored deposits beyond its last saved cursor.
3. If either decoder produces a different message or canonical hash, keep every role stopped. Resolve the affected
   Bitcoin, parser, persisted-state, and execution identities before release, or select an explicit transition rule.
4. While every role remains stopped, install the corrected parser in every role. Verify each deployed artifact before
   any role resumes. If any node cannot join the stop and upgrade, this release procedure is blocked.
5. Resume only the upgraded roles. Keep old binaries disabled. Bitcoin can mine payments during the stop. The upgraded
   roles process those payments after restart, so no old parser records them during an upgrade overlap.

This procedure is a release requirement. The census at height 152,263 does not prove that the stop or upgrade occurred.
After the new parser has processed deposits, a return to an old binary needs a separate compatibility check.

[bitcoin-solver]: https://github.com/bitcoin/bitcoin/blob/v27.0/src/script/solver.cpp
[bitcoin-interpreter]: https://github.com/bitcoin/bitcoin/blob/v27.0/src/script/interpreter.cpp
[bitcoin-policy]: https://github.com/bitcoin/bitcoin/blob/v27.0/src/policy/policy.cpp
