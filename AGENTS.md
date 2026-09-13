# AGENTS.md

## Scope

Applies to this repository and all descendants unless a nested `AGENTS.md` adds more specific instructions.

## Purpose

This repository owns Via source and runtime behavior. It does not own live or desired-state deployment.

Via is a fork of zkSync Era. Keep Via-specific features cohesive and minimize unnecessary divergence so they remain maintainable through upstream updates or a future re-fork.

## Read first

Before changing non-trivial runtime behavior, read the relevant Via guide and the nearest crate README or config.

## Important paths

- Runtime: `core/`
- Verifier: `via_verifier/`
- Prover / Indexer: `prover/`, `via_indexer/`
- Bitcoin: `core/lib/via_btc_client/`, `core/node/via_btc_*`, and verifier BTC paths
- DA / Reorg: `core/lib/via_da_clients/`, `core/node/via_main_node_reorg_detector/`, and `via_verifier/node/via_reorg_detector/`. (When touching one, check the other — see *Reuse and duplication discipline*.)

## Source-of-truth rules

- Source code describes behavior; it does not prove deployment.
- Prefer existing `via_` modules and extension interfaces over editing inherited upstream code. Reuse existing Via implementations before creating another copy, following the fork customization convention in `docs/via_guides/architecture.md`.
- Explain why an existing Via extension is insufficient before modifying inherited upstream code.
- For Bitcoin work, remember that txids are byte-reversed.

## Safety rules

- Never commit secrets or live credentials.
- Obtain explicit approval for deployments, live or shared-state changes, and migrations outside disposable local test fixtures.
- Keep local agent scratch directories (`.gitnexus/`, `.agents/`, etc.) out of commits.
- Keep private findings out of public issues, PRs, and comments until disclosure is approved.

Within an agreed implementation task, run relevant local checks and fix failures caused by the change without per-step approval when their targets are confirmed to be disposable and isolated from live systems. Do not assume all tests have that property. Continue through verification of the requested outcome.

## Reuse and duplication discipline

Before adding or changing production logic:

- Identify the closest existing function, module, or crate that could own the behavior.
- **Do not tunnel on `via_*`.** The prefix identifies ownership, not a hard code boundary or a compatibility guarantee. Follow callers and callees into upstream and non-`via_*` modules. Always inspect sibling implementations across main-node and verifier, including `core/` and `via_verifier/`. `.github/sibling-paths.yml` is the canonical pair list; reorg work includes both main-node and verifier detectors.
- Extend the existing owner or extract genuinely shared behavior into `core/lib/` or the appropriate shared crate. Do not create shared abstractions solely for hypothetical reuse.
- Keep node-specific execution concerns in the node layer. If similar logic remains separate, explain the invariant, ownership boundary, or execution-context difference.

Complete these ownership checks before implementation. Record the evidence required by `.github/pull_request_template.md` when preparing the PR.

### Anti-pattern and preferred pattern

- **Anti-pattern:** Copy detector, poller, or fetch/compare logic into main-node and verifier paths with only minor differences.
- **Preferred:** Keep the shared fetch/compare behavior in one implementation in the appropriate shared crate. Node-specific adapters retain their own state, scheduling, and shutdown ownership.

### Maintaining supporting files

- Update `.github/sibling-paths.yml` when sibling relationships change.
- Add recurring scopes to `.github/via-scopes.yml` and map new paths in `.github/via-areas.yml`.

## Commit Message Convention

For Via-specific changes, use Conventional Commits: `<type>(via-<area>): <imperative subject>`.

Allowed types: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `chore`, `build`, `ci`, `revert`.

Use the scopes in `.github/via-scopes.yml`. For upstream-integration commits without Via-specific changes, use `<type>(upstream-sync): <imperative subject>`. Preserve the original messages of imported upstream commits.

Examples:

- `feat(via-reorg): detect deep reorgs from the DA layer`
- `fix(via-btc): correct txid byte reversal in the mempool watcher`
- `refactor(via-da): extract shared inclusion proof parsing`
- `perf(via-verifier): batch L1 batch metadata loads`
- `chore(upstream-sync): merge ZKsync vX.Y.Z`

