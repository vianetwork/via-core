# Refork: via-core on zksync-era core-v29.19.2 — Phase 0

via-core forked zksync-era at `f37b84ac75` (core-v24.22.0, 2024-08-28) and is ~21
months behind; merging upstream is no longer feasible. Decision: **refork** latest
zksync-era and re-port the via surface, with **regenesis** for the new chain and the
current testnet frozen as a **reference oracle** for differential testing.

This directory is the Phase 0 deliverable set — the inventory, the architecture
mapping, and the orchestration specs that the port executes against.

## Contents

| Doc | What it answers |
|---|---|
| [port-inventory.md](port-inventory.md) | What diverged, categorized; what work each diverged path implies against the pin (`core-v29.19.2`); porting order. Data: [`etc/refork/inventory.csv`](../../etc/refork/inventory.csv) |
| [seams/01-node-framework.md](seams/01-node-framework.md) | Where via wiring lives in v29 (framework split into per-crate `node/` modules) |
| [seams/02-settlement-btc-sender.md](seams/02-settlement-btc-sender.md) | v29 settlement abstraction is ETH-shaped; via stays a parallel stack |
| [seams/03-config.md](seams/03-config.md) | env/protobuf config is gone; via configs become smart-config derives |
| [seams/04-state-keeper-fee-model.md](seams/04-state-keeper-fee-model.md) | `StateKeeperIO`/`BatchFeeModelInputProvider` seams survive; re-apply deltas, don't re-fork |
| [seams/05-da.md](seams/05-da.md) | v29 ships a native Celestia client; trait gained `ensure_finality` |
| [seams/06-dal-migrations.md](seams/06-dal-migrations.md) | Regenesis migration strategy: renumber after upstream's latest |
| [seams/07-watchers-reorg-storage-init.md](seams/07-watchers-reorg-storage-init.md) | The small-delta units and their per-unit recipe |
| [smithers-workflow.md](smithers-workflow.md) | The port-unit pipeline spec (worktree agent → build/test backpressure → diff eval → approval gate) |
| [differential-evals.md](differential-evals.md) | Parity scorers vs the reference node |

## Headline findings

1. **The refork is additive.** Via never deleted the ETH stack — of 371 deleted files,
   only 12 still exist in v29 (CI/docker noise). No surgical-removal pass is needed.
2. **A large share of the diff is backports.** 591 diverged paths are byte-identical
   to v29 (via cherry-picked upstream work after the fork): zero port cost.
3. **Forked via crates carry small deltas** (e.g. `via_state_keeper` is ±1.8k over its
   base while upstream moved that crate ±11k). The port re-applies deltas onto v29
   crates instead of carrying forks.
4. **Two seams shrink outright in v29**: config (env/protobuf plumbing deleted
   upstream) and DA (native Celestia client upstream).

## Next steps (local, not in this repo)

1. Fork `matter-labs/zksync-era` at tag `core-v29.19.2`; build and run the unmodified
   devnet as the clean baseline. Copy `docs/refork/` + `etc/refork/` into the fork.
2. Index both repos in GitNexus (`npx gitnexus analyze`) and use cross-repo queries to
   validate/refine the seam docs during the interactive spike.
3. `smithers init` from [smithers-workflow.md](smithers-workflow.md); seed cross-run
   memory with the seam conclusions; run the pilot unit `via_btc_client` end-to-end.

## Reproducing the inventory

```bash
git remote add upstream https://github.com/matter-labs/zksync-era.git
git fetch upstream tag core-v29.19.2 --no-tags
python3 etc/refork/build_inventory.py
```
