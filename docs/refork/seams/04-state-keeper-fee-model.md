# Seam 04 — State keeper & fee model

## What via patched in v24

- `core/node/via_state_keeper` is a full fork of `state_keeper` (10.9k LOC), but the
  real via delta is **41 files, +1,767/−1,938** — custom `io/` (mempool IO over
  `via_mempool`), seal criteria, and persistence/output-handler tweaks.
- `core/node/via_fee_model` delta vs `fee_model` is tiny: **+141/−1,298** — mostly
  deleting ETH gas-adjuster paths and feeding BTC-denominated inputs; there is also a
  `via_gas_adjuster` wiring layer and `via_main_node_fee_params_fetcher`.
- 21 B/feature modifications to upstream `state_keeper` remain in via's tree (some are
  backports; the inventory marks which).

## The v29 extension point

- The trait seams via relies on still exist at the same altitude:
  - `pub trait StateKeeperIO` — `core/node/state_keeper/src/io/mod.rs:189`
    (plus `IoSealCriteria` supertrait).
  - `pub trait BatchFeeModelInputProvider` — `core/node/fee_model/src/lib.rs:35`.
- Upstream moved `state_keeper` by ±11k lines since v24 (new VM dispatch through
  `vm_executor`, unsealed-batch handling — see new dal migration
  `…add_l1_batches_unsealed_number_index`), so the forked-crate copy is unsalvageable;
  the delta is not.

## Port approach

1. **Do not re-fork `state_keeper`.** Extract via's delta
   (`git diff f37b84ac75:core/node/state_keeper HEAD:core/node/via_state_keeper`) and
   re-implement it against v29's crate. Target shape: via provides its own
   `StateKeeperIO` implementation (mempool IO) and seal criteria in a thin
   `via_state_keeper` crate that depends on upstream `state_keeper`, instead of a fork.
   If v29's internals make the thin-crate shape impossible (private modules), fall back
   to a fork-with-delta but record every divergence in the unit's notes.
2. `via_fee_model`: implement `BatchFeeModelInputProvider` over BTC fee inputs
   (`via_gas_adjuster` → `via_btc_client` feerate estimation). The −1,298 deletion side
   of the delta disappears (we no longer carry a stripped copy).
3. This unit is the **differential-eval anchor**: batch sealing behavior, fee inputs,
   and pubdata limits must match the reference testnet node on replayed traffic before
   the unit is approved (see differential-evals.md).

## Risks

- v24→v29 protocol-version gap changes batch environs (`vm_executor`, interop roots).
  Regenesis means via does not need old protocol versions — port against the
  **latest protocol version only** and strip via's multi-version compatibility paths.
