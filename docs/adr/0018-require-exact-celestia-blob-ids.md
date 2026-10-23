---
status: accepted
date: 2026-10-23
---

# Require exact Celestia blob IDs and distinguish malformed references from RPC failures

## In short

Via's Celestia client accepts a blob ID only when it decodes to exactly 40 bytes: an 8-byte big-endian height followed
by a 32-byte commitment. Any hex letter case is valid. Invalid text, other lengths, malformed child IDs, and truncated
manifests return contextual non-retriable errors. RPC failures stay retriable. This changes accepted references, without
changing caller polling, voting, batch skipping, or fallback behavior.

## Background for a new reader

Via publishes batch data and proofs on Celestia. A Bitcoin inscription carries a blob ID that tells a reader where to
fetch that data. The height identifies a Celestia block, and the commitment identifies the blob within that block. The
ID is hexadecimal text representing one fixed layout. It has no version field or extension fields.

A large payload can use a manifest: a sequence of child IDs, each preceded by a four-byte big-endian byte length. The
generic manifest decoder returns complete entries of any length. The Celestia client applies its 40-byte rule to each
returned ID. A complete manifest entry and a valid Celestia reference are different checks.

A non-retriable error says that repeating the same local parse cannot repair the input. It does not decide the batch's
validity or dictate what its caller does next. Inclusion callers currently leave work pending and poll again. The
fallback client tries another source after either kind of primary error.

## What went wrong

The ID parser sliced decoded input before checking its length. Short valid hex could panic. A longer ID selected the
object named by its first 40 bytes, silently ignoring the suffix. Root and child parse errors were then wrapped as
retriable, even though an identical request could not repair the text.

The manifest decoder also sliced length prefixes and bodies without checking that the bytes existed. Truncation could
panic instead of returning an error. Its caller discarded the original decoding error, losing useful context.

## Decision

1. Decode Celestia IDs into a fixed 40-byte buffer. Reject shorter and longer IDs before requesting the blob they name.
   Accept lowercase, uppercase, and mixed-case hex. Keep the existing big-endian height and commitment bytes.
2. Return a contextual non-retriable error for root or child ID parsing and malformed manifest framing. Preserve the
   underlying error and identify the relevant reference. Keep RPC failures retriable.
3. Read each manifest length completely and check that its body is present. Preserve empty manifests, complete
   zero-length entries, and complete arbitrary-length entries in the generic decoder. Celestia validates the resulting
   IDs. Preserve envelope interpretation and raw historical data.
4. Preserve callers, sequential child fetches, polling, proof verdicts, votes, batch skipping, and fallback behavior. A
   permanent parsing error remains an error, without becoming a completed invalid-proof verdict.

## Considered options

**A. Exact 40-byte IDs and permanent local errors. Accepted.** This matches Via's fixed layout and distinguishes
deterministic input errors from RPC failures. It narrows the set of references that the Celestia primary accepts.

**B. Reject short IDs but preserve suffix parsing. Not chosen.** It preserves retrieval for existing over-long IDs, but
accepts multiple strings for the same object without a defined Via extension contract. A demonstrated historical
extension would require a compatibility decision.

**C. Reject or skip the batch, or vote `NotOk`. Not chosen.** Other rollups discard malformed inputs under their own
derivation rules. Importing that outcome would also require Via rules for canonical batch continuity, persistence,
voting, restart, and fallback. A parser fix does not settle those rules or turn unavailable evidence into a completed
proof verdict.

**D. Remove panics but keep parse errors retriable. Not chosen.** Repeating a local parse cannot repair malformed text
or framing. Existing inclusion pollers retry independently of the flag, so retaining the wrong classification provides
no scheduling benefit.

## Why this design

Established systems use both exact and permissive boundaries. The format's contract decides which rule fits, rather than
a universal Celestia-ID rule.

