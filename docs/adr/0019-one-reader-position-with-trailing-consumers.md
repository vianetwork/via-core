---
status: accepted
date: 2026-10-24
---

# Keep one reader position and let consumers trail it

This supersedes [ADR 0010](0010-commit-each-scan-group-with-its-position.md). Each node has one Bitcoin reader position, the highest canonical header committed with that block's immediate reader rows. Proof verification and other slow consumers keep their own durable progress or completion state. Missing DA or a proof gap does not stop header observation or independent withdrawal observation. Dependent interpretation waits when historical configuration is unresolved; reader head alone never certifies it ready.

Prepare Bitcoin/DA input and long computations outside write transactions. Commit reader rows, ancestry and position together. Commit consumer output and its completion together. Reconcile a consumer's saved source with canonical headers before catch-up and rewind its abandoned completion in the suffix transaction. A completed maximum does not prove the missing earlier work is complete.

At final save, take the existing owner/source gate through commit and revalidate the captured row/header incarnation, pending status and current readiness/canonical maturity gates. Proof payload, parent and historical configuration inputs are immutable under that row identity; they are not compared a second time. Delete/reinsert or disconnect/reaccept must not let an old computation attach to new work. Retain the existing committed admission hold, with no second recovery-state framework. In-flight signed/send history remains recorded; no drain or signing-message change is required.

Reth's consumer-head reconciliation and Era's consumer progress supply the mechanisms. One marker-generic storage module serves both node databases; per-node delete lists remain concrete. One position is simpler than four reader groups while preserving explicit slow-work completion. Upgrade by stopping all shared writers, applying the selective reset/reconciliation contract in [ADR 0020](0020-tag-bitcoin-rows-and-replay-exactly.md), and restarting. Format version alone never enables authority.

A consumer persists its applied position and its oldest-still-needed position separately, in the same transaction as its effects. The latter follows PostgreSQL's replication-slot restart-position contract: advancing applied progress cannot release history still needed after a crash. Both positions identify exact retained header rows and rewind with abandoned history.

The initial replay latch waits for historical configuration decisions, while bridge decisions wait for local proof verdicts. Requiring that latch for local verification would make the dependency cycle permanent. Local verification and its verdict save may therefore proceed during replay, only when that proof's canonical inputs are complete and mature. Votes, signatures, L2 execution, finalization and withdrawals still require global replay readiness and fresh source authority.
