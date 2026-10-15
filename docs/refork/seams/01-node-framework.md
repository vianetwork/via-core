# Seam 01 — Node framework composition

**Priority: highest.** This seam shapes every other port unit.

## What via patched in v24

- `core/node/node_framework` was a single crate containing the framework *and* all
  layer implementations. Via added **31 layer files** under
  `src/implementations/layers/via_*` (btc_sender aggregator/manager/vote/vote_manager,
  verifier coordinator_api/verifier, btc_client, btc_watch, da_dispatcher, gas_adjuster,
  state_keeper set, storage init, reorg detectors, indexer, zk verification, …) plus
  ~20 modified framework files.
- `core/bin/via_server/src/node_builder.rs` (and `via_external_node`) compose those
  layers; the binaries depend on `zksync_node_framework`, which is why the Cargo
  dependency graph makes `via_server` look shallow.

## The v29 extension point

- The framework moved to **`core/lib/node_framework`** and is now framework-only:
  `WiringLayer`, `Task`, `Resource`, `ZkStackService(Builder)`, `FromContext`/
  `IntoContext` derives. No `implementations/` directory exists anywhere.
- Every component crate owns its layers in a **`src/node/` module**, usually behind a
  `node_framework` cargo feature. Verified examples in the pin:
  `core/node/eth_sender/src/node/{aggregator,manager}.rs`,
  `core/lib/da_client/src/node/` (`pub mod node` behind `#[cfg(feature = "node_framework")]`),
  `core/lib/web3_decl/src/node/resources.rs` (shared client resources, e.g.
  `SettlementLayerClient`).
- `core/bin/zksync_server/src/node_builder.rs` still exists and composes layers; same
  for `core/bin/external_node/src/node_builder.rs`.

## Port approach

1. Each ported via crate ships its own `src/node/` module containing the layer(s) that
   today live in `node_framework/src/implementations/layers/via_*`. The layer code
   itself moves nearly verbatim; only imports change (`zksync_node_framework` no longer
   exports resources — shared resources come from `web3_decl::node` / the owning crate).
2. Shared via resources (Bitcoin RPC client from `via_btc_client`, verifier DAL pools)
   become `Resource` impls in their owning crate's `node` module, mirroring how
   `web3_decl::node::resources` exposes ETH clients.
3. `via_server/src/node_builder.rs` is **regenerated from v29's
   `zksync_server/src/node_builder.rs`** (delta vs v24 was only +398/−618), swapping
   ETH layer additions for via ones. Same for `via_external_node`.
4. The ~20 via modifications to framework internals must be re-justified one by one;
   most were needed because layers lived in-crate. Expect nearly all to disappear.

## Consequence for sequencing

`zksync_node_framework` mediation is gone, so the true dependency order is the crate
waves in the inventory; the server binaries integrate last per milestone and serve as
each milestone's smoke test (`cargo run --bin via_server` against regtest).
