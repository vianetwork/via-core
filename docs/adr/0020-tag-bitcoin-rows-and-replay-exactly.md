---
status: accepted
date: 2026-10-24
---

# Tag Bitcoin rows and replay them without losing local history

This supersedes [ADR 0013](0013-keep-evidence-membership-and-derived-state-apart.md). Every Bitcoin-derived reader row records origin height and hash. On an eligible reorg, delete replaced reversible rows and rewind reader/affected consumer progress in one SQLx transaction, then rescan normally. Keep prior configuration versions so suffix deletion restores the earlier choice. Midnight's transactional block/child save and Era's ordered multi-DAL deletion are adapted to Via's schema. There is no universal evidence/membership/derived split.

Replay compares every immutable payload field under a retained identity. Exact equality keeps execution and completed verdict fields unchanged; a mismatch returns a named first-field conflict without overwrite. In particular, replay never resets an executed upgrade, fixing #443. A vanished unexecuted deposit leaves the queue even when no batch references it. A re-mined `(txid, vout)` never becomes another L2 credit for an already executed deposit.

Configuration follows branch-bound Bitcoin block, transaction and message order. System-message parsing records block hash and transaction index. Changes included at h take effect at h + 1. Two distinct changes for the same role in one block cause none of them to take effect, even with equal values; exact replay of one occurrence is not a second occurrence. Timestamp order is not authority. [ADR 0015](0015-verifier-set-at-proof-inclusion-decides-finalization.md) defines the historical proof set and deferred rotation decision.

Existing stores upgrade with a format check, selective reset and ordinary replay. Keep signing attempts/risk/nonces/reservations/holds, executed or attached deposits/upgrades, completed proof verdicts and their origin/policy, in-flight inscriptions, DA IDs, and every FK parent needed by those rows. Preserve sender requests/history without deletion cascades. Retain expected withdrawal/origin/batch and observation/output/inclusion parents referenced by reservations or fulfillment history. Never TRUNCATE CASCADE or reset identity sequences. Kept rows are history until current source and replay readiness authorize their use.

Reconcile each kept executed deposit by txid, vout, amount, retained receiver and the L2 operation already executed. Join it to the actual Bitcoin payment and retained execution evidence; do not re-decode historical deposits with today's parser or synthesize a new L2 operation. Preserve its old priority identity and L2 hash, including after a matching remine. A new position alone is not a second credit or a conflict with those execution IDs. Missing payment, ambiguity or a real mismatch in retained payment/execution identity stops reconciliation. An authorized read-only testnet-copy census is a precondition of reset approval, including historical rotations and missing DA. Do not discard completed verdicts because their old DA bytes may be unavailable, promote legacy/development verdicts, or guess new proof evidence.

Authority stays disabled until normal replay reaches its checked cutoff and reconciles every required kept fact. A new format version is not completion. Missing required source or DA leaves the store held with data intact. The precise allowlist follows these kept roots and their transitive parent closure; unknown dependencies refuse reset rather than expand deletion. This trades a general migration interpreter for ordinary replay plus explicit retained-state checks, without replacing the existing testnet.

Amended 2026-10-26: one ballot is keyed by exact proof and member. An identical
repeat is ignored without another weight or a chain lookup; a different
ballot from that member stops the node for diagnosis. This follows
[CometBFT AddVote](https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/types/vote_set.go).
Via compares authenticated vote input: a new Bitcoin inscription has a different
inclusion and transaction witness signature, which are not a new ballot. The
first canonical occurrence remains stored. This refines the earlier rule that
every second occurrence conflicts.

Only rotation evidence excludes proofs already finalized before the reset cutoff. These are pre-#469 completed history whose expired testnet DA prevents re-verification. They remain unproven, with their original row IDs and FKs; no proof incarnation or legacy recomputation route is introduced. A rotation at h is judged on canonical proofs after that cutoff through h, which the verifier verifies itself. Mainnet starts without such legacy rows. This exception grants no voting, signing, finalization or withdrawal authority.
