# AGENTS.md

## Scope

Applies to this repository root and all descendants unless a nested `AGENTS.md` adds more specific instructions.
This repository contains Via Network protocol and runtime code. Keep changes reviewable, and grounded in source behavior, tests, and deployment facts.

## Purpose

This repo owns Via source/runtime behavior: main node, external node, verifier, indexer, BTC sender, prover, contracts, config semantics, docs, and developer tooling that ship with `via-core`.

This repo does not own live or desired-state deployment. Use sibling repos for Kubernetes, Helm, Hetzner/OpenTofu/Ansible, GCP/Terragrunt, explorer code, and durable incident synthesis. Source changes here can affect future builds, but do not prove what is currently deployed.

## Read first

- Before changing non-trivial runtime behavior, read the relevant Via guide
  (`docs/via_guides/development.md`, `docs/via_guides/architecture.md`, or
  `docs/via_guides/data-flow.md`) and the nearest crate README/config; do not
  infer behavior from file names alone.

## Important paths

- Runtime: `core/`; verifier: `via_verifier/`; prover/indexer: `prover/`, `via_indexer/`.
- Bitcoin: `core/lib/via_btc_client/`, `core/node/via_btc_*/`, verifier BTC paths, and `core/lib/dal/src/via_btc_sender_dal.rs`.
- DA/reorg: `core/lib/via_da_clients/`, verifier DA paths, `core/node/via_main_node_reorg_detector/`, and `via_verifier/node/via_reorg_detector/`.
- Contracts/config/CLI/docs/examples: `contracts/`, `configs/`, `etc/`, `zkstack_cli/`, `docs/`, `docker/`, `.github/`.

## Source-of-truth rules

- Keep source, desired state, and live state separate: repo code does not prove
  deployment, and deployment repos do not prove runtime behavior. Verify against
  source, kube/Helm/infra repos, live cluster/host state, public APIs, DB state,
  or tests as appropriate.
- Do not assume upstream ZKsync behavior is unchanged after Via-specific forks.
  Check Via files and upstream references before classifying a divergence.
- Treat current upstream ZKsync `main` documentation as a reference, not as a
  direct source of repo rules. Before importing newer upstream guidance, verify
  the referenced files, commands, and upgrade architecture exist in this repo's
  current ZKsync lineage and Via-specific fork state.
- Prefer Via-specific `via_` modules, crates, services, binaries, and components
  when extending fork behavior. Touch upstream-derived non-`via_` code only when
  the task requires that exact path or a Via component depends on it, and explain
  why a `via_` extension was not enough.
- For Bitcoin transaction investigations, check byte-reversed 32-byte txids
  before declaring a transaction missing from public explorers or node RPC.

## Safety rules

- Never commit secrets, private keys, wallet material, database URLs, RPC
  passwords, cookies, decrypted env files, or generated local state.
- Do not query or print live secrets from deployment repos or clusters while
  working in this repo.
- Do not deploy, restart workloads, run migrations against live databases, or
  mutate live infrastructure from this repo without explicit operator approval.
- Keep local diagnostic artifacts out of commits. Use `.git/info/exclude` for
  local agent/indexing scratch directories such as `.gitnexus/`, `.claude/`, or
  similar tool output if they appear.

## Change discipline

- For bug fixes, include a regression test when the affected crate/test harness
  makes it practical.
- Preserve rich error context in production/library paths. Prefer propagated
  errors and `anyhow::Context` / `with_context` over flattening failures into
  generic strings. Avoid `unwrap()` / `expect()` in non-test runtime paths.
- Make examples model safe usage when they are copied by operators or integrators;
  do not leave panic-prone examples around hardened library APIs.
- Compact-by-default: prefer one-line expressions and short helper calls when
  they remain readable and rustfmt accepts them; avoid manual wrapping churn and
  do not reformat unrelated existing code just to change line shape.
- Reuse-first: search for the canonical abstraction before adding helpers,
  parsers, clients, DAL methods, conversions, or runtime glue. Prefer extension
  over near-duplicates; if new code is needed, place it beside the closest owner
  and explain the non-reuse decision in the PR.
- For runtime behavior changes that affect deployments, document the required
  deployment/config follow-up in the PR body. Do not imply live rollout has
  happened unless it has been verified separately.
- When touching duplicated main-node/verifier/external-node logic, check whether
  the same bug exists in the sibling implementation before stopping.
- When changing async fetch/compare code, preserve or explicitly encode ordering
  assumptions. Do not compare height-indexed data by vector position unless the
  ordering contract is proven and tested.
- For `zkstack_cli/` Rust changes, rebuild the installed CLI with
  `zkstackup --local` before validating CLI behavior.
- For Forge/deployment/upgrade flows, verify generated paths, env vars,
  deployment state, and call order against Via's actual command flow. Prefer
  fail-fast root-cause fixes over broad fallbacks, `try/catch`, or low-level
  `staticcall` probes that hide missing deployments or ordering bugs; use
  `cast run` or equivalent tracing for opaque failures.
- Protocol-sensitive areas include Bitcoin, DA/Celestia, verifier/prover, state
  keeper, reorg/reverter, contracts/upgrades, migrations/DAL, serialization,
  hashes, signatures, byte order, and inscriptions. Search call sites and
  downstream consumers before changing APIs, config keys, DB schema, metrics, or
  encoded data.
- Long-lived services in `core/node` and `via_verifier/node` often communicate
  through database state rather than direct calls. Before changing one, identify
  the table/query it polls, rows it creates or updates, downstream consumers, and
  retry/error/stop-signal behavior.

## GitNexus and cross-repo review

- Use GitNexus before non-trivial impact analysis or Via cross-repo review; then
  verify decisive graph findings against exact source files.
- Treat `kube-state` and `helm-charts` as deployment/config context, not live
  proof. Keep `.gitnexus/`, `.claude/`, `.rooignore`, and similar agent artifacts
  local/excluded unless an ignore-policy PR is explicitly intended.

## Validation

Choose checks that match the files changed and report exactly what ran.

Common local gates:

```bash
git diff --check
zkstack dev fmt
zkstack dev lint
cargo test -p <crate-or-package>
```

For targeted Rust changes, prefer the narrowest meaningful crate tests first,
then broader checks if the change is shared or safety-critical. If a command
cannot run locally, state why and list the source inspection or narrower check
used instead.

## PR descriptions

Use `.github/pull_request_template.md` for PR bodies.

Write PR descriptions for a human reviewer/operator, not as an agent/tool audit
log. Explain why the change exists, what behavior changes, what intentionally did
not change, how to review the risk, which checks ran, and whether any live
infrastructure action occurred.

Use impersonal, operator-facing wording. Avoid first-person singular or plural
phrasing. Name the component, crate, runtime invariant, config key, deployment
boundary, or operator action instead.

For runtime/safety PRs, the `Why` section should describe the observed failure
mode and the invariant the code should enforce. The `What did not change` section
should prevent unsafe assumptions, especially around live rollout, database
mutation, secrets, deployment desired state, and follow-up infra work.
