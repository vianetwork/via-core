# AGENTS.md — via_reorg

This crate hosts the **single source of truth** for L1 reorg comparison logic
used by both the main-node detector and the verifier detector.

Before editing:

1. Any new helper must be pure, dependency-free, and testable in isolation.
2. Changes here affect two sibling detectors; keep call sites thin.
3. Apply the *Source comment discipline* rules from the root AGENTS.md with extra strictness — this crate is read by future LLMs as the canonical example of "durable invariant" documentation.

See root `AGENTS.md` → *Reuse and duplication discipline* and *Source comment discipline*.
