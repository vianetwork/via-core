# Seam 03 — Config system

## What via patched in v24

- 12 via config structs in `core/lib/config/src/configs/`: `via_btc_client`,
  `via_btc_sender`, `via_btc_watch`, `via_bridge`, `via_celestia`, `via_consensus`,
  `via_general`, `via_l1_indexer`, `via_reorg_detector`, `via_secrets`, `via_verifier`,
  `via_wallets`.
- Matching deserialization plumbing spread over three crates: `core/lib/env_config`
  (6 modified files), `core/lib/protobuf_config` (14 modified files + protos),
  and `zksync_core_leftovers` temp-config assembly.

## The v29 extension point

- **`core/lib/env_config` and `core/lib/protobuf_config` no longer exist** (0 files in
  the pin). The entire env/proto duplication is gone.
- Config structs in `core/lib/config/src/configs/*` derive
  `smart_config::{DescribeConfig, DeserializeConfig}` directly (verified:
  `configs/da_dispatcher.rs`). The crate exposes `full_config_schema()` and
  `ConfigRepository` + `sources` for layered file/env loading, and secrets live in
  `configs/secrets.rs`.

## Port approach

1. Re-express each via config struct as a `DescribeConfig`/`DeserializeConfig` derive in
   `core/lib/config/src/configs/via_*.rs`, register it in `full_config_schema`, and add
   via secrets (BTC RPC auth, Celestia auth, wallet keys) to the v29 secrets config.
   The ~20 via-patched env_config/protobuf_config files are **dropped entirely** — this
   seam shrinks in the port.
2. `via_wallets` and `via_secrets` should be mapped onto v29's wallets/secrets
   equivalents rather than ported as parallel files — flag for the interactive pass.
3. The file-based config tree under `etc/env`/`configs/` changed shape upstream
   (837 B/wiring rows are dominated by config plumbing); regenerate via's example
   configs from the new schema (`smart-config-commands` powers a CLI for this behind
   the `cli` feature) instead of porting old YAML/env files.

## Sequencing

This unit must land in **wave 1** alongside `via_btc_client`, since every via crate
reads its config struct. It is mechanical and an ideal early Smithers unit after the
pilot.
