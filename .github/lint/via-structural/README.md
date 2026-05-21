# Via Structural Rules

This directory contains **Via-specific structural review aids**.

These rules exist to protect high-risk areas in Via code (especially reorg detection, L1 sync, BTC integration, and related ordering/identity logic) from classes of bugs that are easy to introduce and hard to catch in normal review or testing.

## Purpose

The rules are deliberately narrow and focused on known dangerous patterns that have caused real incidents (see PR #355 for the motivating example).

They are **not** general Rust linting. They are review prompts that force explicit consideration of ordering, identity, and async assumptions in critical paths.

## Scope

These rules only apply to Via-specific code paths. They are intentionally kept separate from the generic ZK Stack lint tooling (`zkstack dev lint`).

## Current Tools

- [ast-grep](./ast-grep/) — Structural pattern matching using tree-sitter-based rules.

## Running the Checks

```bash
# Advisory mode (default) — shows findings but exits 0
.github/scripts/check-via-structural-rules.sh

# Strict mode — exits non-zero on new unbaselined findings or scanner errors
VIA_STRUCTURAL_RULES_MODE=strict .github/scripts/check-via-structural-rules.sh
```

## Philosophy

- A match does **not** automatically mean the code is wrong.
- A match means: "This pattern has bitten us before. Please explicitly justify why the ordering/identity contract is still safe here."
- The rules are advisory by default so they can be introduced without breaking existing code while the team builds confidence in them.
- Strict mode uses `ast-grep/baseline.txt` during rollout: known findings stay visible, but only new unbaselined findings fail the check.
- Over time, baseline entries should be removed as the underlying code is fixed or the rule is narrowed.

## Future

This directory is designed to hold multiple complementary mechanisms (ast-grep, custom lints, property testing helpers, etc.) under the same `via-structural` umbrella as we strengthen defenses around the most dangerous parts of the Via runtime.
