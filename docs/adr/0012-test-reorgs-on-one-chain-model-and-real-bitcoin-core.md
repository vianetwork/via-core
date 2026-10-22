---
status: accepted
date: 2026-10-22
---

# Test reorgs on one shared chain model and four real Bitcoin Core cases

## In short

The shared Bitcoin reader is tested in two layers. Most cases run against one
in-memory chain model in `core/lib/via_test_utils`, which replaces the three
separate mock chains Via has today. Its fork cases are copied from LDK, and a
restart regression is adapted from BDK. Four cases run against a real Bitcoin
Core node in regtest mode, including a natural competing fork. Both layers are
required checks on pull requests that change the reader.

## Background for a new reader

A reorg test needs a chain that changes under the reader. Bitcoin Core offers
two ways to make one in regtest, a local test network where blocks can be
mined on demand. `invalidateblock` marks a block invalid, so the node drops it
and its descendants. Submitting a competing branch with more work makes the
node switch to it through its normal chain selection.

Via has three chain fixtures today, each with its own model:

- `core/node/via_btc_watch/src/test/checkpoint.rs` for the main-node reader.
- `via_verifier/node/via_btc_watch/src/test/` for the verifier reader.
- `via_verifier/node/via_reorg_detector/src/tests.rs`, which seeds mismatching
  hashes instead of changing a chain.

