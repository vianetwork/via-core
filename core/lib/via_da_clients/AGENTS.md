# AGENTS.md — via_da_clients (core)

This directory is **sibling-paired** with `via_verifier/lib/via_da_client/`.

Before pushing changes to this directory:

1. Inspect `via_verifier/lib/via_da_client/` for parallel behavior.
2. If logic is shared, extract to `core/lib/` (or keep here if this is the right shared home) rather than duplicating.
3. Run `just via-check-strict` and ensure it passes.

See root `AGENTS.md` → *Reuse and duplication discipline* for the rationale and mandatory checks.

Note: Naming differs between sides (via_da_clients vs via_da_client). Keep this in mind when searching.
