---
status: accepted
---

# Reject invalid deposit address lengths without stopping Bitcoin scans

Bitcoin senders choose their deposit notes, so a payment to the bridge does not guarantee a valid deposit message.
The parser rejects the affected encoding when an address length is invalid and continues with other messages.
Retrying the same bytes cannot repair the note and would let its sender prevent progress through later Bitcoin blocks.

## Decision

[PR #390](https://github.com/vianetwork/via-core/pull/390) replaces the deposit address-length panics with rejection in
the shared `MessageParser`. An independently valid companion message remains eligible for acceptance.
The [deposit encoding reference](../../core/lib/via_btc_client/DEV.md#deposit-decoding) defines the length checks.

RPC and database failures that reach a scanner still leave its block range unfinished for retry.
The scanner saves its cursor only after its message processors succeed. A diagnostic can record a rejected note
without making that rejection fail the block.

PR #390 preserves the existing receiver interpretation and selection of inscriptions and OP_RETURN outputs.
Changing those rules can alter previously decoded deposits and their canonical transaction hashes.
Such a change needs a separate decision about historical replay and any protocol transition.

## Consequences

A [rejected bridge payment](../../CONTEXT.md) receives no L2 credit.
Rejection does not reverse the Bitcoin payment or trigger a refund.

This decision covers deposit address-length failures. It does not establish that every malformed Bitcoin message
is safe to process. The [bridge guide](../via_guides/bridging.md#why-an-invalid-note-can-block-every-restart) explains
the failure across restarts and the difference from zkSync Era's event decoder.