The main-node detector has no tests. No test calls `invalidateblock`,
`reconsiderblock` or `preciousblock`. A manual two-node procedure in
`docs/via_guides/test-reorg.md` and `etc/via_reorg/reorg.sh` has no assertions
and does not run in CI. Pull requests run no Rust tests, and `tests-via.yml`
runs only when started by hand
([#445](https://github.com/vianetwork/via-core/issues/445)).

## What went wrong

The verifier scan on `main` stops at the first block with a bridge withdrawal
([#442](https://github.com/vianetwork/via-core/issues/442)), and that change
reached `main` with no failing check. The batch 265 skip
([ADR 0007](0007-serialize-verifier-scans-and-cap-recovery.md)) was found on
testnet after 11 days, not by a test. The two detector defects in
[#444](https://github.com/vianetwork/via-core/issues/444), a shrinking tip and a
fork deeper than the window, were found by reading code. None of the three
fixtures models the authoritative node's `confirmations: -1` answer, so no test
could check the rule in
[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md).

## Decision

1. **One chain model in `core/lib/via_test_utils`.** The three chain fixtures
   merge into one model. The existing `BitcoinRpcServer` stays the transport
   fixture. Assertions on each node's database rows stay with the node that
   owns them. The maintainer approved merging this main-node and verifier test
   code.
2. **Copied and adapted cases.** LDK's eight applicable fork cases are copied:
   same chain, different chains, equal-length fork, shorter fork, longer fork,
   chain without headers, no new blocks and some new blocks. Their assertions
   check Via's chain difference and committed rows instead of LDK listener
   calls. BDK's restart regression from PR #2167 is adapted: restart the
   reader with a configured start above the fork point and check that no
   invalidated height is skipped.
3. **Four real Bitcoin Core cases:**

   | Case | What it does and checks |
   | --- | --- |
   | R1, disconnect-only shrink | `invalidateblock` without mining a replacement. The stored tip gets `confirmations: -1`, the recovery barrier stays until recovery completes, and no scan position moves past unread blocks. |
   | R2, natural competing fork | Build a valid branch from a known ancestor, extend the current branch, then submit the longer branch. The node switches through its own chain selection, and the reader undoes and applies the right blocks. |
   | R3, fork beyond the limit | Submit a branch whose real fork point is deeper than the rollback-search limit. The reader stops loudly and does not invent a partial fork point. |
   | R4, re-mined message and restart | Replace the branch that holds a Via message with a branch that holds the same message in another block, restart the reader on the same disposable database, and check the new inclusion and scan positions. |

   R2 and R3 translate Bitcoin Core's `create_empty_fork` test helper into
   Rust, keeping its names and order.
4. **Required on reader pull requests.** CI provides a pinned Bitcoin Core
   binary and a disposable database. If it cannot provide them, the job fails
   visibly instead of skipping. This depends on
   [#445](https://github.com/vianetwork/via-core/issues/445). Before the build,
   the `bitcoind` test crate from rust-bitcoin's `corepc` is checked for fit
   with Via's toolchain.

## Considered options

**A mock chain only.** Rejected. Nothing would check the model against real
Bitcoin Core RPC answers or chain selection.

**`invalidateblock` cases only.** Rejected. Bitcoin Core's own tests moved away
from it for reorgs, because forced invalidation skips validation paths that a
real competing fork takes. Via keeps it only for R1, where a shrink with no
replacement is the behavior under test.

**Two-node Docker tests only**, automating today's manual runbook. Rejected.
Fixed waits and peer-to-peer timing make failures slow and racy, and they hide
the exact commit or lock step that failed.

**Importing LDK's or BDK's test environments.** Rejected. They assert on their
own listener and emitter APIs, which Via does not use, and they bring
dependencies that [ADR 0011](0011-shared-reader-in-via-reorg-with-copied-ldk-walk.md)
rejected.

## Why this design

Established projects run most reorg cases on an in-memory chain and keep a few
on a real node. LDK's `Blockchain` test fixture supports `fork_at_height` and
`disconnect_tip`, and its
[fork tests](https://github.com/lightningdevkit/rust-lightning/blob/d7a980641c6325ddda5072a0f470ef8392676cf5/lightning-block-sync/src/lib.rs#L620-L790)
cover shorter, equal-length and longer forks. They are MIT or Apache-2.0, so
they can be copied with their origin. BDK
[PR #2167](https://github.com/bitcoindevkit/bdk/pull/2167), merged on
2026-04-24, fixed an emitter that skipped invalidated heights when it started
above the fork point, the same failure class as the batch 265 skip. Its
[regression test](https://github.com/bitcoindevkit/bdk/blob/d02f8909171af695fcb078c6fca082833f835b86/crates/bitcoind_rpc/tests/test_emitter.rs#L671-L709)
is MIT or Apache-2.0. Bitcoin Core
[PR #32587](https://github.com/bitcoin/bitcoin/pull/32587), merged on
2025-11-26, replaced `invalidateblock`-based reorg simulations with real
competing forks built by
[`create_empty_fork`](https://github.com/bitcoin/bitcoin/blob/79d6e874e1dad6822b5d7e6993aa205d21ed2cbf/test/functional/test_framework/blocktools.py#L119-L137),
which is MIT.

Compared with what Via had, one chain model replaces three, every rule in ADR
0008 has a deterministic case, and four cases check the real RPC boundary.
None of today's reorg behavior is checked automatically. After this decision a
reader change cannot merge without these checks. The
[research note](../research/bitcoin-reorg-handling.md#tests) links each source
at a fixed commit.

## Consequences

- The three existing chain fixtures are deleted after their callers move to
  the shared model.
- Reader pull requests need a runner that can start Bitcoin Core and a
  PostgreSQL database.
- Copied test code carries origin comments with pinned permalinks.
- The client's existing Docker-based regtest helper is repaired or retired
  when the runner is chosen, so Via keeps one way to start Bitcoin Core in
  tests.

## Not decided here

- Which runner image and resources provide Bitcoin Core and the database
  ([#445](https://github.com/vianetwork/via-core/issues/445)).
- How tests inject a crash in the middle of recovery.
- Whether the manual two-node runbook stays as an operator exercise.

This decision builds on
[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md) and
[ADR 0011](0011-shared-reader-in-via-reorg-with-copied-ldk-walk.md). It
authorizes no live deployment. Decision ticket:
[#437](https://github.com/vianetwork/via-core/issues/437). Evidence:
[#430](https://github.com/vianetwork/via-core/issues/430).
