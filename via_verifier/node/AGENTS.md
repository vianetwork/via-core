# AGENTS.md — via_verifier/node/

This tree owns verifier runtime wiring and execution concerns. Shared main-node/verifier behavior belongs in `core/lib/` or the appropriate shared crate, not in either node tree.

Before adding behavior, inspect related implementations in `core/node/` and apply root `AGENTS.md` → *Reuse and duplication discipline*. Use `.github/sibling-paths.yml` for known pairs. Preserve genuine execution-context differences rather than forcing speculative extraction.

Record reuse evidence when preparing the PR, as required by `.github/pull_request_template.md`.
