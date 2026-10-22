---
status: accepted
date: 2026-10-22
---

# Recovery repairs only reversible, provable steps, and inspection is read-only

## In short

Recovery repairs a reorg automatically only when every step is reversible and
provable: the fork point is proved, the needed history is retained, every
affected processor has a complete undo, and the stores agree. Each recovery
step belongs to one of four classes. Reversible state and revocable authority
are repaired automatically. External effects, meaning anything signed,
broadcast or paid, and executed L2 state are never undone automatically. When
a step needs one of those, recovery stops loudly, keeps the recovery barrier
and reports the failed condition and the last proved identities. Revoking
stale authority and adding holds are always allowed. A separate read-only
inspection shows what recovery would do, without writing anything. Recovery
applies a plan only if the identities it was planned against still hold. A run
that really writes is a mutation rehearsal, and it runs only on a disposable
copy of the database.

## Background for a new reader

[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md) lets the
Bitcoin reader prove a fork point by walking parent hashes back at most 100
blocks, and stop loudly beyond that.
[ADR 0010](0010-commit-each-scan-group-with-its-position.md) keeps
`via_l1_reorg` as the committed recovery barrier.
[ADR 0013](0013-keep-evidence-membership-and-derived-state-apart.md) says how
each processor withdraws membership and rebuilds derived state. This decision
says which of those steps recovery may take on its own, and how an operator
looks before it acts.

The verifier reverter has no floor and may revert finalized batches, as
`docs/via_guides/reorg.md` describes. Via's copy of ZKsync Era's block
reverter dropped Era's read-only `print-suggested-values` command. The
verifier server has no component selection. The main `via_server` has the
upstream `--components` flag.

## What went wrong

- Automatic repair is safe only when the reader can prove where the fork
  starts, still has the data it needs, and knows that every processor's undo
  and replay is correct. Without those, it can delete valid state or replay
  effects that were already sent to Bitcoin.
- A numeric depth does not prove any of that. A database `executed` flag does
  not show what other parties already consumed.
- A "dry run" that writes and then rolls back is not read-only in PostgreSQL.
  Sequence values stay consumed, and Via uses `BIGSERIAL` keys. The run takes
  production locks. It cannot undo an external action that another task has
  already started.