| Exact-width or complete-consumption boundary                                                                                                                       | What it enforces                                                                                                       |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------- |
| [Lumina commitment deserialization](https://github.com/eigerco/lumina/blob/f11959550851afb8c2062b30e38cc87242467cdc/types/src/blob/commitment.rs#L113-L131)        | The complete decoded commitment must fit a 32-byte hash. This is the commitment field, not Via's composite ID.         |
| [Nitro ReferenceDA certificate](https://github.com/OffchainLabs/nitro/blob/5712eef570d2c5a4c4842b1bb308f9613754be51/daprovider/referenceda/certificate.go#L73-L96) | Its fixed certificate must contain exactly 99 bytes.                                                                   |
| [hex 0.4.3 `decode_to_slice`](https://github.com/KokaKiwi/rust-hex/blob/b2b4370b5bf021b98ee7adc92233e8de3f2de792/src/lib.rs#L312-L327)                             | The text must encode exactly the output slice's length. Odd length and invalid digits return errors.                   |
| [OP frame parser](https://github.com/ethereum-optimism/optimism/blob/5de51bfd7d67ef42122db9015677da4d9c9896ab/op-node/rollup/derive/frame.go#L116-L154)            | The input must contain complete frames, with no partial frame or stray suffix. Complete additional frames are allowed. |

| Boundary that permits extra bytes                                                                                                                                          | What those bytes mean                                                                                                                                                                        |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Celestia `op-alt-da`, formerly `op-plasma-celestia`](https://github.com/celestiaorg/op-alt-da/blob/439e78893fc9836d2ce5f8b1893d5dbbb361b738/celestia_storage.go#L98-L130) | Accepts at least 40 bytes. At 48 bytes or more it reads share offset and size as an extension. It ignores unused bytes, including suffixes beyond those fields. Its height is little-endian. |
| [Rollkit Celestia adapter](https://github.com/rollkit/celestia-da/blob/40c34287cd362983575dfd9d335633935057bd96/celestia/celestia.go#L187-L200)                            | Passes all bytes after the eight-byte little-endian height as the commitment, without a local exact-width check. This does not prove the downstream RPC accepts arbitrary widths.            |
| [Nitro AnyTrust certificate parser](https://github.com/OffchainLabs/nitro/blob/5712eef570d2c5a4c4842b1bb308f9613754be51/daprovider/anytrust/util/util.go#L304-L362)        | Reads the required fields without requiring exhaustion of the supplied reader.                                                                                                               |
| [OP batch RLP specification](https://github.com/ethereum-optimism/specs/blob/21d1ff54af56c76883e201e63cc84f69e07461a9/specs/protocol/derivation.md#L437-L442)              | Allows extra content after a fully decoded batch. It does not permit arbitrary bytes between encoded batches.                                                                                |

Via has one fixed ID layout, with no version or extension. Celestia's OP adapter supplies a different format, including
optional fields and a different height byte order. Its extension cannot be imported by retaining an arbitrary suffix.
Adopt the existing hex decoder's exact-width operation and adapt checked manifest reads to Via's framing. No outside
client or protocol code needs to be copied.

Error disposition is also protocol-specific. The
[OP Alt-DA reader](https://github.com/ethereum-optimism/optimism/blob/5de51bfd7d67ef42122db9015677da4d9c9896ab/op-node/rollup/derive/altda_data_source.go#L65-L102)
discards malformed commitments and treats ordinary fetch failures as temporary.
[Nitro AnyTrust](https://github.com/OffchainLabs/nitro/blob/5712eef570d2c5a4c4842b1bb308f9613754be51/daprovider/anytrust/util/util.go#L186-L280)
returns empty payload for malformed certificates but propagates retrieval errors. Nitro's
[ReferenceDA reader](https://github.com/OffchainLabs/nitro/blob/5712eef570d2c5a4c4842b1bb308f9613754be51/daprovider/referenceda/reference_reader.go#L55-L72)
returns plain deserialization errors, whereas specifically labeled validation errors become empty batches in the
[inbox](https://github.com/OffchainLabs/nitro/blob/5712eef570d2c5a4c4842b1bb308f9613754be51/arbstate/inbox.go#L110-L135).
These examples support distinguishing failures, without prescribing Via's batch disposition.

## Consequences

- Short IDs and truncated manifests return errors instead of panicking.
- An over-long ID that previously resolved by prefix now leaves its batch pending when the Celestia error reaches the
  inclusion caller. The caller logs and polls again. This is an observable compatibility change.
- A configured fallback is still attempted and can provide data after the Celestia primary rejects an ID. Strict primary
  parsing does not establish rejection across every backend.
- Valid producer bytes, manifest serialization, legacy raw data, and child fetch order remain unchanged. Earlier valid
  children may have been fetched before a later invalid child is encountered.
- No new dependency, shared helper, caller rule, or metric contract is needed.

## Not decided here

- Whether malformed references should reject or skip a batch, or affect votes.
- A dedicated alert for a batch stuck on a malformed reference.
- The historical on-chain length census. Inspected built-in producers emit exactly 40 bytes, but that does not establish
  the width of every already inscribed reference or custom producer's output.

This decision authorizes no live deployment, historical replay, or database change.
