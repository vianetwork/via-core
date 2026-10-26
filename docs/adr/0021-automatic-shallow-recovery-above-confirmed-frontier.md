---
status: accepted
date: 2026-10-24
---

# Recover shallow suffixes above the confirmed frontier and stop otherwise

This supersedes [ADR 0014](0014-repair-only-reversible-provable-steps-and-inspect-read-only.md) and the independent 100-block search limit in ADRs 0008/0009. Let d be the number of old-branch blocks disconnected to the proved common block. Recover automatically only when d < N and the common block is at or above the durable confirmed frontier, preserving its hash. N is the same network policy as maturity: 6 mainnet, 100 testnet4, 100 signet, 1 isolated regtest.

Persist the frontier as height and hash under the same exclusion used by authority admission and rollback. Advance it monotonically from checked canonical mature history; never lower it on a shrink or reset. Reconcile and protect already executed/signed/published legacy origins too. Startup validates the frontier and replay readiness before authority tasks run. The frontier protects earlier effects; it does not replace current canonicality/maturity checks or completed proof evidence.

This durable boundary is necessary even with N-deep action gates. At N=100, block 901 can justify an effect at tip 1000. Rewinds to 950 and then 900 each remove only 50 blocks. The second must stop because it crosses the saved frontier. Restart between them must preserve that result. Hyperlane's persisted confirmed_height check supplies this invariant, adapted to Via's inclusive formula, full headers and source policy.

At the Nth old removal, a frontier crossing, missing ancestry, source-identity conflict or unreconciled retained irreversible fact, stop the whole node before deleting the suffix. Persist the existing authority-disabled signal and report network, N, heads, frontier and last proved identity. Transient/inconclusive RPC failures hold fresh authority and retry without destructive guesses. The error must escape the reader loop to node supervision; logging and retrying a fatal error is insufficient.

Deep recovery currently means stop and diagnose. No automatic L2/tree/snapshot rollback, recovery class engine, inspection framework or stopped-node repair command is selected. The existing reverter is useful infrastructure, not a proven repair procedure for both roles. A separate design and rehearsal must establish that procedure. Local signing risk and in-flight history remain intact. This decision authorizes no live repair.
