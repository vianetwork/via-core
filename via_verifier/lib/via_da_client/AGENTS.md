# AGENTS.md — via_da_client (verifier)

This directory is **sibling-paired** with `core/lib/via_da_clients/`.

Before pushing changes to this directory:

1. Inspect `core/lib/via_da_clients/` for parallel behavior.
2. If logic is shared, extract to `core/lib/` rather than duplicating.
3. Run `just via-check-strict` and ensure it passes.

See root `AGENTS.md` → *Reuse and duplication discipline* for the rationale and mandatory checks.

Note: Naming differs between sides (via_da_client vs via_da_clients). Keep this in mind when searching.
