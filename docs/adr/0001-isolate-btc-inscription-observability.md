---
status: accepted
---

# Isolate BTC inscription observability from sender execution

Bitcoin RPC and database queries used to observe inscriptions can fail, run slowly, or block without yielding. Running
them inline with transaction processing makes telemetry availability a prerequisite for submission, confirmation,
voting, and shutdown. A timeout alone does not interrupt a synchronous call or a future that never yields.

## Decision

The main-node and verifier sender managers own their processing cadence, reorg-pause handling, and shutdown. Observation
must not delay submission, confirmation, voting, resend progression, the next sender iteration, or caller-runtime drop.

Run observation in a separately owned, bounded execution context with its own RPC transport and database connection
capacity. A blocked observation must not start replacement work or accumulate retries. Telemetry reports observations;
it does not own resend policy.

Publish only observations that belong to the current sender generation and complete within their freshness budget.
Scrapes perform no I/O and do not advance producer timestamps. Startup, expiry, invalidation, reorg pause, shutdown,
RPC/SQL failure, and late completion must not appear as a fresh healthy zero. A successful empty observation publishes
explicit zero counts.

## Alternatives and consequences

Inline observation is simpler and uses fewer resources, but allows monitoring failures to stop transaction processing.
Moving work to a task and wrapping it in a timeout is insufficient when blocking work can still occupy the sender's
runtime or delay its shutdown. Strict isolation is chosen over those lower-cost arrangements.

Isolation requires a bounded worker and separate client resources. A permanently blocked call may retain those resources
until it returns; prompt sender shutdown does not imply that the call was cancelled. Separate connections also do not
isolate database-server CPU or I/O.

Alerts and dashboards must distinguish unavailable evidence from healthy zero counts and coordinate changes with the
producer's metric contract. This accepted decision defines the required behavior; it is not evidence of implementation
or deployment status.
