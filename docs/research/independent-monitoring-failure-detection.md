---
status: pending
---

# Independent detection of monitoring failure

## Question and evidence boundary

Who notices when the observability host can no longer send alerts? An always-firing local alert cannot report its own evaluator's disappearance. A dead-man monitor needs a receiver, clock, storage, and human notification path outside the failure domain that sends the heartbeat.

This note compares the required behavior with actual source.
It does not establish that any outside account, endpoint, route, or monitor is active or absent.
No secret, live endpoint, host, notification, or fault-injection test was accessed.

The Via application baseline is [`8a49f355bfe31720f21b8181db471243709194e5`](https://github.com/vianetwork/via-core/tree/8a49f355bfe31720f21b8181db471243709194e5). Local newer zkSync is [`ff5f519b11cff863edcfa0f75af10fea113806b0`](https://github.com/matter-labs/zksync-era/tree/ff5f519b11cff863edcfa0f75af10fea113806b0), not verified latest upstream.

## Application health is not external failure detection

Via's accepted [ADR 0001](../adr/0001-isolate-btc-inscription-observability.md) requires missing or stale inscription evidence to remain unavailable. That protects the meaning of telemetry while a consumer is running; it does not supply a consumer outside the observability host. The [synthetic bridge check](synthetic-bridge-checks.md) proves a different property: a controlled value transfer completes. Neither control depends on implementing the other.

Local newer zkSync's `AppHealthCheck::check_health` queries registered `CheckHealth` components concurrently and aggregates their `HealthStatus`. Its slow and hard default limits are 500 milliseconds and three seconds. `HealthStatus::is_healthy` accepts both `Ready` and `Affected`. `AppHealthCheck` is a shared node-framework resource named `common/app_health_check`. These are application-health interfaces, not a persisted external expectation of the next heartbeat. A process that cannot run cannot use this API to prove its absence. See [health statuses and time limits](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/health_check/src/lib.rs#L23-L145), [aggregation](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/health_check/src/lib.rs#L198-L325), and [framework registration](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/health_check/src/node.rs).

The separate ZK Stack `SettlementFlow` is also not a dead-man monitor. It measures unsettled-block age and exports its result; it still needs an independent consumer to notice that the process stopped updating. Source: [`era-watchdog` at `025b1ed28f0eca77825488effc802390f11f582e`, `src/settlement.ts`](https://github.com/matter-labs/era-watchdog/blob/025b1ed28f0eca77825488effc802390f11f582e/src/settlement.ts). No equivalent external monitoring-host heartbeat receiver was established in the inspected newer zkSync health and sender owners. This is a bounded source conclusion, not a claim about the project's production infrastructure.

## Two implemented receiver models

### Healthchecks tracks an expected arrival and a grace period

At [`3731fc452b0ddca253e48d8ff7968b12539df125`](https://github.com/healthchecks/healthchecks/tree/3731fc452b0ddca253e48d8ff7968b12539df125), `Check` stores `code`, `timeout`, `grace`, `last_ping`, `last_start`, `alert_after`, and `status`. For a simple check that is up, `get_grace_start` returns `last_ping + timeout`. `get_status` becomes `grace` at that boundary and `down` when the grace period ends. `new`, `paused`, and `down` are explicit states. A start signal is separately limited by `last_start + grace`. See [`hc/api/models.py`](https://github.com/healthchecks/healthchecks/blob/3731fc452b0ddca253e48d8ff7968b12539df125/hc/api/models.py).

`sendalerts.Command.handle_going_down` selects checks whose `alert_after` is in the past, excludes already-down checks, recomputes status, and conditionally updates the previous status before creating a `Flip`. Thus receiving HTTP requests and detecting their absence are separate execution paths. A working ping endpoint without the alert worker is not sufficient. The worker uses bounded concurrency through `BoundedSemaphore` and `ThreadPoolExecutor`. See [`hc/api/management/commands/sendalerts.py`](https://github.com/healthchecks/healthchecks/blob/3731fc452b0ddca253e48d8ff7968b12539df125/hc/api/management/commands/sendalerts.py).

`views.ping` accepts an identified check, records the HTTP method and bounded body, and delegates to `Check.ping`. A check configured with `methods == "POST"` ignores other methods. Optional body filtering can change a request into fail, start, success, or ignored. This makes the receiver compatible in principle with a POST heartbeat, but also means an HTTP 200 alone does not prove that the check's success state advanced. Source: [`hc/api/views.py`](https://github.com/healthchecks/healthchecks/blob/3731fc452b0ddca253e48d8ff7968b12539df125/hc/api/views.py). Compatibility must include actual method, body-filter configuration, and receiver-side last-success evidence.

### Uptime Kuma checks elapsed time since a stored push

At release `1.23.16`, commit [`5bb329fa0e8af9395911f33d5a6d6bb9b128c0f7`](https://github.com/louislam/uptime-kuma/tree/5bb329fa0e8af9395911f33d5a6d6bb9b128c0f7), the route `GET /api/push/:pushToken` looks up an active monitor by `push_token`, stores a `heartbeat` with `monitor_id`, `time`, `status`, and `duration`, and applies maintenance state. Notifications use `Monitor.isImportantForNotification` and `Monitor.sendNotification`. See [`server/routers/api-router.js`](https://github.com/louislam/uptime-kuma/blob/5bb329fa0e8af9395911f33d5a6d6bb9b128c0f7/server/routers/api-router.js).

The push branch in `Monitor.start` compares elapsed time to `beatInterval * 1000 + bufferTime`, where the buffer is one second. An absent heartbeat or exceeded window raises `No heartbeat in the time window`. A still-valid push schedules the next check without inserting a fabricated successful heartbeat. This directly avoids renewing freshness merely because the receiver loop is alive. See [`server/model/monitor.js`](https://github.com/louislam/uptime-kuma/blob/5bb329fa0e8af9395911f33d5a6d6bb9b128c0f7/server/model/monitor.js#L650-L681).

This inspected release's push route is GET, not a drop-in receiver for an Alertmanager webhook POST. An approved compatibility adapter or a different receiver is needed; an adapter adds another component whose failures and retries must be understood. Self-hosting this receiver on the observability host would also defeat the purpose. The source establishes the mechanism, not suitability of a particular hosting arrangement.

## Judgment and failure domains

Reuse an existing receiver mechanism rather than writing another timer in `via-core`. The application should not own outside-account provisioning, routing credentials, paging policy, or the monitor's persistence. A health service such as Healthchecks is a closer fit for POST heartbeat receipt; Uptime Kuma illustrates an alternative with a concrete method mismatch that must not be ignored. No vendor is selected by this research.

### Existing Hetzner and NixOS configuration

Hetzner with NixOS is a possible direction, not an accepted or completed migration.
The local infrastructure checkout, named `via-infra-hetzner`, already contains a dead-man integration
candidate. If that direction is selected, inspect its current desired-state owner, or its successor,
before choosing receiver and routing changes. Reconcile the existing work rather than create a
second heartbeat implementation.

Exact infrastructure inspection details remain in the private handoff until repository visibility
and disclosure permission are established. Source inspection does not prove active configuration,
accepted heartbeats, or human notification delivery.

### Portable mechanism, not a Kubernetes recommendation

A heartbeat from the real rule-evaluation and notification path covers more than a host cron job. The desired chain is rule evaluator, local alert delivery, outbound network, outside receiver, outside expiry evaluator, and human notification. A direct cron ping would continue reporting health when alert evaluation or alert delivery has failed. Conversely, a heartbeat sent through a special receiver does not prove that an ordinary chat or paging receiver works. Delivery canaries are a separate control.

The kube-prometheus v0.13.0 [`Watchdog` rule][watchdog-rule] uses `vector(1)` to stay firing.
Its description expects an external integration to notice when notifications stop arriving.
The packaged [Alertmanager configuration][watchdog-route] routes it to a receiver named `Watchdog`
with no notifier configured. That configuration sets a twelve-hour repeat interval.
An always-firing rule and a matching route therefore do not establish external delivery.
This is comparative evidence, not a proposal to install kube-prometheus, adopt Kubernetes, or use
its twelve-hour interval for Via. The same rule-evaluator-to-Alertmanager-to-external-receiver
mechanism can apply to NixOS without adopting the kube-prometheus package.

Alertmanager v0.27.0's [webhook notifier][alert-webhook] sends JSON by HTTP POST and treats 2xx
responses as transport success. That still does not prove a heartbeat receiver advanced its success
state, as the Healthchecks example above shows. An operator must reconcile the effective repeat
interval, grouping delays, retry behavior, and external expiry window. These source examples select
neither a Via receiver nor its timing.

Independence is not just another process or region. Shared cloud account suspension, DNS, network tunnel, identity provider, secret store, deployment automation, or the same human-notification service can remove both sides. The operator must state which failures the chosen arrangement survives and which remain shared. An outside outage can look like a source failure; the alert should report missing evidence rather than asserting the host has died.

The arrival clock should be the external receiver's clock. A sender timestamp is diagnostic data, not authority to extend a deadline arbitrarily. A leaked heartbeat URL or token can manufacture liveness, so the endpoint is a credential. Duplicate retries may extend the apparent last-arrival time. Long retry queues or replayed old messages can therefore delay detection even though they cannot prove new source work. Bounded sender request timeouts, bounded retry age, and no backlog replay are useful requirements.

The detection bound includes the receiver's expiry rule, its evaluation delay, and notification delivery.
For example, a five-minute interval with ten minutes of grace expires about fifteen minutes after the last accepted success.
Scheduler and delivery delays add to that interval.
Maintenance, rate limits, clock jumps, receiver restart, and source restart need explicit states.
Repeated successes should not page.
Missing and recovered transitions need deduplication without suppressing reminders for prolonged failures.

## Ownership, re-fork, and remaining proof

The platform owner owns the heartbeat producer configuration and its release lifecycle. A named operator owns the outside account, receiver, deadline, notification route, escalation policy, and recovery. Re-forking the node must not move the outside expiry evaluator into the node or its observability host. The stable interface is heartbeat identity and accepted receipt semantics, not a VM version, DAL schema, or L2 contract address. Newer zkSync health APIs may enrich diagnosis, but they cannot replace this external contract.

Before commissioning an implementation, the operator must determine whether existing configuration already satisfies this contract.
The remaining choices are the outside service and failure domain, POST compatibility, expected interval, grace period, and human notification route.
Activation requires separate approval.

Completion needs distinct evidence for configuration, activation, and missed-heartbeat detection. A source snapshot identifies the intended sender and receiver contract. An observed first accepted heartbeat binds that configuration to the outside check. A separately authorized interruption, observed by the external expiry evaluator and delivered to a human within the agreed bound, establishes missing-heartbeat detection. Recovery and maintenance behavior then need evidence too. A successful notification button, a dashboard displaying an always-firing alert, or a ping HTTP 200 proves only part of this chain. None of these operational proofs was attempted here.

## Receipt, expiry, and notification are separate contracts

This proposal covers M1 through M5. Prefer the existing desired-state heartbeat route and an
outside managed receiver over another heartbeat daemon. A separately operated self-hosted receiver
is viable if its database, expiry worker, notification credentials, and recovery owner remain
available when the source deployment fails. Hosting location alone does not establish that.

The pinned Healthchecks [`Check.ping` implementation][healthchecks-models] locks the check with
`select_for_update` inside `transaction.atomic`. It records the receiver's `now()` and a `Ping`
in that transaction. Success and failure both update `last_ping`; a start does not. Therefore
`last_ping` alone is not a last-success assertion. An acceptance record must include check identity,
the effective action, status, and receipt time. The `rid` field correlates a start with a later
completion; it is not an anti-replay sequence for successive heartbeat successes.

Maintenance also needs deliberate semantics. With `manual_resume`, a paused Healthchecks check
records arriving requests as ignored rather than silently resuming. Otherwise a success can change
the state to up. `Flip.select_channels` suppresses new-to-up and paused-to-up notifications, so
the first ping after maintenance does not by itself prove recovery notification delivery.
Prefer an approved maintenance deadline and explicit verification that the receiver is armed again.
A pause without an expiry or accountable resume owner can conceal a permanent outage.

The pinned [`sendalerts` worker][healthchecks-sendalerts] conditionally marks a `Flip` processed
before submitting notification work. Its normal idle loop sleeps for two seconds, but occupied
workers, a stopped process, and notification-provider delay can extend detection. This code is not
an exactly-once human-delivery guarantee. Record the outside notification result and human receipt,
not only the database transition or a test-button result.

Alertmanager's pinned [`RetryStage::exec`][alert-retry] uses exponential backoff with
`MaxElapsedTime = 0` and stops when its context ends. A ten-second webhook request timeout is
therefore not a ten-second total retry lifetime. Effective grouping and notification contexts must
be included in the timing argument. An old request accepted after an outage renews Healthchecks
from receipt time. A static Watchdog event does not provide a per-heartbeat monotonic sequence.

For a receiver configured with expected interval `T` and grace `G`, expiry occurs at the last
accepted success plus `T + G`. A human-detection bound also needs a bound on late accepted retries,
the expiry worker's delay, and notification delivery. If those delays are not bounded, describe
`T + G` as the configured expiry window rather than a guaranteed outage-to-human deadline.
Choose the allowed delay with the operator. A stricter freshness protocol may justify an adapter
that checks a fresh authenticated sequence, but that adds persistence and another failure mode.
Do not add it merely to claim a precision the notification provider cannot support.

Future activation evidence must bind the effective rule and route to the intended outside check.
Then a separately approved interruption must produce outside expiry, human receipt, and recovery
within the agreed bounds. Maintenance, token rotation, receiver restart, and an ignored request
that still returns HTTP 200 need explicit evidence. No operational action was performed here.

[healthchecks-models]: https://github.com/healthchecks/healthchecks/blob/3731fc452b0ddca253e48d8ff7968b12539df125/hc/api/models.py
[healthchecks-sendalerts]: https://github.com/healthchecks/healthchecks/blob/3731fc452b0ddca253e48d8ff7968b12539df125/hc/api/management/commands/sendalerts.py
[alert-retry]: https://github.com/prometheus/alertmanager/blob/0aa3c2aad14cff039931923ab16b26b7481783b5/notify/notify.go

[watchdog-rule]: https://github.com/prometheus-operator/kube-prometheus/blob/2648d6fc4e5fb1f98c2914aa2be902476e68cc7a/manifests/kubePrometheus-prometheusRule.yaml#L25-L37
[watchdog-route]: https://github.com/prometheus-operator/kube-prometheus/blob/2648d6fc4e5fb1f98c2914aa2be902476e68cc7a/manifests/alertmanager-secret.yaml#L37-L52
[alert-webhook]: https://github.com/prometheus/alertmanager/blob/0aa3c2aad14cff039931923ab16b26b7481783b5/notify/webhook/webhook.go
