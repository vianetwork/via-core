# AGENTS.md — main-node reorg detector

This directory is **sibling-paired** with `via_verifier/node/via_reorg_detector/`.

Before pushing changes to this directory:

1. Inspect `via_verifier/node/via_reorg_detector/` for parallel behavior.
2. If logic is shared, extract to `core/lib/` rather than duplicating.
3. Run `just via-check-strict` and ensure it passes.

See root `AGENTS.md` → *Reuse and duplication discipline* and *Source comment discipline* for the rationale and mandatory checks.
