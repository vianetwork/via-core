---
status: pending
---

# Inscription monitoring without sender dependencies

## Question and evidence boundary

How can Via report current overdue inscriptions without making sender progress depend on telemetry? [ADR 0001](../adr/0001-isolate-btc-inscription-observability.md) already answers the isolation question. The remaining choices concern the producer candidate, its public metric contract, consumer ownership, and release order. This research does not reopen the decision or establish activation.

Via source is pinned to [`8a49f355bfe31720f21b8181db471243709194e5`](https://github.com/vianetwork/via-core/tree/8a49f355bfe31720f21b8181db471243709194e5).
Local newer zkSync is pinned to [`ff5f519b11cff863edcfa0f75af10fea113806b0`](https://github.com/matter-labs/zksync-era/tree/ff5f519b11cff863edcfa0f75af10fea113806b0), not verified latest upstream.
A candidate requires an identifiable source revision and evidence for its behavior.
An ADR or a historical test report alone cannot establish that the candidate satisfies the contract.

## What the current producer measures

The main-node `ViaBtcInscriptionManager::loop_iteration` returns during a reorg, then calls `update_inscription_status` before `send_new_inscription_txs`. Its status loop reads inflight requests, selects a request history, checks confirmation, and fetches Bitcoin height for unconfirmed attempts. When `sent_at_block + stuck_inscription_block_number <= current_block`, it queries and writes `report_blocked_l1_batch_inscription`. That gauge contains a batch identifier, not a count. There is no clearing assignment in the successful-empty path. A fresh scrape can therefore expose an old value. See the [manager](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/via_btc_sender/src/btc_inscription_manager.rs#L63-L196) and [metric declaration](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/node/via_btc_sender/src/metrics.rs#L38-L61).

`ViaBlocksDal::get_first_stuck_l1_batch_number_inscription_request` joins `via_btc_inscriptions_request` to `via_btc_inscriptions_request_history`, takes `MIN(l1_batch_number)`, and filters with strict `<`. It does not filter out confirmed requests or restrict the join to the current attempt. Its equality boundary also differs from the manager's inclusive boundary. These are source semantics, not evidence that a particular deployed dashboard is wrong today. See the [query](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/dal/src/via_blocks_dal.rs#L503-L528).

The verifier has its own `ViaBtcInscriptionManager::update_inscription_status_or_resend`, `via_btc_sender_dal`, and `via_verifier_btc_sender` metric prefix. It tracks `Indexed`, `Voted`, `Finalized`, and `Rejected` batch states, which are not the main node's `Latest` and `Finalized` states. Its business operations must remain verifier-owned. See the [verifier manager](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_btc_sender/src/btc_inscription_manager.rs) and [metrics](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_btc_sender/src/metrics.rs). The [sibling map](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/.github/sibling-paths.yml) names both owners.

## Comparable implementations and their limits

Local newer zkSync's `EthSenderMetrics` separates `number_of_inflight_txs`, labelled by `OperatorType`, from `last_known_l1_block`, labelled by `BlockNumberVariant`. `track_block_numbers` reads `L1BlockNumbers.latest`, `.finalized`, and `.fast_finality`; `track_eth_tx_metrics` dispatches between `AggregatedActionType::L2Block` and `::L1Batch`. This is a useful separation of counts, stages, and identities. It is not a Bitcoin inscription observer and does not implement ADR 0001's private execution resources or iteration freshness. Copying the Ethereum metric names would also lose the distinction between Bitcoin height and Via batch number. See [newer metrics](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/eth_sender/src/metrics.rs#L124-L245).

Two chain-specific monitoring implementations provide closer comparisons:

- ZK Stack's `SettlementFlow` reads `getBlock("finalized")`, then the first unsettled L2 block, then L1 `getBlock("latest")`. `watchdog_settlement_age` measures the L1 timestamp minus that unsettled block's timestamp. An observed absence of an unsettled block sets zero; RPC failure instead calls `recordFlowFailure`. This is a useful distinction between an empty result and a failed observation. It is an independently scheduled RPC monitor, not an inscription-attempt classifier. Its chain-time age does not establish local producer freshness, and its two-second step timeout is not proof of cancellation. Source: [`era-watchdog` at `025b1ed28f0eca77825488effc802390f11f582e`, `src/settlement.ts`](https://github.com/matter-labs/era-watchdog/blob/025b1ed28f0eca77825488effc802390f11f582e/src/settlement.ts).
- Optimism's `gameMonitor.monitorGames` fetches a `HeadBlockFetcher` result, passes `headBlock.Hash` and a bounded `gameWindow` timestamp to `Extract`, and retains separate ignored and failed counts. `loop` schedules sequential observations; `StopMonitoring` cancels context and waits with a caller deadline. A block-hash-anchored snapshot and explicit partial failures are useful for Via. Context cancellation still depends on cooperative callees and does not satisfy Via's stronger non-yielding-call isolation requirement. Source: [`optimism` at `33fbe016b879d3c4a5187516cbab7668b4527627`, `op-dispute-mon/mon/monitor.go`](https://github.com/ethereum-optimism/optimism/blob/33fbe016b879d3c4a5187516cbab7668b4527627/op-dispute-mon/mon/monitor.go).

Prometheus Blackbox Exporter's `Handler` creates `probe_success` and `probe_duration_seconds` per request, dispatches through `Probers`, and derives a context deadline from module timeout and `X-Prometheus-Scrape-Timeout-Seconds`. It is useful for an external transport check. Its deliberate scrape-triggered I/O is the wrong producer design for the accepted Via contract. Source: [`blackbox_exporter` at `46ea0224f57b708fdff317f8872765a77ef64034`, `prober/handler.go`](https://github.com/prometheus/blackbox_exporter/blob/46ea0224f57b708fdff317f8872765a77ef64034/prober/handler.go).

## Required observation meaning

The following is a proposed handoff contract, not a claim about recovered candidate code. It preserves ADR 0001:

- A request identity is distinct from its submission attempts. Classification uses the outstanding request and one deterministically selected current attempt. A replaced or confirmed historical attempt cannot remain overdue merely because its history row remains stored.
- Observation includes its sender iteration, acquisition time, expiry, Bitcoin height, and completeness. Height zero is valid. Negative or future attempt heights, missing history, or missing batch association cannot become healthy zero. A batch identifier remains optional metadata, separate from counts.
- The sender invalidates evidence before processing and during reorg pause or shutdown. Only a successful observation for the still-current iteration may publish. A successful empty query publishes zero counts; startup, timeout, expiry, late completion, and RPC or SQL failure publish unavailable evidence.
- One bounded worker owns independent RPC transport and database connection capacity. A stuck call admits no replacement worker and accumulates no retry queue. Scrapes read an in-memory result and perform no I/O. SQL statement limits and transport deadlines reduce resource occupation but do not prove a blocked call has ended.

A pending-request query still needs a data-volume bound or an explicit incomplete result when its limit is reached. One query and one worker bound concurrency, not database work or returned rows. Query-plan evidence must cover a large retained history and a large pending set. Separate connection pools do not isolate database-server CPU or I/O, as ADR 0001 explicitly notes.

Consumers need the same target identity for counts, freshness, and completeness. Aggregating counts from one instance with the timestamp of another can manufacture health. Clock skew, future timestamps, stale series, reorg pauses, slow Bitcoin blocks, and low transaction demand require distinct treatment. Fresh positive overdue evidence remains actionable even when other requests are incomplete. An absent series or failed query must not be replaced with zero.

Counts, acquisition timestamps, and a completeness indicator are one possible metric representation.
Another can omit unavailable measurements and expose a separate status. Neither representation is
selected here. Either needs consumer evidence that startup, expiry, partial results, and failed
observations cannot become healthy zero. Scrape time cannot replace acquisition time.

Prometheus's [instrumentation guidance at `8bdb919e820ad27adc12fc66daf38531c3d9a801`](https://github.com/prometheus/docs/blob/8bdb919e820ad27adc12fc66daf38531c3d9a801/docs/practices/instrumentation.md#L227-L234)
recommends exporting the timestamp of an event rather than continuously updating its age.
For Via, the acquisition time is a metric value, separate from the time Prometheus scrapes that value.
That supports detecting a stopped producer. The same guidance's advice to initialize known metrics
to zero does not mean that an unsuccessful observation measured zero. It does not select a Via
metric implementation.

## Ownership and future re-fork

The current producer owners are the two sender managers and their role-specific DALs.
`via-core` owns the meaning and version of exported observations.
Consumer owners must reconcile alert rules, generated dashboards, embedded dashboard copies, and runbooks together.
Repository assignments and release responsibilities require a separate operational decision.

The Via contract that must survive a re-fork is isolation, bounded admission, current-attempt identity, reorg invalidation, and unavailable-versus-zero semantics. The existing table layouts, `vise` declarations, and framework wiring are implementation choices. Local newer zkSync exposes `AppHealthCheck` as the shared resource `common/app_health_check` and supports `ReactiveHealthCheck` and `CheckHealth`. A cached observer status can inform these interfaces. `AppHealthCheck::check_health` concurrently polls checks with time limits; that does not turn a sender-runtime task into a strictly isolated worker. See [resource registration](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/health_check/src/node.rs) and [health aggregation](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/health_check/src/lib.rs#L198-L325).

Re-fork evidence must exercise both real sender lifecycles with blocked RPC and SQL, failed observation, successful empty recovery, superseded attempt, threshold equality, late result, reorg pause, shutdown, and caller-runtime drop. Consumer fixtures must include two targets with conflicting freshness, future timestamps, missing counts, incomplete positive counts, and metric removal. No tests were run here. A module split is a proposal until both framework adapters support these behaviors.

## Remaining decision

Recover and identify the exact candidate before choosing it. Then name one producer owner and the consumer owners, agree one metric contract without misleading legacy aliases, and choose a coordinated release order. Consumer definitions may be prepared before producer release, but must explicitly represent unavailable evidence until the producer is active. Source review, merged code, configured consumers, and observed activation are separate evidence states. This research provides the first, not the last.

## Concrete handoff proposal

This proposal covers I1 through I4. It does not select an unrecovered candidate or amend ADR 0001.

The two earlier draft revisions expose why candidate identity matters. At
[`c0132e881ed7a5365e670c81b4abd71ebb3c9652`](https://github.com/vianetwork/via-core/blob/c0132e881ed7a5365e670c81b4abd71ebb3c9652/core/lib/via_btc_client/src/metrics.rs),
`InscriptionObserver::refresh` awaits a supplied future under `tokio::time::timeout`, then checks
elapsed time. That final check rejects a late value but does not free a caller blocked inside the
future. At
[`01c31e8c81c2b2197268282900ac1b0b6d7ee8be`](https://github.com/vianetwork/via-core/blob/01c31e8c81c2b2197268282900ac1b0b6d7ee8be/core/lib/via_btc_client/src/metrics.rs),
`InscriptionObserver::observe` invalidates its snapshot, awaits height and requests under a timeout,
and publishes `InscriptionState`. Neither inspected method owns the separate execution resources
required by ADR 0001. These revisions are not interchangeable with a later isolated candidate.

Prefer one shared classifier and bounded observer with two role-owned SQL adapters. A private
worker thread and runtime can preserve sender shutdown while retaining one permanently blocked
call. A subprocess provides a stronger resource-reclamation option but adds process supervision
and message transport. Choose it only if reclaiming blocked observation resources is a requirement.
Neither option permits replacement work while the old observation remains admitted.

The installed RPC stack makes that ownership concrete. Via's
[`BitcoinRpcClient`](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/client/rpc_client.rs)
calls a synchronous Bitcoin client from async methods. Its
[`with_retry`](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/via_btc_client/src/utils.rs)
permits one initial call plus three retries, with three 500 ms sleeps. The installed jsonrpc 0.18
transport uses 15-second socket-operation timeouts; this is not an end-to-end observation deadline.
An async timeout alone cannot interrupt synchronous execution that does not yield.

Tokio 1.42's
[`new_current_thread`](https://docs.rs/tokio/1.42.0/tokio/runtime/struct.Builder.html#method.new_current_thread)
does not create an OS thread. A private current-thread runtime needs an explicit thread owner.
[`shutdown_background`](https://docs.rs/tokio/1.42.0/tokio/runtime/struct.Runtime.html#method.shutdown_background)
does not wait for blocking work; it does not terminate a synchronous call already blocking that
thread. Prompt manager shutdown therefore requires a deliberate bounded detach policy or a
reclaimable process boundary, not merely that method call. The manager must not join a stuck worker
or start replacements, and no worker-owned resource may be needed by a sender or scrape.

For the public contract, prefer one coherent optional snapshot rather than individually updated
gauges. Proposed snapshot fields are `generation`, `acquired_at`, `expires_at`, `bitcoin_height`,
`pending`, `overdue`, `incomplete`, and optional `first_overdue_batch`. These are proposed names,
not recovered implementation fields. Use a monotonic deadline internally. Export acquisition time
as Unix seconds for consumers, and reject future timestamps beyond the agreed clock-skew allowance.
An expired snapshot remains unavailable even if wall-clock correction would make it appear fresh.

Proposed metrics are `via_btc_inscription_pending_requests`,
`via_btc_inscription_overdue_requests`, `via_btc_inscription_incomplete_requests`, and
`via_btc_inscription_observed_at_timestamp_seconds`, with bounded `sender` values `main` and
`verifier`. Keep transaction IDs and generations out of labels. The full scrape-target identity
must join every count to its timestamp. Omit count and acquisition series when unavailable.
Publish explicit zeros only after a successful empty observation. A fresh incomplete snapshot can
prove a positive overdue lower bound, but cannot prove that all requests are healthy. A row-limit
hit must also mark the snapshot incomplete.

The installed vise 0.2.0 constrains how absence is implemented. An ordinary `Gauge` exports its
numeric default or last value; `Family` uses an append-only map and continues exporting created
members. Leaving a gauge untouched does not remove its stale series.
[`Collector<Option<M>>`](https://docs.rs/crate/vise/0.2.0/source/src/collector.rs)
is a supported omission mechanism: `None` visits no metrics. The collector closure runs during
scrape, so it must only load an immutable snapshot, check its monotonic expiry, and encode bounded
metrics. It must not perform RPC, SQL, or acquire a lock held across worker I/O.
An equivalent custom collector is possible; a public observation-status series with retained values
is a different contract requiring every consumer to gate those values.

This is not zero-cost encoding. A scalar snapshot can avoid `Family`'s cloned label-key list and
per-member lookups, but formatting still performs work. Future proof must scrape with absent, fresh,
expired, and partial-positive snapshots while real worker I/O remains blocked. It must show both
sample presence/absence and scrape responsiveness, not only the internal snapshot state.

The producer owner must freeze those names and meanings with the consumer owners before release.
Consumers first need fixtures and definitions that show new metrics as unavailable until present.
Then release the producer and retire the old blocked-batch metric without an alias or zero-fill
fallback. Remove obsolete alert expressions, generated dashboards, embedded copies, and runbook
references in each affected repository. A rule that compares a batch identifier with a count
cannot be migrated by changing its metric name.

Future local proof must hold a real observation RPC and SQL call blocked while each actual sender
continues its business work and stops promptly. It must also show bounded busy admission, result
rejection after a generation change, threshold equality, zero height, partial positive evidence,
empty recovery, reorg invalidation, and scrape-only reads. Those are implementation proofs.
Separate activation evidence must identify the running producer version and the effective consumer
definitions. No tests or activation checks were performed for this research.
