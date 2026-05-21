# AGENTS.md — core/lib/

This directory (and its sub-crates) is the preferred home for logic that should be shared between main-node and verifier.

Before duplicating detector, poller, watcher, client, or similar logic in `core/node/` or `via_verifier/node/`, check whether it belongs here.

See root `AGENTS.md` → *Reuse and duplication discipline*.
