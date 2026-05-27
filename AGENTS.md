# AGENTS.md

## Scope

Applies to this repository and all descendants unless a nested `AGENTS.md` adds more specific instructions.

## Purpose

This repository owns Via source and runtime behavior. It does not own live or desired-state deployment.

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
- Prefer Via-specific `via_` modules when extending fork behavior.
- Follow the call graph into upstream (non-`via_*`) code when necessary, and explain why a Via extension was insufficient.
- For Bitcoin work, remember that txids are byte-reversed.

## Safety rules

- Never commit secrets or live credentials.
- Do not run migrations or deploy from this repo without explicit approval.
- Keep local agent scratch directories (`.gitnexus/`, `.agents/`, etc.) out of commits.

## Reuse and duplication discipline

Apply this section before writing implementation code. The PR template’s *Reuse & Duplication* and *Performance, Complexity, and Resource Impact* sections cannot be filled in honestly after the fact — they exist to surface the reasoning required here.

**Production code is permanent audit cost.** Every new function, detector, client, poller, watcher, or worker adds ongoing review and maintenance burden. Production LOC is measured as **net delta** (lines added minus lines removed, excluding comments, documentation, tests, and generated files). Before writing new code, identify the existing function or module that should own the behavior. If you cannot name it, you are likely introducing duplication.

**Do not tunnel on `via_*`.** `via_*` directories are not hard boundaries. Follow the call graph into upstream and non-`via_*` modules. Always check sibling implementations across node types (main-node vs. verifier, `core/` vs. `via_verifier/`) before adding new logic.

