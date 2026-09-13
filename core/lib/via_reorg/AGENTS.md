# AGENTS.md — via_reorg

This crate hosts the **single source of truth** for L1 reorg comparison logic
used by both the main-node detector and the verifier detector.

Keep reorg comparison helpers pure, dependency-free, and testable in isolation. Changes affect both sibling detectors; keep their call sites thin.

See root `AGENTS.md` → *Reuse and duplication discipline* and *Source comment discipline*.
