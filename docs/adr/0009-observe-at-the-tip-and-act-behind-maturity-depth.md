---
status: accepted
date: 2026-10-22
---

# Observe Bitcoin at the tip and act only behind a maturity depth

## In short

The Bitcoin reader follows the chain tip and records there only facts it can
undo. Actions that cannot be undone, such as votes, finalization and payouts,
wait until the block that justifies them has reached the network's maturity
depth. The inclusion block counts as the first confirmation. The depths are 6
on mainnet, 100 on testnet4, 100 on signet and 1 on regtest. The same depth
decides when the sender treats its own transaction as complete. How far back
recovery may search for a fork point is a separate setting, the
rollback-search limit.

## Background for a new reader

A Bitcoin block can be replaced by a reorg until enough blocks are built on
top of it. How deep reorgs go depends on the network
([#441](https://github.com/vianetwork/via-core/issues/441)):

- Mainnet had 15 reorgs in the last 365 days. The deepest replaced 2 blocks.
- Testnet4 reorgs about 15 times a day. About 0.8 reorgs a day are 12 blocks
  or deeper, and the deepest since September 2024 replaced 73 blocks. None
  reached 100 blocks in about 25 months.
- Since June 2026 signet has had about one deliberate one-block reorg a day.
  Its deepest recorded reorg, in 2024, replaced 22 blocks. Its operator
  reported an accidental branch of 163 blocks in June 2026.

Today a Via reader reads only up to `tip - block_confirmations` and acts on
everything it reads. The sender separately waits until its own transaction has
`block_confirmations` confirmations, using Bitcoin Core's count, in which the
inclusion block is the first. The repository's base configuration sets both
values to 0. The deployed testnet uses 12 for both. A deployment configuration
change on 24 December 2025 raised them from 6 to 12 with no recorded reason,
most likely because of testnet4's reorgs. No mainnet deployment exists.
Separately, both reorg detectors re-check a window of the last 100 blocks,
hard-coded in `reorg_window()`.

## What went wrong

One number did two jobs. Staying 12 blocks behind the tip delayed the moment a
reader saw a fork, because it never looked at the newest 12 blocks. Acting on
everything it read assumed that 12 blocks are final. Testnet4 breaks that
assumption about once a day.

The number was also counted two ways. The reader first sees a block when the
block has 13 confirmations, because it stops at `tip - 12`. The sender
completes at 12 confirmations. Nothing tied either value to a network, and 12
was never chosen for mainnet at all. The recovery window of 100 blocks was a
third number, hard-coded and not tied to either.

## Decision

1. **Observe at the tip.** The reader follows the tip and records only
   reversible facts there.
2. **Act behind a maturity depth.** Irreversible actions, namely votes,
   finalization and payouts, wait until their block reaches the network's
   maturity depth.
3. **Count inclusively.** The inclusion block is confirmation 1. A block at
   height `h` is mature when it is on the authoritative node's active chain and
   `tip - h + 1 >= N`. A height above the tip is never mature.
4. **One depth per network:**

   | Network | Maturity depth N |
   | --- | --- |
   | mainnet | 6 |
   | testnet4 | 100 |
   | signet, if used | 100 |
   | regtest, isolated tests only | 1 |

5. **One policy for authority and sender completion.** The same depth decides
   when a source grants authority and when the sender treats its own
   transaction as complete. There are no separate depths per action.
6. **Protective steps do not wait.** Observing at the tip, adding a
   preventive hold and revoking authority whose source left the chain happen
   immediately.
7. **Keep the rollback-search limit separate.** How far back recovery may
   search for a fork point is its own setting, not derived from the maturity
   depth. [ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md)
   sets it.

## Considered options

**Read only to `tip - N` and act on everything read (today).** Rejected. The
reader sees forks late, and it treats N blocks as final, which testnet4
breaks about once a day.

**Act at the tip and compute confirmations at query time**, as BDK and
NBXplorer do. Rejected. That model suits wallet balances, which can change
back. It does not suit votes and payouts, which cannot.

**12 on every network.** Rejected. Testnet4 exceeds it about daily, and the
two ways of counting remain.

**12 on mainnet.** Rejected by amendment. No record justifies 12 for mainnet.
The value came from the testnet change described above, and testnet4's depth
of 100 now covers that reason.

**Citrea's table copied unchanged** (6 mainnet, 100 testnet4, 5 signet).
Rejected. A signet depth of 5 is below signet's recorded reorgs.

**A lower testnet4 value, 6 to 30.** Rejected after it was weighed against the
latency of 100 (about 18 hours). Other projects accept the risk on test
networks: Citrea's own Clementine bridge uses 6 on testnet4, Rootstock 10 on
its testnet and Alpen 6 on signet. Testnet4 still produced reorgs of 59, 53 and
50 blocks in 2026. Below about 60, each of those makes Via act on a block that
disappears, and the new recovery stops for manual repair. On a public testnet
that means operator work and downtime several times a year, which costs more
than the slower feedback. Citrea reached 100 the same way: 8 at first, 30 on
2024-12-13 after a 12-block reorg, and 100 on 2025-04-25 after a 30-block
reorg, accepting the longer latency in its documentation. Via's own testnet
values (0, then 3, 6 and 12 during 2025) carried no recorded reason.

**A separate depth per action.** Rejected. Each would be a setting with no
requirement behind it.

## Why this design

Every Bitcoin layer 2 studied separates seeing a block from acting on it.
Citrea's `bitcoin-da` uses a `finality_depth` of 6 on mainnet and 100 on
testnet4, with an inclusive finalized height. Alpen gates actions through one
inclusive predicate, `is_l1_reorg_safe`, which returns nothing for a future
height. Clementine counts the inclusion block as confirmation 1. Babylon waits
before an action grants power and acts at once on one that revokes. LDK
matures on-chain events after `ANTI_REORG_DELAY = 6`. Six confirmations is the
long-used Bitcoin convention and the mainnet value of Citrea, Alpen and
Clementine. The
[research note](../research/bitcoin-reorg-handling.md) links each source at a
fixed commit.

Compared with what Via had, a fork is visible as soon as the authoritative
node sees it, and the depth that protects an action is written per network
with one counting rule. Mainnet deposits, finalization and withdrawals wait
about half as long as a 12-block rule would make them wait. Testnet actions
wait longer, but the depth exceeds every testnet4 reorg recorded so far.

## Consequences

- The reader sees a fork when its authoritative Bitcoin node does, not 12
  blocks later.
- The reader is exposed to every shallow reorg. On testnet4 recovery runs about
  15 times a day, so recovery is a routine path, not an exception.
- On testnet4 and signet, a vote waits for 100 blocks. Testnet4 produces a
  block about every 11 minutes, so that is about 18 hours.
- The depth values are policy. Historical depths do not bound future reorgs,
  and a signet operator can produce deeper branches.
- The testnet4 value is a workaround for testnet4's minimum-difficulty rule. A
  move to a test network without it (BIP 95 testnet5, still a draft, or
  signet) is the structural fix and a reason to revisit this table.
- Operators configure two values per network with different meanings: the
  maturity depth and the rollback-search limit.
- `docs/via_guides/reorg.md` states 6 confirmations for deposits. It must
  describe this policy when the change is implemented.

## Not decided here

What happens to a reorg deeper than the rollback-search limit, and which state
recovery may repair automatically, are decided in
[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md) and
[ADR 0014](0014-repair-only-reversible-provable-steps-and-inspect-read-only.md). Moving the public
testnet from testnet4 to signet is outside this decision.

This decision builds on
[ADR 0007](0007-serialize-verifier-scans-and-cap-recovery.md) and
[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md). It changes
no deployed setting. Decision tickets:
[#435](https://github.com/vianetwork/via-core/issues/435) (observe and act) and
[#452](https://github.com/vianetwork/via-core/issues/452) (depths, unit and
sender completion, with the mainnet amendment).
