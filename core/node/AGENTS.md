# AGENTS.md — core/node/

This tree owns main-node runtime wiring and execution concerns. Shared main-node/verifier behavior belongs in
`core/lib/` or the appropriate shared crate, not in either node tree.

Before adding behavior, inspect related implementations in `via_verifier/node/` and apply root `AGENTS.md` → _Reuse and
duplication discipline_. Use `.github/sibling-paths.yml` for known pairs. Preserve genuine execution-context differences
rather than forcing speculative extraction.

Record reuse evidence when preparing the PR, as required by `.github/pull_request_template.md`.