- The main-node reverter skips its executed-batch guard whenever a reorg row
  exists
  ([`core/node/via_block_reverter/src/lib.rs`, lines 106 to 149](https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/node/via_block_reverter/src/lib.rs#L106-L149)).
- The main-node reverter deletes snapshot files after its SQL commit. A crash
  between the two leaves files that no later run deletes
  ([lines 152 to 175](https://github.com/vianetwork/via-core/blob/6bcdc07a6ad2605c5fc7124836a923a14a75954c/core/node/via_block_reverter/src/lib.rs#L152-L175)).

## Decision

1. **Every recovery step has a class.** The four classes are:
   - **Reversible**: derived state that recovery can rebuild exactly, such as
     canonical membership, wallet selection or the queue of unexecuted
     deposits.
   - **Revocable authority**: permission that a replaced block granted and
     that recovery can withdraw, such as finalization or withdrawal
     eligibility.
   - **External effect**: anything signed, broadcast or paid.
   - **Executed L2 state**: an effect that L2 execution has already applied,
     such as an executed deposit.
2. **Recovery is automatic only for reversible and revocable steps that it can
   prove.** All four conditions must hold: a proved fork point, retained
   history, a complete undo for every affected processor, and agreement
   between the stores.
3. **External effects and executed L2 state are never undone
   automatically.**
4. **Recovery stops loudly when a condition fails.** It stops when the fork
   point is not proved, history is missing, a processor has no complete undo,
   the stores disagree, replay conflicts with evidence, or a step is an
   external effect or executed L2 state. It keeps the barrier and reports the
   failed condition and the last proved identities.
5. **Revoking stale authority and adding holds are always allowed.** They do
   not wait for a stop to be resolved.
6. **Inspection is a separate read-only path.** It runs in a PostgreSQL
   `READ ONLY` transaction and wires no sender, coordinator or signer. It
   reports planned changes, missing evidence and stops.
7. **Recovery applies a plan only if the planned identities still hold.** The
   apply step compares the identities it planned against, such as chain
   positions and scan positions, inside its own transaction. If one has
   changed, it refuses and plans again.
8. **A mutation rehearsal runs only on a disposable copy.** It is never called
   read-only.
9. **No general effect digest yet.** Replay uses explicit identities and the
   field comparison from ADR 0013.

10. **Snapshot cleanup finishes before the barrier clears.** The main-node
    reverter records the snapshot files to delete in the same transaction as
    the revert, then deletes them. A crash resumes from that record. The
    barrier clears only when every recorded deletion is done, so recovery
    waits while object storage is down.
11. **The manual main-node reverter stays as the only operator route for hard
    repairs.** It uses the same inspection and identity checks as the reader.
    Its executed-batch guard no longer has a reorg-row bypass. An override for
    executed state is explicit, separately authorized and never part of
    automatic recovery.

These two decisions close the current main-node reverter's gaps: it skips its
executed-batch guard when a reorg row exists, and its snapshot cleanup after
the SQL commit does not resume after a crash.

## Considered options

**Automatic replay within a numeric depth.** Rejected. Depth does not prove
retained history or the absence of external effects.

**Stop at any finalized batch.** Rejected. It contradicts the current verifier
guide, freezes reversible finality repair, and still misses earlier signing
and payment risk.

**Run the full node in a rolled-back transaction as a dry run.** Rejected. It
is not read-only and cannot contain external effects.

**A runtime dry-run flag threaded through the normal writers.** Rejected.
Every writer must remember to check the flag. Kubernetes' in-process hooks and
`pg_rewind`'s file helpers both depend on that convention.

**A stored effect digest now.** Deferred. A digest is useful only after its
field set, history baseline and encoding are defined.

**Clear the barrier and clean up snapshots later.** Not chosen. It needs a
proof that a stale snapshot can never be served. Main-node hard reorgs are
rare, so waiting for cleanup costs little.

**Remove the manual reverter and let the reader do everything.** Rejected.
Automatic recovery must not rewrite executed L2 state (step 3).

## Why this design

Each part follows an established project that was read at a pinned commit:

- **Refuse rather than guess.** PostgreSQL's `pg_rewind` compares timeline
  identity and start position to find the common ancestor and fails when there
  is none
  ([`pg_rewind.c`](https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/bin/pg_rewind/pg_rewind.c#L1052-L1085),
  C, PostgreSQL licence). Debezium fails with an operator message when its
  stored position is no longer available
  ([`BaseSourceTask.java`](https://github.com/debezium/debezium/blob/215ffa1ad3f13a484b11c8deb9600a05d9ef08cf/debezium-connector-common/src/main/java/io/debezium/connector/common/BaseSourceTask.java#L146-L167),
  Java, Apache-2.0). Kafka followers find the divergence by leader epoch
  ([`AbstractFetcherThread.scala`](https://github.com/apache/kafka/blob/65566690ca1ddb584388c1026e2ff6673e9c6f19/core/src/main/scala/kafka/server/AbstractFetcherThread.scala#L489-L541),
  Scala, Apache-2.0). Via rejects Kafka's fallback to the high watermark when
  no epoch is known. All adapted.
- **Never undo below the committed boundary.** etcd Raft panics when a log
  conflict reaches a committed entry
  ([`log.go`](https://github.com/etcd-io/raft/blob/1931ff5beacc1e2539003c0c70f096285f04c425/log.go#L107-L167),
  Go, Apache-2.0). Via adapts the guard as a stop before external effects and
  executed L2 state.
- **Compensate, do not erase.** Apache Fineract reverses with a linked
  opposite entry
  ([`JournalEntryWritePlatformServiceImpl.java`](https://github.com/apache/fineract/blob/71c39730e9178301b7ab949ba711238ce81fc66d/fineract-provider/src/main/java/org/apache/fineract/accounting/journalentry/service/JournalEntryWritePlatformServiceImpl.java#L318-L408),
  Java, Apache-2.0). TigerBeetle corrects a posted transfer with a new
  transfer and never edits the original
  ([recipe](https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/docs/coding/recipes/correcting-transfers.md#L1-L34),
  Apache-2.0). Adapted.
- **Inspect without writes.** Kubernetes server-side dry run reads, checks and
  computes the result without calling storage update
  ([`dryrun.go`](https://github.com/kubernetes/apiserver/blob/9c4ec15b8038e9f47e8caf219903649dc67c992a/pkg/registry/generic/registry/dryrun.go#L30-L107),
  [tests](https://github.com/kubernetes/apiserver/blob/9c4ec15b8038e9f47e8caf219903649dc67c992a/pkg/registry/generic/registry/dryrun_test.go#L131-L196),
  Go, Apache-2.0). Adapted as a separate path, not as a flag. In a PostgreSQL
  `READ ONLY` transaction, `nextval` on a permanent sequence fails
  ([`sequence.c`](https://github.com/postgres/postgres/blob/a39e53a700c5b728f2f7dab964e33b157979fb3a/src/backend/commands/sequence.c#L649-L660)),
  so an inspection that tries to allocate a `BIGSERIAL` key fails loudly.
- **Apply only if unchanged.** etcd transactions compare revisions before they
  write
  ([`txn.go`](https://github.com/etcd-io/etcd/blob/bf9cadf044f0531036a61c9b6dbeba8aef2a3b30/server/etcdserver/txn/txn.go#L32-L101),
  Go, Apache-2.0). Kubernetes rejects an update whose resource version changed
  ([`store.go`](https://github.com/kubernetes/apiserver/blob/9c4ec15b8038e9f47e8caf219903649dc67c992a/pkg/registry/generic/registry/store.go#L873-L896),
  Go, Apache-2.0). Adapted as guarded SQL writes that refuse instead of
  retrying. Terraform rejects a saved plan whose state changed
  ([`backend_local.go`](https://github.com/hashicorp/terraform/blob/7a5717566e57a9d8705bed20fe199a0785ba0416/internal/backend/local/backend_local.go#L282-L345)),
  but its code is BSL 1.1, so it is an idea only.
- **A typed stop.** ord returns
  [`Unrecoverable`](https://github.com/ordinals/ord/blob/df660674bf78d95db866376aec377def38df3a88/src/index/reorg.rs#L25-L48)
  (CC0-1.0). ZKsync Era refuses to revert executed batches by default
  ([`block_reverter`](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/block_reverter/src/lib.rs#L119-L180),
  MIT or Apache-2.0).
- **Cleanup that survives a crash.** Kubernetes keeps an object until every
  finalizer recorded on it is removed
  ([`store.go`](https://github.com/kubernetes/apiserver/blob/9c4ec15b8038e9f47e8caf219903649dc67c992a/pkg/registry/generic/registry/store.go#L691-L708),
  Go, Apache-2.0), and Flink records completion durably before it reports it
  ([`CheckpointCoordinator.java`](https://github.com/apache/flink/blob/14fa27183139cb9ec0a5f2b34f5d72f7477115c1/flink-runtime/src/main/java/org/apache/flink/runtime/checkpoint/CheckpointCoordinator.java#L1363-L1441),
  Java, Apache-2.0). Adapted as recorded deletions in the main-node reverter.

No studied project runs real recovery writes on a disposable copy as a
feature. That part is operations practice, not a donor.

Compared with what Via had, the boundary is a list of effect classes, not a
depth or a database flag. Stale authority is withdrawn at once, while signed
and paid history stays. An unprovable repair produces a named stop that an
alert can report. An operator can see the planned change before anything
writes, and a plan that went stale is refused instead of applied to a
different state. The
[research note](../research/bitcoin-reorg-handling.md#decomposed-prior-art-per-sub-problem)
lists every donor with its licence and verdict.

## Consequences

- Every processor's undo must name the class of each step it takes.
- The verifier gets an inspection entry point that cannot construct the
  sender, the coordinator or the signer.
- Recovery that reaches an external effect or executed L2 state waits for an
  operator. On testnet4, where recovery runs often, such stops become visible
  instead of silent.
- The main-node reverter changes in two places: its guard and its snapshot
  cleanup. Cleanup gets a small record of pending deletions.

## Not decided here

- How an operator resolves a stop that involves an external effect or
  executed L2 state. That needs its own procedure.
- A stored effect digest, if replay must ever prove equal effects beyond
  identities and undo.

This decision builds on
[ADR 0008](0008-one-bitcoin-reader-per-node-owns-reorg-recovery.md),
[ADR 0010](0010-commit-each-scan-group-with-its-position.md) and
[ADR 0013](0013-keep-evidence-membership-and-derived-state-apart.md). It
authorizes no live deployment, database change or live recovery. Decision
ticket: [#436](https://github.com/vianetwork/via-core/issues/436). Evidence:
[#459](https://github.com/vianetwork/via-core/issues/459).
