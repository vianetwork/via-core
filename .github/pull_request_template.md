## Why

<!--
Explain the runtime, protocol, developer-experience, or documentation problem in
human terms.

For protocol/runtime/safety changes, write a real reviewer/operator-facing
narrative rather than a terse summary or bullet-only changelog. The reader may be
a future reviewer or operator trying to understand why this behavior exists
during an incident, release, or migration. Include enough context for that reader
to reconstruct the reasoning without reading the chat, agent logs, or every
linked issue.

Cover the relevant parts of the story:
- What was observed in source behavior, tests, logs, public APIs, live systems,
  upstream code, or deployment desired state?
- Why was the existing behavior unsafe, misleading, incomplete, hard to review,
  or insufficient for operations?
- What future maintainer, release, migration, external-node bootstrap, proof
  flow, verifier path, BTC sender path, reviewer, or incident-response path could
  be harmed by leaving it as-is?
- Which protocol/runtime invariant, config contract, ordering guarantee, data
  shape, deployment boundary, or review/process boundary should this repo enforce
  now?

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

## Reuse & Duplication

<!--
Required for any PR that adds or changes production logic. For docs-only,
comments-only, generated-only, test-only, or process-only changes, write
"Not applicable — <one-line reason>".

Name the specific function, module, or crate. Vague answers such as
"searched the repo" or "no duplication found" are not acceptable.

- Closest existing implementation: Name the function, module, or crate that
  already does something similar. If none was found, state where you searched
  (at minimum the nearest via_* crate and the sibling core/ or via_verifier/ path).

- Reuse decision: State whether you extended, extracted, replaced, or
  deliberately duplicated existing logic — and why. If duplication was
  introduced or left in place, justify it.

- Sibling check: For changes in reorg detection, Bitcoin handling, DA clients,
  watch services, verifier/prover paths, or any logic mirrored across core/
  and via_verifier/, explicitly state whether you reviewed the sibling
  implementation and whether it shares the bug, shares the fix, or is
  intentionally different.

High-risk areas (Bitcoin, DA/Celestia, reorg/reverter, verifier/prover, hot DAL
paths, migrations, serialization, hashes, signatures, inscriptions) require
especially specific answers.

Anti-pattern example:
- New detector logic added in both core/node/.../reorg_detector and via_verifier/node/.../reorg_detector with only minor differences.
- No shared code was extracted.

Required pattern:
- Shared logic extracted to core/lib/ (or appropriate shared crate).
- Only thin node-specific wrappers remain in core/node/ and via_verifier/node/.
-->

**Required for any PR that adds or changes production logic in a `via_*` path.**

Before writing implementation code, you must have completed the checks described in the root `AGENTS.md` → *Reuse and duplication discipline*.

**You must include the following in this section** (answers must be specific — vague or placeholder text will be rejected):

- Closest existing implementation considered: `<function or module name>`
- Sibling paths inspected: `<list of specific paths>`
- Search command run: `<exact rg/grep command>`
- Shared extraction decision: extracted to `<path>` **OR** not extracted because `<exact invariant / ownership / execution-context difference>`

Vague answers such as "searched the repo" or "no duplication found" are not accepted.

> Self-check before submitting: Have you named a specific function? Have you listed sibling paths actually inspected (with backticks)? If any answer is no, revise this section.

## Performance, Complexity, and Resource Impact

<!--
Required for changes touching Via runtime paths (via_* crates, reorg detection,
Bitcoin sender/watch/client, DA/Celestia, verifier, prover, state keeper, hot
DAL queries, async fetch/compare logic, ordering-sensitive code). Encouraged
elsewhere. For docs-only or process-only changes, write
"Not applicable — <one-line reason>".

Focus on the dimensions that matter for this change. A table is usually the
clearest way to show trade-offs — use or adapt the format below. Prose is also
acceptable.

Do not fabricate precision. Do not optimize for fewer lines at the expense of
correctness or error context.
-->

**Recommended table format (adapt rows as needed):**

| Dimension                        | Before                          | After                           | Notes |
|----------------------------------|---------------------------------|---------------------------------|-------|
| Time complexity (main operation) | O(n)                            | O(n)                            | Now compares by explicit height |
| Allocations per operation        | 1 Vec + N clones                | 1 HashMap build + sort          | After buffer_unordered |
| Happy-path work per call         | Positional zip                  | Height-keyed HashMap lookup     | Correctly handles sparse windows |
| Memory pressure                  | Grows with window size          | Bounded (~100 entries)          | - |
| DB / I/O / RPCs                  | -                               | -                               | No change |
| Net production LOC (approx.)     | -                               | +~65 lines                      | Excluding comments, docs & tests |

**Guidance:**
- Include a complexity row when the shape of the work actually changed.
- Prioritize allocations and real work done on the common (happy) path.
- Treat production LOC as audit cost — disclose the approximate delta.
- You may add, remove, or rename rows. A table is not required if it would not improve clarity.

If the change has no meaningful performance or resource impact, state that clearly.

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

# Via structural rules (ast-grep)
# Run for sibling-paired paths (see .github/sibling-paths.yml) or other high-risk areas.
# Use `just via-check-strict` when required. See root AGENTS.md → Validation section.
just via-check
just via-check-strict

cargo test -p <relevant-crate>

# Secret scanning
gitleaks protect --staged --redact --no-banner
gitleaks git --log-opts="main..HEAD" --redact --no-banner
```

## Author checklist

- [ ] PR title follows Conventional Commits.
- [ ] Tests, docs, formatting, and linting were updated or marked not applicable.
- [ ] Ran `just via-check` (and `just via-check-strict` where required) and recorded results in the Checks run section.

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
