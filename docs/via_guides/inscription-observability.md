# Current inscription observations

Each sender refreshes a database snapshot on its existing polling interval, before transaction processing and the
reorg check. This runs without new L2 transactions or mempool demand. Confirmation and submission retain their existing
ordering. A confirmation appears in the next successful snapshot.

The shared collector exports these gauges with `sender="main"` or `sender="verifier"`. Coordinator processes use the
verifier sender. Retain the scrape labels identifying cluster, service and instance; classify each instance before
aggregating across roles or replicas.

| Metric | Unit and meaning |
| --- | --- |
| `via_btc_sender_inscription_pending` | Unconfirmed requests, including requests without history. |
| `via_btc_sender_inscription_overdue` | Requests whose latest attempt has a known Bitcoin block age at least the configured threshold. |
| `via_btc_sender_inscription_unobserved` | Requests with missing history, invalid sent height, or unavailable batch association. May overlap overdue. |
| `via_btc_sender_inscription_first_overdue_batch` | Smallest known L1 batch number among overdue requests; zero when none is available. This is an identifier, not a count. |
| `via_btc_sender_inscription_observed_at_timestamp_seconds` | UNIX seconds when the successful observation started, including fractional seconds. |

The four status gauges come from one SQL statement and are published with the observation start timestamp as one
snapshot. A successful empty snapshot exports four zeros and a timestamp. Startup, a failed observation, or a stopped
manager exports no samples for that sender.
While a refresh is pending, the previous snapshot retains its original timestamp. A fresh Prometheus scrape does not
advance this timestamp.

## Reading the result

Choose a freshness budget above the configured polling interval and scrape interval, with operational allowance for
normal request latency. For the default five-second polling interval, a 60-second budget is an example, not a producer
guarantee. Reject future timestamps as well as expired ones. Match every metric on the same instance and sender labels.

- Fresh `overdue > 0` proves current overdue requests at the observation time. If `unobserved > 0`, it is a lower bound
  and some evidence remains unknown.
- Fresh `overdue = 0` with `unobserved = 0` proves no overdue requests in that snapshot.
- Missing, failed, expired or incomplete evidence cannot establish a healthy zero. Show UNKNOWN; do not use `or vector(0)`.
- A zero first-batch gauge alone says nothing about health. Batch zero is valid, and an overdue verifier request may
  have lost its batch association.

An aggregate healthy result requires fresh complete observations for every expected sender instance. Detect missing
targets against the expected deployment inventory. Do not gate an overdue alert on transaction demand.

## Request and attempt semantics

Only requests with `confirmed_inscriptions_request_history_id IS NULL` participate. The greatest history ID determines
the latest attempt, matching confirmation processing even when timestamps tie or disagree. Multiple histories count
once. The existing inflight-list timestamp ordering only determines membership, not the processed attempt; those
transaction-processing queries are unchanged.

Age is `current Bitcoin height - sent_at_block`; equality with `stuck_inscription_block_number` is overdue. Height zero
is valid. Writers append history after successful inscription; no history means a request has not recorded a sent
attempt. Missing history and negative or future heights remain unobserved, with no fallback to older attempts.

Main requests carry their batch number. Verifier requests resolve it through the vote-request mapping and then the
votable transaction. Both mapping keys are unique. Removing a votable transaction can remove its mapping while leaving
the request. Such requests still count as pending, and as overdue when their age is known; their batch is unobserved.

## Cost, failures and consumer cutover

Each interval performs one height read and one aggregate SQL statement on a separate, short-lived pool checkout. It
replaces the height reads per unconfirmed attempt and the historical blocked-batch query. The SQL filters pending
requests first and selects latest histories together, avoiding a history scan per request. Existing indexes do not
cover history by request, so populated snapshots can still scan historical rows. No index or schema migration is added.

Observation errors are logged and remove the snapshot; they do not propagate into transaction processing. The timeout
uses the polling interval. The existing Bitcoin transport performs synchronous calls, so it can overrun an async timeout;
late results are rejected after return. A stalled sender exposes an aging observation timestamp rather than a newly
healthy result. Scraping uses cached data and performs no SQL or RPC calls.

The latched `via_btc_sender_report_blocked_l1_batch_inscription` and
`via_verifier_btc_sender_report_blocked_l1_batch_inscription` metrics are removed. Their old values were batch IDs,
not counts. Replace legacy dashboard panels and the server alert based on `changes(...[15m])` with the current gauges
and freshness checks above. Update verifier/coordinator panels and alerts independently, plus inventory, runbooks and
consumer tests. Legacy inflight gauges remain available but are not part of this coherent snapshot.

This source change does not establish deployment or live recovery. Producer image provenance and consumer rollout
require separate verification. Historical settlement repair and pipeline/finality metrics are outside this contract.
