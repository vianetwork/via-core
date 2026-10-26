# Bitcoin reader and reorg handling

Each main, external and verifier node follows its configured Bitcoin Core source through one reader.
The shared reader stores canonical headers, a confirmed frontier and separate consumer positions.
Node tasks own polling and shutdown; each role's DAL owns its SQL and dependent-row deletion.
See [ADR 0019](../adr/0019-one-reader-position-with-trailing-consumers.md),
[ADR 0020](../adr/0020-tag-bitcoin-rows-and-replay-exactly.md) and
[ADR 0021](../adr/0021-automatic-shallow-recovery-above-confirmed-frontier.md).

## Observation and authority

Headers are observed at the tip. Protocol inputs commit with the protocol consumer's applied position.
The verifier's withdrawal observer has a separate consumer, so missing DA or proof work can trail header ingestion.
Each consumer also stores its oldest still-needed position in the same transaction as its effects.

Irreversible actions require complete initial replay, mature canonical inputs and a fresh source check.
This includes votes, signatures, finalization and withdrawals, including cached or resumed work.
A source outage holds those actions. Deposit maturity and replay readiness are checked at mempool selection;
L2 execution consumes admitted transactions without a Bitcoin RPC or a per-block reader-state gate.
Sender completion uses its configured `block_confirmations`; the guarded DB write does not wait for its own
inscription to reach the reader's content depth. Inclusive content maturity is `tip - source_height + 1 >= N`:

| Network | N |
|---|---:|
| Bitcoin mainnet | 6 |
| Testnet4 | 100 |
| Signet | 100 |
| Isolated regtest | 1 |

Replay readiness waits for historical configuration decisions, which can depend on local proof verdicts.
Requiring readiness for those verdicts would create a cycle. Local verification and verdict save can run
while replay continues, provided that proof's canonical inputs are complete and mature.
This exception grants no publication or signing authority.

## Shallow recovery

Automatic recovery requires fewer than N disconnected old blocks and an ancestor at or above the saved frontier.
The reader checks the old suffix and its parent chain even when Core has no replacement block yet.
Native dependent deletion, canonical-header rewind, consumer rewind and frontier preservation commit together.
The existing owner and source fences exclude competing reader changes during each protected action.

For example, at tip 100 with N=6 the mature boundary is block 95. If block 100 is replaced, the reader
can rewind to 99 and remove an unexecuted deposit from the abandoned block. A replacement inclusion
gets a new header identity. Replay compares the same Bitcoin outpoint and payload before admission;
it preserves any already-executed operation instead of minting another operation for the remine.

## Stops and retained history

A reorg of N or more old blocks, a frontier crossing, missing retained history or a replay conflict stops the reader.
The first reason is durable and readiness is cleared. Restarting does not clear the reason or authorize repair.
Keep the database and matching Bitcoin history for diagnosis. This build supplies no automatic deep-recovery procedure.
The main-node L2 block-reverter command remains a separate operation with its existing cascades.

Reset retains completed rows, signing risk and their required parents until canonical source replay reconciles them.
For rotation evidence only, proofs finalized before the reset cutoff remain exempt: they are pre-#469 history,
and expired testnet DA makes re-verification impossible. Their original rows and FKs remain unproven and unchanged.
Mainnet starts without those rows. Later canonical proofs through the rotation height require local verification.

## Monitoring

The `via_bitcoin_reader` metric family labels the role as `core` or `verifier`.
Inspect `head`, `frontier`, `replay_ready`, `replay_unreconciled`, `stopped` and `consumer` together;
consumer metrics also label the consumer. `replay_unreconciled=1` means retained source rows still need
canonical binding. Each replay attempt warns with its first deposit hash, votable ID or protocol minor;
the gauge returns to zero when those bindings and retained proof bindings are complete.
A healthy HTTP endpoint does not establish reader readiness. The durable reader row carries the diagnostic reason.
Publication metrics include pending and in-flight requests, errors, inscription and confirmation durations,
and account balance with the actual signing address. Dashboard and alert rollout belongs to deployment repositories.