Detector, poller, watcher, and fetch/compare logic copied across main-node and verifier paths is the canonical anti-pattern this section exists to prevent (see PRs #355–#360).

**Mandatory pre-coding check.** Before creating a new file, struct, detector, poller, watcher, client, DAL method, parser, or conversion in a `via_*` path, complete the following:

1. Name the closest existing function or module you considered extending or replacing.
2. List the specific sibling paths you inspected (main-node ↔ verifier, `core/` ↔ `via_verifier/`). For reorg-related work this must include both `core/node/via_main_node_reorg_detector/` and `via_verifier/node/via_reorg_detector/`.
3. If you are not extracting shared logic, state the exact invariant, ownership boundary, or execution-context difference that prevents extraction.

If you cannot complete the steps above, stop and do the search before writing implementation code.

### Anti-pattern vs Required Pattern (example)

**Anti-pattern** (duplication of sibling logic):

- Similar detector / poller / watcher logic added in both a `core/node/` path and the corresponding `via_verifier/node/` path with only minor differences.
- No attempt to extract shared behavior.

**Required pattern**:

- Shared logic extracted to `core/lib/` (or the most appropriate shared crate).
- Only thin node-specific wrappers remain in `core/node/` and `via_verifier/node/`.

This pattern applies to any duplicated logic across main-node and verifier (not just reorg detectors).

### Maintaining supporting files

When new duplication patterns, high-risk areas, or sibling relationships are discovered, update the following files:

- `.github/sibling-paths.yml` — Add new main-node ↔ verifier path pairs.
- `.github/via-scopes.yml` — Add new `via-*` scopes when a recurring area of work appears.
- `.github/via-areas.yml` — Map new paths to the appropriate `Via-Area` value.

Also consider adding a small `AGENTS.md` file in the relevant high-risk directory if one does not already exist.

## Commit Message Convention

Use Conventional Commits. All Via-specific changes must use a `via-` scope.

**Format:**

```text
<type>(via-<area>): <imperative subject>
```

**Allowed types:** `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `chore`, `build`, `ci`, `revert`

**Examples:**

- `feat(via-reorg): detect deep reorgs from DA layer`
- `fix(via-btc): correct txid byte-reversal in mempool watcher`
- `refactor(via-da): extract shared inclusion proof parsing`
- `perf(via-verifier): batch L1 batch metadata loads`
- `fix(via-reorg): align reorg handling with new upstream batch finality rules`
- `chore(upstream-sync): merge ZKsync vX.Y.Z`

**Scopes:** See `.github/via-scopes.yml` for the current list of allowed scopes, including Via-specific `via-*` scopes and the non-Via `upstream-sync` scope.

When merging upstream ZKsync changes that are **not** Via-specific, use the `upstream-sync` scope instead of a `via-` scope.

## Change discipline

- Include a regression test for bug fixes when practical.
- Preserve rich error context (`anyhow::Context`, `with_context`, `?`) in production paths. Never strip it to shorten a diff.
- When changing async fetch/compare code, preserve or explicitly document ordering assumptions.
- Protocol-sensitive areas (Bitcoin, DA, reorg, verifier/prover, serialization, hashes, signatures, inscriptions) require extra care. Search call sites and downstream consumers before changing them.

Reuse and duplication rules are defined in the dedicated section above.

## Source comment discipline

Source comments describe durable runtime truth: invariants, contracts, and
why the obvious alternative is wrong. They are not a log of how the code
was produced.

**Allowed:** contract/doc comments on `pub` items, non-obvious invariants,
footgun/sentinel values, why-not-alternative reasoning, cross-component
coupling (stated once on the governing type), performance/ordering
constraints a refactor would violate, external references (BIP, RFC, CVE).

**Not allowed in `.rs` files** — put in commit messages, PR descriptions,
or the issue tracker instead:

- Debugging history, "how we found it" narratives, pre/post-fix comparisons.
- Private environment names, host names, customer names, dates,
  incident-specific block heights or addresses.
- References to `AGENTS.md`, lint rule filenames, or PR numbers.
- Strategy jargon ("defense-in-depth", "future-proofing", "belt-and-suspenders").
- Narration of the next statement. If unclear, rename or restructure.

**Style:** declarative present tense; one idea per comment; state each
contract once; prefer stating consequences over shouting prohibitions;
on `pub` items and critical internal functions in shared or high-risk crates,
lead with plain-language meaning and the required operator action before
internal terminology;
aim ≤15 % comment lines in high-risk files (20 % ceiling).

### High-risk-change checklist

Before pushing to BTC, DA, reorg, verifier, prover, or sibling paths
(`.github/sibling-paths.yml`): review the comment diff separately and
ask of every added comment:

> *Does this state a durable invariant, contract, footgun, why-not, or
> cross-component coupling? If not, move it to the PR description.*

### Anti-pattern → required pattern

```rust
// ✗  Regression for the Hetzner private external-node rehearsal:
//    The pre-fix positional `zip` paired DB row 0 (height 100_891) with
//    canonical row 0 (height 100_792) and reported a false reorg.
//    Caller MUST treat SparseAt as inconclusive and skip — never demote the
//    `via_btc_watch` cursor. See AGENTS.md §Reuse and the structural rule
//    via-reorg-height-association-required.yml (defense-in-depth).

// ✓  Not a reorg. Canonical data for this height was missing, so the
//    comparison is incomplete. Do not demote or move the `via_btc_watch`
//    cursor; do not trigger reorg handling.
```

Incident details belong in commit messages and PR descriptions.

## Review Expectations

When making changes, consider both correctness **and** performance.

High-risk areas (reorg detection, L1 sync, BTC integration, DA, and hot database paths) require explicit reasoning about time complexity, allocations, and cache behavior.

Reason about the common (happy) path, not only worst-case Big-O. State the expected work in concrete terms: allocations per operation, DB calls, RPCs, locks held, serialization, background work, and approximate production LOC added. Asymptotic complexity hides the constants that matter at production throughput.

Unjustified duplication or missing sibling checks are grounds for blocking merge.

## Validation

Run the standard local checks before pushing:

```bash
git diff --check
zkstack dev fmt
zkstack dev lint
cargo test -p <crate>
just via-check          # structural lint (ast-grep), advisory
```

**Structural lint (blocking for sibling-paired paths)**

If your change touches any of the following, run `just via-check-strict` and ensure it passes before pushing:

- `core/node/via_main_node_reorg_detector/`
- `via_verifier/node/via_reorg_detector/`
- Any path pair listed in `.github/sibling-paths.yml`
- `.github/lint/via-structural/ast-grep/rules/` or `.github/scripts/check-via-structural-rules.sh`

`just via-check` (and `just via-check-strict` for sibling-paired paths) runs structural lint rules that catch common duplication, ordering, and identity anti-patterns across main-node and verifier code. These rules are maintained in `.github/lint/via-structural/ast-grep/rules/`. `zkstack dev lint` does not cover them.

A pre-push hook runs these rules in advisory mode for reorg detector paths as a safety net, but do not rely on it — run the command yourself.

If a rule fires and you believe it is a false positive, document the reasoning in the PR description. Do not silence rules without justification. Rule sources live in `.github/lint/via-structural/ast-grep/rules/`.

## GitHub issues and PRs

Use the appropriate issue template. For runtime, protocol, L1/BTC/reorg, verifier, or external-node issues, do not use free-form issues.

All PRs must follow `.github/pull_request_template.md`. Write descriptions for human reviewers and operators, not as an agent/tool audit log. Focus on the “Why”, what actually changed, what did not change, the risk, and the checks that were run. Use impersonal, operator-facing wording and name the component, crate, runtime invariant, config key, deployment boundary, or operator action instead.

## Tooling and cross-repo notes

- Use GitNexus for non-trivial cross-repo impact analysis.
- Treat `kube-state` and `helm-charts` as deployment context, not runtime proof.
- CodeRabbit reviews are advisory. Verify suggestions against source and tests.

## Directory-level guidance

High-risk directories should contain a small `AGENTS.md` that points back to this section. This is often the most effective way to ensure agents encounter the relevant rules at the point of use.
