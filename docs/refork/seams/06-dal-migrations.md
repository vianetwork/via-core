# Seam 06 — DAL & migrations

## What via patched in v24

- **7 via migrations interleaved in `core/lib/dal/migrations/`** (inscription requests,
  `via_data_availability`, `via_votes`, indexer metadata, wallet migration,
  `via_l1_blocks`, `via_da_chunk_blob`) — timestamps interleave with upstream's, which
  is exactly what makes a merge impossible.
- ~2,649 added LOC of via DAL modules inside `core/lib/dal` (via queries/models) plus
  28 real B/feature modifications to upstream DAL files and 262 generated `.sqlx`
  entries.
- Separate DALs that are clean crates already: `via_verifier/lib/verifier_dal`
  (10 migrations, own database) and `via_indexer/lib/via_indexer_dal`.

## The v29 extension point

- `core/lib/dal` remains the main-node DAL; upstream squashed old migrations and added
  new ones through `20260519…` (airbender SNARK, unsealed-batch index). Via's deleted
  migration files in the diff are upstream's own squash — no action.
- `zksync_db_connection` patterns (instrumented queries, `.sqlx` offline data)
  unchanged in spirit.

## Port approach — regenesis makes this clean

1. **Renumber all via migrations after the pin's latest migration** (>
   `20260519000000`), squashed into a small ordered set (one per subsystem:
   btc_sender inscriptions, DA, votes/bridge, wallets, l1 blocks/indexer). No upstream
   migration history merging; the wallet-migration backfill (`20250731…`) becomes part
   of the initial schema, not a migration.
2. Port via DAL modules as new files in `core/lib/dal/src/` (they are additions, not
   modifications); rerun `cargo sqlx prepare` to regenerate `.sqlx` (the 262 generated
   rows in the inventory are not ported by hand).
3. Re-derive the 28 modified upstream DAL files against v29 — most touch
   eth_sender-adjacent queries; check each against the parallel-stack decision
   (seam 02): if via_btc_sender has its own tables, the upstream file may not need
   modification at all in v29.
4. `verifier_dal` and `via_indexer_dal` port as whole crates (they share only
   `zksync_db_connection`/`zksync_basic_types`); renumber their migrations too for a
   clean genesis.

## Verification hook

Schema diff between old-chain and new-chain databases is a standing differential eval:
every `via_*` table/column present on the reference testnet must exist (or have a
recorded successor) in the new fork's schema.
