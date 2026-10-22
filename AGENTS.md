# AGENTS.md

## Purpose

This repository owns Via source and runtime behavior. Deployment belongs to the
deployment repositories. Source changes do not prove rollout.

Via is a zkSync Era fork. Keep Via features cohesive and minimize divergence so
upstream updates and a future re-fork remain manageable. The re-fork target is
no longer zksync-era `main`. For re-fork planning, read
`docs/research/refork-target-and-upstream-direction.md` and
`docs/research/refork-preparation-prior-art.md`. When changing Via-owned
behavior, read the target note's "What current via-core work does meanwhile"
section.

## Authority

An implementation request covers its local edits, local disposable tests and
fixes for failures it caused. Finish the work and its checks without asking
again. Report unrelated failures without expanding scope or waiving required
gates.

Ask only before deploying, changing live or shared systems, spending funds, or
using real keys, wallets or nodes, unless the user already approved that action.
One approval covers the steps it names. If one part is blocked, continue with
the rest.

This repository is public. Keep secrets and live credentials out of commits.
Publish private findings only with disclosure approval. Exclude local agent
scratch files from commits.

## Reuse and duplication discipline

Before changing production logic, identify the existing owner and follow its
callers and callees beyond `via_*`. Inspect related main-node and verifier
implementations using `.github/sibling-paths.yml`.

DA siblings have different names, so search `core/lib/via_da_clients` and
`via_verifier/lib/via_da_client`.

Prefer existing Via extensions before modifying inherited upstream code.
Explain why they cannot serve the change. See `docs/via_guides/architecture.md`.

Shared behavior belongs in `core/lib/` or its established shared crate. Keep
node-specific scheduling, state and shutdown in the node layer. Explain
deliberate duplication in the PR template.

Update `.github/sibling-paths.yml`, `.github/via-scopes.yml` and
`.github/via-areas.yml` when their relationships or paths change.

## Read when relevant

- Runtime behavior: read the relevant `docs/via_guides/` guide and nearest crate
  README or config.
- Unfamiliar design or implementation questions: use the research workflow in
  `docs/research/README.md#research-before-and-during-design-and-implementation`.
  Reuse evidence for mechanical changes.
- Withdrawal, coordinator authentication, signing, proof, deposit, upgrade or
  monitoring changes: read the relevant section of
  `docs/design/via-correctness-and-monitoring/README.md` and its accepted ADRs.
- Lasting design rationale or an evidenced workflow gap: use the matching
  section of `docs/research/README.md`.

## Principle skills

Use these skills for the matching task, if installed:

- API replacement: `principle-migrate-callers-then-delete-legacy-apis`.
- Retries and restarts: `principle-make-operations-idempotent`.
- Concurrent writes: `principle-separate-before-serializing-shared-state`.
- Debugging: `principle-fix-root-causes`.
- Tests: `principle-test-behavior-not-implementation`.

For other situational principles, use the conditional index in
`docs/research/README.md#principle-skills-for-concrete-implementation-questions`.

## Agent skills

- Issue tracker: GitHub issues, with local `.scratch/issues/` files for
  private detail. See `docs/agents/issue-tracker.md`.
- Domain docs: single context, `GLOSSARY.md` and `docs/adr/`. See
  `docs/agents/domain.md`.
- New glossary terms and ADRs: use the `domain-modeling` skill.

## Change discipline

Preserve production error context. Preserve or document async fetch/compare
ordering assumptions.

Bitcoin, DA, reorg, verifier/prover, serialization, hashes, signatures and
inscriptions are protocol-sensitive. Inspect producers and consumers before
changing behavior. Changes to accepted messages, serialized bytes, hash inputs
or persisted meaning are behavior changes. State the compatibility impact and
add focused tests or replay evidence for affected producers and consumers.
Verify txid byte order at Bitcoin boundaries.

Preserve necessary external and rolling-upgrade compatibility, with a removal
condition for temporary paths. For bug fixes, add focused observable regression
coverage where practical. When metric contracts change, update affected
dashboards and alerts.

## Source comment discipline

Comments explain durable invariants, consequences and operator actions. State
cross-component coupling once on its governing type. Put task history in the PR
or issue. For adopted or adapted designs, explain the concrete consequence
beside the code and cite a pinned origin.

## Code Review Rules

Review correctness and common-path complexity, allocations, copies, DB calls,
RPCs, locks, serialization and cache behavior for protocol-sensitive or hot
paths. Flag unjustified duplication and missing sibling checks. Verify reviewer
suggestions against source and tests.

## Validation

Before pushing, run `git diff --check` and relevant existing checks. Format Rust
as [docs/formatting.md](docs/formatting.md) describes:

- Via-owned paths: `scripts/format-changed.sh --check-config` and
  `scripts/format-changed.sh --check --base origin/<PR-base-branch>`.
- Do not run `cargo fmt`, `zkstack dev fmt` without `--check`, or editor
  formatting on Via paths. They rewrite existing 120-column code to the
  100-column profiles.
- Upstream-owned paths: `zkstack dev fmt --check`. Findings on untouched Via
  files are expected.

For Rust changes, also run:

```bash
zkstack dev lint --check
cargo test -p <crate>
just via-check
```

For every Rust change, run `just via-dup-check`. It compares the working tree with
the merge-base of `HEAD` and `origin/main`; `just via-dup-check --base REF` uses REF exactly.

- Exit 1 (blocking copy): move shared logic to its existing owner under the reuse rules,
  or mark an intentional copy with `jscpd:ignore-start <reason>` and `jscpd:ignore-end`.
  Never add a marker only to pass. Stop and ask before merging main-node and verifier
  copies or protocol-sensitive copies.
- Exit 2: a tool or setup error. Fix the cause; it is not a duplication finding.

`just via-dup-boundaries` (advisory) and `just via-dup-inventory` (existing copies) are
optional. For intentional copies and CI deferral, see `.github/lint/via-structural/README.md`.

Also run `just via-check-strict` for paths covered by
`.github/sibling-paths.yml`, `.github/lint/via-structural/ast-grep/rules/` or
`.github/scripts/check-via-structural-rules.sh`. Its duplication and structural
gates must pass. Advisory checks do not replace it. Explain false positives
rather than suppressing rules without justification.

Documentation-only changes need content and reference checks. The strict path
gate still applies. Report checks not run and why in the PR's What changed
section. Reuse successful checks until
relevant inputs change or a failure raises a new concern. For CI changes, verify
the resulting workflow run.

## Delivery

Use the appropriate issue template and `.github/pull_request_template.md` for
the problem, the change and reuse evidence.

For requested commits, use `<type>(via-<area>): <imperative subject>`.
Choose scopes from `.github/via-scopes.yml`. Use `upstream-sync` for upstream
integration and preserve imported commit messages.

Nested AGENTS.md files contain local ownership and constraints. Point to these
root rules and the sibling map rather than repeating workflows.