## Change discipline

- Include a regression test for bug fixes when practical.
- Preserve rich error context (`anyhow::Context`, `with_context`, `?`) in production paths. Never strip it to shorten a diff.
- When changing async fetch/compare code, preserve or explicitly document ordering assumptions.
- Protocol-sensitive areas (Bitcoin, DA, reorg, verifier/prover, serialization, hashes, signatures, inscriptions) require extra care. Search call sites and downstream consumers before changing them.
- Changes to accepted messages, serialized bytes, hash inputs, or persisted-data meaning are behavior changes, not behavior-preserving refactors. State the compatibility impact and provide focused tests or replay evidence for affected producers and consumers.
- When replacing an internal API or implementation, migrate all consumers and remove the obsolete path in the same change. Retain compatibility layers only for an identified external or rolling-upgrade requirement, with an explicit removal condition. Include dashboards and alerts when changing metric contracts.

## Source comment discipline

Source comments explain durable runtime truth: contracts, invariants, non-obvious consequences, ordering or performance constraints, and why an obvious alternative is wrong. State cross-component coupling once on its governing type.

Use declarative, plain language and one idea per comment. On public items and critical shared functions, explain the meaning and required operator action before internal terminology. Prefer clear names and structure over narration of the next statement.

Keep debugging history, incident-specific details, private environment names, agent instructions, lint filenames, PR references, and strategy jargon out of `.rs` comments. Put relevant history in the PR or issue; retain useful external protocol references such as BIPs and RFCs.

In high-risk files, aim for at most 15% comment lines, with a 20% ceiling. Before pushing BTC, DA, reorg, verifier, prover, or sibling-paired changes, review added comments against this policy.

## Review Expectations

Review correctness and performance. For protocol-sensitive or hot paths, explain complexity and common-path work: allocations, copies, DB calls, RPCs, locks, serialization, background work, and cache behavior where relevant.

Report approximate net production LOC as audit cost: additions minus removals, excluding comments, documentation, tests, and generated files. Do not shorten a diff at the expense of correctness or error context.

Unjustified duplication or missing sibling checks are grounds for blocking merge.

## Validation

Before pushing, run `git diff --check` and checks relevant to the changed paths. For Rust changes, also run:

```bash
zkstack dev fmt
zkstack dev lint
cargo test -p <crate>
just via-check          # structural lint (ast-grep), advisory
```

Documentation-only changes need content and reference checks rather than Rust suites. Record checks not applicable to the change with a reason in the PR. The strict path-based gate below still applies.

Run `just via-check-strict` and ensure it passes before pushing changes under any path in `.github/sibling-paths.yml`, `.github/lint/via-structural/ast-grep/rules/`, or `.github/scripts/check-via-structural-rules.sh`.

The strict command includes the duplication ratchet and structural lint. `zkstack dev lint` and advisory pre-push hooks do not replace it. Document structural-rule false positives in the PR; do not silence rules without justification.

## GitHub issues and PRs

Use the appropriate issue template. For runtime, protocol, L1/BTC/reorg, verifier, or external-node issues, do not use free-form issues.

Follow `.github/pull_request_template.md`, including its reuse evidence, performance, and validation requirements where applicable. Write for reviewers and operators: explain why, behavior changes, risks, boundaries, and checks actually run. Use impersonal wording that names the affected component or contract rather than an agent/tool activity log.

## Tooling and cross-repo notes

- Choose source inspection, LSP, or graph tools according to the question. GitNexus is useful for non-trivial cross-repo analysis; verify graph results against checked-out source. Missing or stale graph data does not prevent source-based analysis.
- Use symbol-aware tooling for cross-file renames rather than textual replacement.
- Treat `kube-state` and `helm-charts` as deployment context, not runtime proof.
- CodeRabbit reviews are advisory. Verify suggestions against source and tests.

## Directory-level guidance

Keep nested `AGENTS.md` files focused on local ownership and constraints. Refer to these root rules and `.github/sibling-paths.yml` rather than repeating general workflows.
