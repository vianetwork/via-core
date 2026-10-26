---
status: accepted
date: 2026-10-24
---

# The shared Bitcoin reader lives in `via_reorg` and copies LDK's fork walk

Amended 2026-10-24: one reader position replaces scan groups; the N/frontier
boundary replaces the search limit; copied code uses pinned comments and a
short attribution list without automated provenance or drift tooling.


## In short

The one Bitcoin reader from
[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md) is written
once, in the existing crate `core/lib/via_reorg`. The main node, the external
node and the verifier plug in their own processors through small adapters. The
code that finds a fork point is copied from LDK, with a pinned origin. Via takes no runtime dependency on LDK or BDK. Block headers come from
the authoritative Bitcoin node through `via_btc_client`. The reader checks each
header's identity and parent link but does not check proof of work itself.

## Background for a new reader

Today the main node and the verifier each carry their own reader, detector,
reverter and startup code, in `core/node/` and `via_verifier/node/`. ADR 0007
and PR #404 had to apply the same rules to both trees by hand. The repository
rules in `AGENTS.md` place shared behavior in `core/lib/` and keep
node-specific scheduling, state and shutdown in the node layer.

Three pieces of shared code exist:

- `core/lib/via_reorg` is a dependency-free crate with one comparison,
  `scan_for_reorg`, which returns the first height at which stored and fetched
  hashes differ, and one helper, `reorg_window_start`. Both detectors use it.
- `core/lib/via_btc_client` is the Bitcoin RPC client. It also contains the
  inscription indexer, which parses Via messages out of blocks. It wraps the
  archived `bitcoincore-rpc` crate, whose types must not leak to callers.
- Each tree has its own `MessageProcessor` trait. The two traits differ by DAL
  connection and error type, and both let a processor call Bitcoin RPC while
  the scan transaction is open.

## What went wrong

Every reader fix had to land twice and stay identical. The two readers each own
their transaction, configuration loading, block fetch, processor dispatch and
scan position update. A fix in one tree that is missed in the other is a new
bug. The shared crate `via_reorg` only compares hashes inside a window, so it
cannot prove a fork point, and nothing shared owns the order of reading,
detecting and recovering.

## Decision

1. **The shared reader grows inside `core/lib/via_reorg`.** That crate already
   owns the shared chain comparison. It becomes the reader: block identities,
   full ancestry, ordered apply and undo, and one reader position with
   trailing consumer completion. It does not import either node's DAL.
2. **Each node supplies fallible adapters.** Prepare hash-bound block input
   outside SQL, then apply immediate rows and the one reader position in one
   transaction. Slow consumers follow with their own progress under
   [ADR 0019](0019-one-reader-position-with-trailing-consumers.md). Scheduling,
   shutdown and per-node delete lists stay in the node layer. Identical reader
   state SQL is shared once over `Connection<'a, DB: DbMarker>` in a small
   shared storage crate; SQL stays outside pure `via_reorg`.

3. **The fork walk is copied from LDK.** The copy keeps LDK's loop and its
   traversal order unchanged: walk back the side with the greater height, or
   both sides at equal height, until the hashes match. Via adapts the header
   type, the lookups (stored side from Via's ancestry table, node side through
   `via_btc_client`), the error type and the N/confirmed-frontier boundary from ADR
   0021. Each copied function carries an origin comment with the pinned
   permalink and the changes made.
4. **No BDK or LDK runtime dependency.**
5. **Headers come from the authoritative node through `via_btc_client`.** The
   client returns hash, height, parent hash and canonical membership
   (`confirmations`) for a header requested by hash. Raw `bitcoincore-rpc`
   types stay inside the client. The reader checks that the returned hash is
   the one it asked for and that each header links to its parent. Independent
   proof-of-work and difficulty validation is outside this redesign: Bitcoin
   Core is the authority on which chain is valid.
6. **The old window API goes.** `scan_for_reorg` and `reorg_window_start` are
   removed when their callers move to the reader. No compatibility wrapper
   stays.

