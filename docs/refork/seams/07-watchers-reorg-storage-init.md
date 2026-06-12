# Seam 07 — Watchers, reorg handling, storage init (grouped small units)

These crates share one shape: small via delta over an upstream base whose v29 trait
seams still exist. They are the bread-and-butter Smithers units.

| Unit | v24 base | Via delta | v29 anchor point |
|---|---|---|---|
| `via_btc_watch` | `eth_watch` | 19 files +1,217/−1,500 | `eth_watch` exists in pin with `node/` layers; via watches Bitcoin blocks/inscriptions instead of ETH events; `via_l1_blocks` table (migration `20250905…`) |
| `via_main_node_reorg_detector` + `via_reorg` | `reorg_detector` | 7 files +285/−1,191 | `core/node/reorg_detector` in pin; via adds Bitcoin-reorg semantics (`via_reorg` lib, docs/via_guides/reorg.md) |
| `via_consistency_checker` | `consistency_checker` | 7 files +71/−1,155 | crate exists in pin; via checks commitments against inscriptions rather than L1 contracts |
| `via_node_storage_init` | `node_storage_init` | 12 files +174/−493 | pin keeps `pub trait InitializeStorage` / `pub trait RevertStorage` (`core/node/node_storage_init/src/traits.rs:8,23`) — via implements them, no fork needed |
| `via_block_reverter` (+ CLI bin) | `block_reverter` | 4 files +228/−224 | crate exists in pin |
| `via_consensus` | `consensus` | 17 files **+8**/−3,980 | near-pure strip-down; decide whether the new fork needs it at all or can run upstream consensus disabled — flag for interactive review |
| `via_mempool` | `mempool` | 4 files +140/−73 | crate exists in pin |

Verifier-network counterparts (`via_verifier/node/via_btc_watch`, `…_reorg_detector`,
`…_storage_init`, `…_block_reverter`) re-use the same deltas against the verifier DAL;
port them in wave 4 by repeating each unit's recipe.

## Port recipe (per unit)

1. `git diff f37b84ac75:<base> HEAD:<via crate>` → the true delta (recorded per-unit in
   the inventory CSV).
2. Read the v29 base crate; decide thin-crate-over-upstream (preferred — confirmed
   possible for storage init via public traits) vs fork-with-delta.
3. Re-apply, move wiring layers into `src/node/` (seam 01), wire into
   `via_server/node_builder.rs`.
4. Unit-level differential eval: watcher units replay recorded Bitcoin regtest
   blocks/inscriptions and must produce identical DB effects (rows in
   `via_l1_blocks`, processed-priority-ops, votes) as the reference node.
