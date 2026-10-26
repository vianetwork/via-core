---
status: accepted
date: 2026-10-24
---

# The verifier set at a proof's inclusion block decides its finalization

## In short

The verifier set that is active at a proof's canonical inclusion block decides
who may vote on that proof and how many votes it needs. Every component uses
this one rule: the reader, recovery when it recomputes finalization, and the
zk verifier's final write. A configuration change included at height `h` takes
effect at `h + 1`, so the set for a proof included in block `p` is the
configuration through block `p - 1`. One shared tally counts each verifier
once for one exact proof and one set. The verifier set may not rotate while a
proof is unfinalized, and code enforces that. This changes how Via interprets
votes across a rotation.

## Background for a new reader

A proof message is a Via message on Bitcoin that references a batch's proof.
Verifiers vote on it, and a batch is finalized when the votes for its proof reach the
threshold of the verifier set. A governance-signed `UpdateBridge` message
included at height `h` activates a new verifier set from `h + 1`
([#451](https://github.com/vianetwork/via-core/issues/451)). No inscription
carries a set identity or epoch. The finalized batches form a prefix only
because one vote is in flight at a time, condition C4 on
[#426](https://github.com/vianetwork/via-core/issues/426).

[ADR 0013](0013-keep-evidence-membership-and-derived-state-apart.md) orders
configuration changes by Bitcoin position and keeps the `h + 1` activation.
This decision says which of those configurations judges a proof.

## What went wrong

- Three components pass three different sets when they count votes. The
  reader uses the set at the vote's block. The verifier reverter uses the set
  at the reorg start minus one
  (`via_verifier/node/via_block_reverter/src/lib.rs`). The zk verifier uses a
  copy taken at process start (`via_verifier/node/via_zk_verifier/src/lib.rs`).
- After a reorg, a replay, or a verdict that arrives after the deciding vote,
  the same batch can be final in one component and not in another. With two
  stored votes, a three-member set reaches the threshold and a five-member set
  does not.
- Main-node finalization is never recomputed after a reorg.
- The main node counts votes per batch instead of per proof
  ([#454](https://github.com/vianetwork/via-core/issues/454)).
- The set lookup orders rows by `created_at`, which ties within one scan
  transaction.

## Decision

1. **The set active at the proof's canonical inclusion block decides.** It
   fixes both membership and the denominator for that proof, in every
   component: the reader, recovery recomputation and the zk verifier's final
   write. With `h + 1` activation, the set for a proof in block `p` is the
   configuration through block `p - 1`. If that history is missing, the
   lookup returns an error. It never falls back to the latest set or the set
   at process start.
2. **One shared tally per exact proof and set.** Each verifier counts once.
   The main-node count across a whole batch
   ([#454](https://github.com/vianetwork/via-core/issues/454)) is fixed by this
   tally. Each role keeps its own proof-verification prerequisite before it
   counts.
3. **Wait for complete evidence and decide from one fixed prefix.** For a
   rotation in block h, use the canonical prefix through the end of h,
   including every proof and vote in that block. Proofs use the set active at
   their inclusion, so a proof in h uses configuration through h - 1. Wait
   with `NotReady` if prefix ingestion, earlier configuration decisions or
   required local proof/deposit evidence is incomplete. Local worker timing
   or unavailable DA never causes permanent rejection.

   Once those inputs are complete, reject the occurrence for good if any
   proof included through h lacks its exact-proof quorum in that prefix;
   otherwise accept it from h + 1. Governance sends a new message after a
   rejected occurrence. Later votes cannot rescue it. For this interpretation
   decision, the quorum includes canonical prefix votes without a maturity
   filter. Actual finalization and authority based on the new configuration
   still require N-deep canonical dependencies under ADR 0009. A reorg
   rewinds the provisional interpretation with its source blocks. This
   proposed variant avoids an extra N-block proof-free interval while keeping
   irreversible actions behind maturity. If required proof DA is permanently
   gone and no sufficient kept verdict exists, `NotReady` can last forever.
   This changes which messages Via accepts; the exact variant needs approval.

This changes consensus interpretation across rotations. Today each vote is
admitted under the set at its own block. After this decision, every vote for a
proof is judged by the set at the proof's inclusion. The wire format of votes
and proofs does not change.

## Considered options

**The set at each vote's block (today).** Rejected. Votes for one proof can be
judged by different sets, and replay can change the result.

**The set at process start, or at the replay tip.** Rejected. Neither replays
the same way twice.

**The set at the first vote.** Rejected. It ties membership to whichever vote
lands first, not to the proof that the votes judge.

**An explicit epoch field in Via messages.** Rejected. It changes the wire
format.

**A joint quorum of old and new sets.** Rejected. Raft, etcd and ZooKeeper
need joint quorums because replicas switch at different times. Every Via node
derives the set from the same Bitcoin prefix.

**Unlimited authority for a retired set.** Rejected. A pre-rotation set could
finalize an old proof at any later time.

**A fixed expiry for a retired set.** Rejected. It adds a time rule with no
requirement behind it. Forbidding rotation while a proof is unfinalized bounds
the window without one.

**Hold a forbidden rotation and activate it when the proof is finalized.** Not
chosen. The activation point would depend on finalization timing instead of
the message's own block, which makes replay harder to reason about.

## Why this design

Three established systems judge a decision by the membership that applies to
the object the decision refers to, not by the membership a node holds now:

- **CometBFT** loads the validator set for a height with
  [`LoadValidators(height)`](https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/state/store.go#L548-L615),
  returns an error when the set is missing, and verifies a commit against one
  set for both the signers and the threshold, rejecting duplicate voters
  ([`validation.go`](https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/types/validation.go#L65-L138);
  Go, Apache-2.0). Adapted: Via adopts the lookup and tally contract on its
  existing wallet table and vote DAL. CometBFT applies validator changes at
  `h + 2`, because each header already commits to the next set. Via has no
  such commitment and keeps `h + 1`.
- **Lighthouse** selects the committee for an attestation from the target the
  attestation names, and returns an error instead of using the head when that
  history is missing
  ([`attestation_verification.rs`](https://github.com/sigp/lighthouse/blob/e423a66763bb1bd780492d635123f208d80c3538/beacon_node/beacon_chain/src/attestation_verification.rs#L1580-L1622);
  Rust, Apache-2.0). It also bounds old authority to the current and previous
  epoch. Idea only: Via bounds it by forbidding rotation instead.
- **ZooKeeper** fixes the required quorum on each proposal when the proposal is
  created
  ([`Leader.java`](https://github.com/apache/zookeeper/blob/777b3723f56456524a052b79f73649e99da1f1aa/zookeeper-server/src/main/java/org/apache/zookeeper/server/quorum/Leader.java#L1316-L1325);
  Java, Apache-2.0). Idea only. For Via, a proof is created when Bitcoin
  includes it.
- **Raft** refuses a new configuration change while an earlier one is still
  pending and drops it instead of queueing it
  ([`raft.go`](https://github.com/etcd-io/raft/blob/1931ff5beacc1e2539003c0c70f096285f04c425/raft.go#L1310-L1345);
  Go, Apache-2.0). Idea only, for rejecting a forbidden rotation.

No donor decides Via's anchor. Choosing the proof's inclusion block is Via
protocol policy.

Compared with what Via had, every component reaches the same verdict for the
same proof, after a restart, a replay or a reorg. A vote for a different proof
of the same batch no longer counts toward it. A retired set cannot finalize an
old proof indefinitely. The
[research note](../research/bitcoin-reorg-handling.md#decomposed-prior-art-per-sub-problem)
lists every donor with its licence and verdict.

## Consequences

- The reverter's and the zk verifier's set lookups change to the shared
  lookup. The main node gets the shared tally.
- Main-node finalization, which recovery never recomputes today, follows the
  same rule wherever it is computed.
- Governance cannot rotate the verifier set while a proof waits. A stuck
  proof therefore also delays a rotation.
- Every node must adopt the rule together, by the stop, migrate and restart
  upgrade in [ADR 0010](0010-commit-each-scan-group-with-its-position.md).

## Not decided here

- Whether any `UpdateBridge` message has been included on testnet4. That
  decides whether past votes crossed a rotation, and needs a chain census.
  Before authorizing reset, a read-only census of an isolated testnet database
  copy and its matching Bitcoin history must identify every historical rotation,
  its prefix proofs, retained verdicts and required DA availability. Missing
  evidence leaves the reset blocked; it is not a negative configuration verdict.
  The maintainer must authorize that census separately. It has not run.

This decision builds on
[ADR 0010](0010-commit-each-scan-group-with-its-position.md) and
[ADR 0013](0013-keep-evidence-membership-and-derived-state-apart.md). It
authorizes no live deployment or database change. Decision ticket:
[#440](https://github.com/vianetwork/via-core/issues/440). Evidence:
[#451](https://github.com/vianetwork/via-core/issues/451) and
[#459](https://github.com/vianetwork/via-core/issues/459).