## Considered options

**A new crate, such as `via_btc_reader`.** Rejected. It would be a second
owner of the same comparison, with no benefit over growing the existing one.

**Recovery inside `via_btc_client`.** Rejected. It mixes recovery and DAL
concerns into the transport client that every Bitcoin caller uses.

**Two node-specific readers.** Rejected. It keeps the duplication that let the
trees drift.

**Depend on LDK `lightning-block-sync`.** Rejected
([#450](https://github.com/vianetwork/via-core/issues/450)). The fork walk is
crate-private. It is reachable only through the `lightning::chain::Listen`
callbacks, which take `&self`, return nothing and have no error or transaction
path, so they cannot carry a SQL transaction or a processor error. The crate
pulls in the whole `lightning` crate, about 260,000 lines, and a second RPC
client. Each LDK minor release breaks its block-source API.

**Depend on BDK `bdk_bitcoind_rpc`.** Rejected. It is blocking and talks to the
raw `bitcoincore-rpc` client, so it bypasses Via's client, retries and RPC
metrics. When it finds no agreement point it restarts from genesis without a
signal, which ADR 0008 forbids.

**Depend on BDK `bdk_chain`.** Not now. It compiles on Via's toolchain and adds
only two small crates. But it is an in-memory chain of `(height, hash)` pairs
without parent hashes, it needs a genesis anchor, and it would duplicate the
ancestry Via must store in SQL. BDK's next line needs Rust 1.85, and Via builds
with 1.82.

## Why this design

The algorithm Via needs is about 30 lines of logic. LDK's
`ChainDifference`, `find_difference_from_header` and `look_up_previous_header`
implement it in
[`lightning-block-sync/src/lib.rs`, lines 329 to 457 at `v0.3-rc3`](https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/lib.rs#L329-L457).
LDK is licensed MIT or Apache-2.0 at the user's option, the same as Via, and
its `LICENSE.md` refers to version control for authorship. LDK and BDK both
separate the block source from the chain logic, which is the split this
decision makes between `via_btc_client` and `via_reorg`. Copying the small
loop keeps a tested traversal order. A dependency would need adapter code
larger than the copy and would bring an API that cannot return Via's errors.

Compared with what Via had, one implementation of reading, fork search and
recovery serves every node. A fix lands once. The fork point is proved by
matching hashes instead of assumed at a window edge. The archived RPC types
stay where they are. The
[research note](../research/bitcoin-reorg-handling.md#library-fit-for-the-shared-reader)
records the dependency probes and links each source at a fixed commit.

## Consequences

- `via_reorg` changes from a pure helper into the reader, and its description
  changes with it.
- Both nodes' readers, detectors, reverters and startup code move behind the
  shared reader. Main-node and verifier code that does the same thing stops
  being duplicated.
- `via_btc_client` may need a `getblockheader` method that returns hash,
  height, parent hash and confirmations.
- LDK's proof-of-work and chainwork checks are not copied. The reader relies
  on its authoritative Bitcoin Core node, which validates the chain it reports.
- If LDK changes the walk, Via does not get the change automatically. The
  origin comment names the commit to compare against, for manual source comparison. No provenance executable, automated textual
  gate, weekly drift checker or donor-issue automation is included.
- The root already ships LICENSE-APACHE and LICENSE-MIT. Keep a short
  component/copyright/licence/pin list, preserve source notices and include
  any applicable donor NOTICE. Do not duplicate the full licence texts.

## Not decided here

- Whether Via revisits `bdk_chain` if its toolchain reaches Rust 1.85.
- How a consumer in another process with its own database follows the reader.
  It stays open on the
  [reader map](https://github.com/vianetwork/via-core/issues/426).

This decision builds on
[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md) and
[ADR 0010](0010-commit-each-scan-group-with-its-position.md). It authorizes no
live deployment or database change. Decision ticket:
[#433](https://github.com/vianetwork/via-core/issues/433). Evidence:
[#450](https://github.com/vianetwork/via-core/issues/450).
