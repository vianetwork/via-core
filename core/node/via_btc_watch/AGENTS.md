# AGENTS.md — main-node btc watch

This directory is **sibling-paired** with `via_verifier/node/via_btc_watch/`.

Before pushing changes to this directory:

1. Inspect `via_verifier/node/via_btc_watch/` for parallel behavior.
2. If logic is shared, extract to `core/lib/` rather than duplicating.
3. Run `just via-check-strict` and ensure it passes.

See root `AGENTS.md` → *Reuse and duplication discipline* for the rationale and mandatory checks.
