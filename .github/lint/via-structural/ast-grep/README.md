# Via Structural Rules — ast-grep

This folder contains [ast-grep](https://ast-grep.github.io/) rules for Via-specific structural patterns that have historically led to subtle but high-impact bugs.

## Why ast-grep?

ast-grep provides fast, structural (tree-sitter based) pattern matching on Rust source. It is well suited for catching dangerous idioms that are hard to express with normal Clippy lints or simple regex (e.g., positional `zip` after `buffer_unordered`, manual pending UTXO chain management).

## Current Rules

| Rule File                    | ID                            | Purpose                                                                 | Status    |
|------------------------------|-------------------------------|-------------------------------------------------------------------------|-----------|
| `reorg-ordering.yml`         | `via-reorg-buffer-unordered`  | Flags `buffer_unordered` inside reorg detector crates. Results lose request order. | Advisory  |
| `via-reorg-height-association-required.yml` | `via-reorg-height-association-required` | Targets common dangerous zip patterns after async fetches in reorg detectors (range zips, `.iter().zip`, `.into_iter().zip`, `.iter_mut().zip`). Includes suppression guidance (`// height-order-guaranteed`). Best used together with `via-reorg-buffer-unordered`. | Advisory |
| `via-da-batch-before-proof.yml` | `via-da-batch-before-proof` | Flags calls to `dispatch_real_proofs` in the DA dispatcher. Advisory heuristic for batch-before-proof ordering. Requires scoping and review. | Candidate / Advisory |
| `via-avoid-duplicate-export.yml` | `via-avoid-duplicate-export` | Flags `pub use $B::$C` when `pub mod $B` is declared in the same file. Prevents unnecessary public API duplication and multiple canonical paths to the same item. | Advisory |

**Core identity being protected**: `bitcoin_block_height_hash` — Bitcoin blocks must be compared and ordered by **explicit height**, never by vector position or async completion order.

Some rule metadata references the Via Agent Harness, which is maintained outside
this repository. If the harness is not checked out locally, treat these as
external references rather than in-repo paths:

- `components/reorg-detector.md`
- `llm-wiki/generated/IDENTITIES.tsv` (especially `bitcoin_block_height_hash`)
- `llm-wiki/generated/STATE_TRANSITIONS.tsv`
- `llm-wiki/generated/RISK_REGISTER.md` (`risk.reorg.positional_height_comparison`)

## Running Locally

```bash
# Advisory mode (recommended for development)
just via-check

# Strict / blocking mode (use in CI for high-risk paths)
just via-check-strict
```

Direct invocation:

```bash
ast-grep scan --config .github/lint/via-structural/ast-grep/sgconfig.yml
```

**Local Availability**: ensure `ast-grep` is on your `PATH`. The CI workflow installs pinned `ast-grep` v0.42.0 into `$HOME/.local/bin` before running the structural rules.

**Harness Integration**: High-quality test and rule proposals live in the external harness under `proposals/critical/`.
The current location of the harness is declared in the harness root README (search for "Current Location").
Many proposals are directly derived from the risk register and are ready for implementation or rule writing. See especially REORG-01, DA-01/02, SK-01/02, CG-01/02, VRF-01/02, FINAL-01, BTC-WATCH-01.

## Refactoring with `ast-grep run --rewrite`

`ast-grep scan` (used by `just via-check` / `via-check-strict`) is for **detection** of dangerous structural patterns. For **mechanical, safe code transformations** across many sites, use `ast-grep run --rewrite`.

This is particularly useful when applying the same change to sibling implementations (main-node ↔ verifier) or to many similar call sites.

### Basic usage

```bash
# Preview changes as a diff (safe — nothing is written)
ast-grep run -p 'old_pattern' -r 'new_pattern' --lang rust path/

# Interactive mode — confirm or edit each change
ast-grep run -p '...' -r '...' --lang rust -i path/

# Apply all rewrites (review the diff first!)
ast-grep run -p '...' -r '...' --lang rust -U path/
```

By default `ast-grep run` only shows what *would* change. Use `-U` / `--update-all` (or `-i` / `--interactive`) to mutate files.

The `-p` / `--pattern` form (with `-r` for rewrite) is the quickest for one-off rewrites. For anything non-trivial, prefer a small rule file (see below).

### Realistic Rust example — adding error context to fetch calls

Both reorg detectors contain call sites like:

```rust
let blocks = self
    .fetch_blocks(from_block_height, to_block_height)
    .await?;
```

When these fail it is valuable to know the exact height range that was requested. We can mechanically add `.with_context(...)` at every matching site:

```bash
ast-grep run \
  -p 'self.fetch_blocks($FROM, $TO).await?' \
  -r 'self.fetch_blocks($FROM, $TO).await.with_context(|| format!("failed to fetch L1 blocks {}-{}", $FROM, $TO))?' \
  --lang rust \
  core/node/via_main_node_reorg_detector/src/lib.rs \
  via_verifier/node/via_reorg_detector/src/lib.rs
```

**Tested note**: The pattern and rewrite above were run against the current detector sources. The structural matcher successfully handled both single-line and multi-line call sites and correctly substituted the metavariables.

### Practical notes from testing

- Multi-line call chains are collapsed into single lines. Run `zkstack dev fmt` (or rustfmt) afterward.
- Rewrites using `.with_context(...)` require `use anyhow::Context;` in scope (already present in the reorg detectors).
- The direct `-r` / replacement string form is convenient for exploration but easy to get slightly wrong on complex expressions. For any rewrite you actually intend to keep, use a rule file instead.

### Using a rewrite rule file (preferred for real changes)

Create a temporary `.yml` file:

```yaml
id: add-fetch-context
language: Rust
rule:
  pattern: self.fetch_blocks($FROM, $TO).await?
fix: |
  self.fetch_blocks($FROM, $TO)
    .await
    .with_context(|| format!("failed to fetch L1 blocks {}-{}", $FROM, $TO))?
```

Run it with:

```bash
# Preview
ast-grep scan -r temp-rewrite.yml path/

# Apply after review
ast-grep scan -r temp-rewrite.yml -U path/
```

Rule files make the transformation explicit, reviewable, and easier to version alongside the resulting diff.

### Guardrails in this repository

- A rewrite is still a code change. Run `git diff --check`, review the diff, run relevant tests, and execute `just via-check` (or `just via-check-strict` for sibling-paired paths) before pushing.
- If the mechanical change highlights duplicated logic between `core/node/...` and `via_verifier/node/...`, prefer extracting the common behavior to `core/lib/` rather than applying the same rewrite in two places (see root AGENTS.md → *Reuse and duplication discipline*).
- Never use `-U` (or `--update-all`) in a fully automated way on production paths without a human in the loop.

This workflow complements the structural lint rules: use `scan` to find problems, use `run -p/-r` or `scan -r` to perform mechanical rewrites once the desired shape is agreed.

## Rule Philosophy & Lifecycle

- Rules start **advisory** by default.
- A match is a prompt for explanation, not an automatic defect.
- Rules target patterns that have previously caused real (or near-miss) bugs in Via reorg detection and BTC inscription logic.
- Preferred: narrow, high-signal rules over broad noisy ones.
- Known findings can be recorded in `baseline.txt` during rollout. `just via-check-strict` still prints those findings, but fails only on new unbaselined findings or scanner errors.
- Every active rule should have:
  - Linked entry in the harness (`IDENTITIES.tsv` / `RISK_REGISTER.md`)
  - Fixture coverage (good + bad examples)
  - False-positive review

**Lifecycle**:
candidate → fixture-tested → repo sweep + false-positive review → active (advisory or strict)

## Fixtures

Good and bad examples live under `fixtures/`:

```text
fixtures/
  reorg/
    zip_bad.rs     # positional zip after unordered fetch on height data
    zip_good.rs    # explicit height-based re-association (HashMap or equivalent)
```

When adding or changing a rule, add or update the corresponding fixtures.

## Scoping

Intended glob restrictions are recorded in `sgconfig.yml` under the `rules:` section and mirrored by the checker script.

The checker script currently passes the same scope through `ast-grep scan --globs` because ast-grep 0.42 did not reliably enforce rule-level globs in this repository setup.

Example (from current `sgconfig.yml`):

```yaml
rules:
  - include: "via-reorg-buffer-unordered"
    glob:
      - "core/node/via_main_node_reorg_detector/**/*.rs"
      - "via_verifier/node/via_reorg_detector/**/*.rs"

  - include: "via-reorg-height-association-required"
    glob:
      - "core/node/via_main_node_reorg_detector/**/*.rs"
      - "via_verifier/node/via_reorg_detector/**/*.rs"
```

**Guideline for new rules:**

- Prefer recording intended glob/ignore scoping in `sgconfig.yml` when the rule targets a specific Via module or crate.
- Keep the checker script's explicit `--globs` list in sync with `sgconfig.yml`.
- Avoid adding `glob` blocks to individual rule files while ast-grep 0.42 remains the constraint; use `sgconfig.yml` as the canonical scope source.

## Adding New Rules

1. Add a new `.yml` file in `rules/`.
2. Define scoping in `sgconfig.yml` under the `rules:` section and mirror that scope in the checker script's `--globs` list. See the **Scoping** section above.
3. Add good/bad fixtures under `fixtures/`.
4. Update this README.
5. Link the rule to the relevant harness artifacts (`IDENTITIES.tsv`, component card, RISK_REGISTER entry).
6. Decide initial mode (advisory vs strict).

## Inscriber / Pending-Chain Patterns (Future Work)

The BTC inscriber maintains an ordered `fifo_queue` of pending `InscriptionRequest` objects. Spendability of change outputs depends on position in that queue (only the head's reveal change output is treated as immediately spendable).

This creates a second class of ordering/identity risk (`pending_chain_utxo_outpoint`, `fifo_queue_head_change_outpoint`).

No syntactic ast-grep rule has been added yet (the pattern is mostly data-structure + HashMap filtering), but the area is documented in:

- `IDENTITIES.tsv` (new rows added after inscriber review)
- Future BTC sender component card

When a clear, high-signal syntactic smell appears, a rule can be added here.

## References

- Via Agent Harness (primary source of truth for identities and risks)
- AGENTS.md (Via Structural Rules section)
- just via-check / via-check-strict

---

*Maintained as part of the Via Agent Harness cartography effort.*
