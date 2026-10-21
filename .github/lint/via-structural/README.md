# Via Structural Rules

This directory contains **Via-specific structural review aids**.

These rules exist to protect high-risk areas in Via code (especially reorg detection, L1 sync, BTC integration, and
related ordering/identity logic) from classes of bugs that are easy to introduce and hard to catch in normal review or
testing.

## Purpose

The rules are deliberately narrow and focused on known dangerous patterns that have caused real incidents (see PR #355
for the motivating example).

They are **not** general Rust linting. They are review prompts that force explicit consideration of ordering, identity,
and async assumptions in critical paths.

## Scope

These rules only apply to Via-specific code paths. They are intentionally kept separate from the generic ZK Stack lint
tooling (`zkstack dev lint`).

## Current Tools

- [ast-grep](./ast-grep/) — Structural pattern matching using tree-sitter-based rules.
- [Via duplication tool](../../../via_duplication/) — Exact new or changed copies in Via-owned Rust, plus advisory reports.

## Running the Checks

```bash
# Advisory mode (default) — shows findings but exits 0
.github/scripts/check-via-structural-rules.sh

# Strict mode — exits non-zero on new unbaselined findings or scanner errors
VIA_STRUCTURAL_RULES_MODE=strict .github/scripts/check-via-structural-rules.sh

# Requires stable Rust, Node/npm, Git, tar, rg, and just >=1.29
just via-dup-check
just via-dup-check --base HEAD^1  # CI's merge-commit first parent
just via-dup-boundaries
just via-dup-inventory > /tmp/via-duplication.md
```

Each command installs registry `jscpd@latest` once in temporary storage, requires a release of at least 5.4.2,
and reports the actual version. No npm dependency or duplication baseline is committed.
The native settings are in `via_duplication/jscpd.json`. Boundary rules are in repository-root
`via_duplication_boundaries.json`, because native globs resolve below the config directory.
The local gate compares the working tree, including dirty and untracked Rust, with
`git merge-base HEAD origin/main`. Use `--base REF` to select an exact commit.
Each invocation regenerates a disposable native fingerprint baseline from that base.

The gate returns 0 for a pass, 1 for blocking exact copies, and 2 for a tool or setup error.
New or changed Via–Via copies block. New Via–upstream file pairs block; changed copies in
previously shared pairs remain advisory. Upstream–upstream copies are excluded.
Check lists new findings and copies in changed files; untouched old copies appear only as a count.
Renamed listings in check are also limited to changed files. Inventory keeps the complete lists.
Renamed copies and shared-owner boundaries are advisory. Separate `contracts/` and `via-core-ext/`
submodules are excluded from all scans, matching the base export.
See [the decided policy](../../../docs/research/duplication-detection-with-jscpd.md).

For an intentional copy, use native `jscpd:ignore-start` and `jscpd:ignore-end` markers with a reason.
The line-based reason check is blocking independently of scanner findings and needs no ast-grep:

```bash
bash .github/scripts/check-via-structural-rules.sh --duplication-ignore-reasons-only
```

An urgent PR can defer duplication exit 1 with a current admin-applied `duplication-deferred` label
and a PR-body line `Duplication-deferred: <reason>`. This never waives exit 2 or a missing ignore reason.
CI always runs the required `Via Duplication` job and publishes its summary and one updated comment.
The separate trusted publisher handles fork PRs without running PR code with write permission.

## Philosophy

- A match does **not** automatically mean the code is wrong.
- A match means: "This pattern has bitten us before. Please explicitly justify why the ordering/identity contract is
  still safe here."
- The ast-grep rules are advisory by default. The duplication gate and ignore-reason check are blocking.
- Strict mode uses `ast-grep/baseline.txt` during rollout: known findings stay visible, but only new unbaselined
  findings fail the check.
- Over time, baseline entries should be removed as the underlying code is fixed or the rule is narrowed.

## Future

This directory is designed to hold multiple complementary mechanisms (ast-grep, custom lints, property testing helpers,
etc.) under the same `via-structural` umbrella as we strengthen defenses around the most dangerous parts of the Via
runtime.
