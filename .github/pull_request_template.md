## Why

<!--
Explain the runtime, protocol, developer-experience, or documentation problem in
human terms.

For protocol/runtime/safety changes, write a short narrative rather than a terse
summary. The reader may be a future reviewer or operator trying to understand why
this behavior exists during an incident, release, or migration.

Cover the relevant parts of the story:
- What was observed in source behavior, tests, logs, public APIs, live systems,
  upstream code, or deployment desired state?
- Why was the existing behavior unsafe, misleading, incomplete, hard to review,
  or insufficient for operations?
- What future maintainer, release, migration, external-node bootstrap, proof
  flow, verifier path, BTC sender path, or incident-response path could be harmed
  by leaving it as-is?
- Which protocol/runtime invariant, config contract, ordering guarantee, data
  shape, or deployment boundary should this repo enforce now?

Distinguish source changes from live proof. Code, docs, tests, configs, and
Docker files in this repo describe behavior or artifacts; they do not by
themselves prove what is currently deployed.

Avoid leading with agent/tool process unless it materially affects review,
safety, or reproducibility.

Use impersonal, operator-facing wording. Avoid first-person singular or plural
phrasing. Name the component, crate, module, config key, runtime invariant,
deployment boundary, or operator action instead.
-->

## What changed

<!--
Group concrete behavior changes by purpose. Prefer behavior-level bullets over a
file-by-file changelog.

Examples:
- The reorg detector now compares sparse L1 block windows by explicit Bitcoin
  height rather than row position.
- The BTC sender now treats byte-reversed txids consistently when reporting
  public explorer evidence.
- The external-node guide now documents the bootstrap dependency that must be
  satisfied before a restart durability check.
-->

## Boundaries and non-goals

<!--
State the source, review, and rollout boundary for this PR.

Use this section to prevent unsafe assumptions, not to repeat the diff. Call out
only non-goals that matter for protocol/runtime safety, release handling, or live
operations.

Good examples:
- This PR updates source code, tests, or docs only; no live deployment was run
  from this branch.
- The runtime behavior changes, but no database rows, wallet state, Kubernetes
  resources, Helm releases, DNS records, cloud resources, or host services were
  changed.
- Secret names or config keys changed, but no secret values were added, moved,
  renamed, printed, or required by this PR.
- The source change can affect future images built from `main`, but rollout to
  `via-main-testnet`, Hetzner external nodes, verifier nodes, or other live
  environments remains a separate operator step after review and approval.
- Deployment desired state in `kube-state`, chart defaults in `helm-charts`, and
  Hetzner/GCP infrastructure repos are unchanged unless explicitly listed.
- The change is limited to a specific component or crate, such as BTC sender,
  reorg detector, DA client, external-node bootstrap, verifier, prover, or docs;
  sibling implementations are intentionally unchanged or separately called out.

Prefer explaining the boundary that could be misunderstood over listing every
possible subsystem that was not touched.
-->

## How to review

<!--
Tell reviewers where to focus and what could be risky.

For runtime changes, mention the relevant crates/modules, invariants, and sibling
paths that should be compared. Examples:
- Bitcoin integration: `core/lib/via_btc_client/`, `core/node/via_btc_watch/`,
  `core/node/via_btc_sender/`, and corresponding verifier-side BTC paths.
- Reorg detection: `core/node/via_main_node_reorg_detector/` and
  `via_verifier/node/via_reorg_detector/`.
- DA/Celestia paths: `core/lib/via_da_clients/` and verifier DA client paths.
- External-node bootstrap/restart surfaces: `docker/via-external-node/`,
  external-node docs/examples, and config keys consumed by deployment repos.
- Prover/verifier/coordinator paths under `prover/` and `via_verifier/`.
- `zkstack_cli/` changes that require rebuilding the installed CLI with
  `zkstackup --local` before validation reflects source changes.
- `zkstack_cli` Forge-script parameter and output-path changes where missing
  deployments, bad paths, or wrong command ordering should fail clearly rather
  than being hidden by broad fallbacks.
- ordering assumptions in async fetch/compare code.
- DB read/write semantics and migration implications.
- config key defaults and deployment follow-up in `kube-state` / `helm-charts`.

Call out whether reviewers should compare against tests, upstream ZKsync code,
public explorer APIs, kube-state desired state, live logs/metrics, or database
state. If upstream ZKsync is used as a reference, identify whether the referenced
files, commands, and behavior exist in this repo's current fork lineage. Do not
imply that source review is a live rollout or live verification.
-->

## Checks run

<!--
Replace or edit this block with commands actually run. Keep exact commands
copy/paste runnable where possible. Remove commands that were not run.
-->

```bash
git diff --check
zkstack dev fmt
zkstack dev lint
cargo test -p <crate-or-package>
gitleaks protect --staged --redact --no-banner
gitleaks git --log-opts="main..HEAD" --redact --no-banner
```

## Author checklist

- [ ] PR title follows Conventional Commits.
- [ ] Tests, docs, formatting, and linting were updated or marked not applicable.

## Live infrastructure and deployment impact

<!--
State both:
1. whether this PR already included live-system interaction, and
2. what deployment impact is expected after merge.

Use one of these patterns for pre-merge actions:

- No live infrastructure actions were run.

or:

- Read-only live checks were run:
  - `<exact command, public API, or read-only query>`
  - `<exact command, public API, or read-only query>`

or:

- Live changes were run:
  - Environment/context:
  - Workload/service/database/resource:
  - Exact command or operation:
  - Reason:
  - Verification:
  - Rollback/follow-up:

Be explicit about mutations: deployments, restarts, migrations, DB writes,
secret reads/writes, wallet/cookie access, temporary pods, DNS/Cloudflare,
ingress/public exposure, cloud resources, and host-level service changes.

Then state expected post-merge behavior, for example:

- Merging this PR changes source/docs/tests only and is not expected to deploy
  live services by itself.

or:

- Merging this PR can affect future images built from `main`; rollout to a live
  environment remains a separate deployment approval and verification step.
-->
