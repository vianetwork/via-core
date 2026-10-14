---
status: accepted
date: 2026-10-14
---

# Serialize verifier scans and cap recovery at completed progress

The verifier's header detector and inscription consumer advance independently.
Recovery at Bitcoin height 156456 cannot establish that a consumer at 156449
processed through 156455. Assigning that height skips proofs in between.
Checking the old height after processing also fails when recovery leaves the
height unchanged or later returns to it. The [checkpoint and continuity
research](../research/verifier-checkpoint-and-proof-continuity.md) records the
evidence and alternatives.

## Decision

Complete inscription scans and reorg recovery share a database-backed mutation
owner. Recovery sets the checkpoint to the minimum of the consumer's actual
completed progress and the height before divergence, inside its transaction.
Zero or absent metadata remains uninitialized. It cannot become evidence that
historical blocks were scanned. The actual divergence boundary independently
governs canonical deletion and withdrawal invalidation.

Scan effects and checkpoint completion commit in one outer transaction. Existing
processor transactions compose as savepoints. Header detection and checkpoint
initialization participate in the same ownership rule so their fetched state and
progress writes cannot race recovery. Every participating writer uses one lock
order, including the existing withdrawal invalidation gate and wallet locks.

For the schema-compatible implementation, ownership spans fetching as well as
application. The scan owner precedes the exclusive withdrawal invalidation gate,
which precedes wallet locks. Acquiring the exclusive gate before processor work
prevents a valid-fork path from upgrading a shared gate after taking a wallet
lock. This blocks other withdrawal mutations during scans and detector fetches.
Fetching outside ownership would require checked recovery generation and chain
identity before effects commit.

A nonsequential accepted proof whose retained parent cannot be found raises a
typed continuity error. The error preserves expected and observed batch numbers,
canonical and observed parent hashes, proof transaction, and scanned Bitcoin
height and hash. A missing initial canonical prefix also fails visibly. The scan
rolls back and the watcher latches a pause with an alertable metric. It does not
repeatedly replay earlier upserts. Operator reconciliation and process
reconstruction are required before another attempt. Existing invalid-publication
skips and valid-fork parent validation retain their separate meanings.

## Alternatives and consequences

A capped assignment alone leaves two unsynchronized writers. Height-only
conditional completion misses unchanged-height recovery and rewind-and-return. A
stored hash alone does not prove that its block was processed. A persisted
generation can permit fetching outside ownership but adds a migration and still
requires atomic effects. Replacing the indexer would broaden the repair without
supplying Via's SQL and message contracts.

The chosen owner works across processes using the existing schema. Holding it
through network fetches can delay recovery and withdrawal mutations. It does not
guarantee a bounded RPC duration, prompt cancellation, or canonical Bitcoin
state at signing. Bitcoin can change while a scan runs. This decision orders
local database mutations rather than eliminating chain changes.

The in-memory gap pause survives polling but resets when the process is
reconstructed. If history is unchanged, the same gap pauses again without
committing partial effects. A restart alone does not repair missing history, and
an alert metric requires a separately configured effective monitoring route. No
live alert configuration is established here.

## Boundaries

This decision accepts source implementation and disposable verification only. It
authorizes no live recovery, deployment, database migration, vote publication,
or withdrawal signing. Previously skipped history needs a separate semantic
rehearsal and approved recovery procedure. Automatic re-derivation,
detector-horizon changes, signing-response repairs, RPC execution isolation,
archived-client replacement, and fresh UTXO validation remain separate work.

The release backport preserves its existing schema and withdrawal model. Main's
later withdrawal lifecycle and source provenance storage must not be imported
merely to repair the checkpoint. Ephemeral scanned-block identity can support
the gap error without changing persisted data or inscription bytes.
