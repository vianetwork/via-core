# Seam 02 — Settlement layer / eth_sender → btc_sender

## What via patched in v24

- `core/node/via_btc_sender` is a fork of `eth_sender` with the largest real delta of
  the forked crates: **25 files, +2,355/−4,116**. Structure mirrors eth_sender:
  `aggregator.rs`, `btc_inscription_aggregator.rs`, `btc_inscription_manager.rs`,
  `publish_criterion.rs`, `aggregated_operations.rs` — commit/proof operations become
  Bitcoin inscriptions built via `via_btc_client`.
- Persistence in via-added DAL tables (`via_btc_inscriptions_request*`, migration
  `20240906134623`), not `eth_txs`.
- 9 B/feature modifications to upstream `eth_sender` itself (mostly disabling/wiring).

## The v29 extension point

- v29 introduces a settlement abstraction, but it is **ETH-shaped, not pluggable**:
  `core/lib/basic_types/src/settlement.rs` defines
  `enum SettlementLayer { L1(SLChainId), Gateway(SLChainId) }` ("L1 or the gateway",
  both Ethereum-compatible), plus `WorkingSettlementLayer` for migration windows and a
  `core/lib/settlement_layer_data` crate. `web3_decl::node::resources` exposes a
  `SettlementLayerClient` resource.
- `eth_sender` still exists as a crate with its own `node/` layers
  (`core/node/eth_sender/src/node/{aggregator,manager}.rs`).

## Decision

**Do not try to add a Bitcoin variant to `SettlementLayer` during the port.** The
abstraction assumes an EVM JSON-RPC settlement target throughout
(`SettlementLayerClient`, gas semantics, `is_gateway()` call sites). Via stays what it
is in v24: a *parallel* sender stack (`via_btc_sender` beside an unused `eth_sender`),
which the inventory shows is exactly how via-core is already built — nothing was
deleted.

Port approach:

1. Extract the via delta (`git diff f37b84ac75:core/node/eth_sender
   HEAD:core/node/via_btc_sender`) and re-apply onto v29's `eth_sender` shape,
   including moving the wiring layers into `via_btc_sender/src/node/`.
2. Audit how the v29 node boots when settlement-related layers are simply not wired:
   enumerate `SettlementLayer`/`SettlementLayerClient` consumers
   (`git grep -l 'SettlementLayer' core-v29.20.0 -- core/`) and record, per consumer,
   whether via omits the layer (preferred) or feeds it a static
   `SettlementLayer::L1(via_chain_id)` placeholder. This audit is the first Smithers
   task of the unit and a deliverable in itself.
3. The verifier-side senders (`via_verifier/node/via_btc_sender`, MuSig2 withdrawal
   broadcasting) depend on this unit plus `via_musig2`; they port in wave 4.

## Risks

- v29 `eth_sender` was substantially refactored (aggregator/manager split changed,
  `FinalityResponse`-style flows); expect the re-applied delta to need real rework —
  this is the unit where the interactive 20% lives, after the state keeper.
- Gateway-migration machinery (`gateway_migrator`, `server_notification`) is new since
  v24 and must be confirmed inert when unwired.
