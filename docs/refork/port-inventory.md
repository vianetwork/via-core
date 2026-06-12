# Refork port inventory

Status: Phase 0 deliverable. Generated against fork point `f37b84ac75` (core-v24.22.0,
2024-08-28) and refork pin **`core-v29.20.0`** (2026-06-04).

Machine-readable companion: [`etc/refork/inventory.csv`](../../etc/refork/inventory.csv),
regenerable with [`etc/refork/build_inventory.py`](../../etc/refork/build_inventory.py)
after `git remote add upstream https://github.com/matter-labs/zksync-era.git &&
git fetch upstream tag core-v29.20.0 --no-tags`.

## Headline numbers

`git diff --name-status -M f37b84ac75..HEAD` → **3,457 diverged paths**
(+257,706/−92,520 lines). Upstream moved +326,693/−167,579 over the same window.

| Category | Subclass | Paths | Meaning |
|---|---|---:|---|
| A | via-crate | 477 | Files inside the 39 standalone via crates (~97k added LOC total with embedded files) |
| A | via-named-embedded | 112 | Via-named files added *inside* upstream crates (node_framework layers, config structs, DAL modules) |
| A | unmarked-addition | 593 | Files via added without a `via` name — **120 are byte-identical to v29** (backports); rest need triage |
| A | generated | 262 | Added `.sqlx` query metadata — regenerates from queries |
| A | backported-migration | 32 | Upstream migrations via cherry-picked after the fork — already in the v29 schema, zero port work |
| B | feature | 583 | Hand-modified upstream files. **110 byte-identical to v29** (pure backports) → 473 real |
| B | wiring | 837 | Small/mechanical edits (Cargo.toml, mod exports, configs) — 306 identical to v29 |
| B | generated / noise / submodule | 151 | Lockfiles, `.sqlx`, CI templates, `contracts` submodule pointer |
| C | deleted-upstream-file | 371 | See below — almost all are backported *upstream* deletions, not via surgery |
| D | via-migration | 39 | Via-owned SQL migrations (see seam doc 06) |

### The `v29_fate` column

Every B/C row (and unmarked additions) is joined against the pin's tree:

- **identical-to-v29** — via's final content equals upstream v29 byte-for-byte. The diff
  was a backport of upstream work (via cherry-picked upstream changes after the fork:
  zk_toolbox→zkstack_cli, multivm versions, prover updates). **Zero port work.**
- **present** — file exists at the same path in v29 with different content. Re-derive the
  via delta against the v29 version.
- **moved** — the file's pre-fork content exists byte-for-byte at another v29 path
  (path recorded). Confirmed code reorganization; re-derive against the new location.
- **moved?** — no content match, but the basename is unique in v29 (weak hint,
  candidate recorded). Confirm before trusting.
- **gone** — no counterpart in v29. 127 B/feature files; each needs an explicit
  decision (usually the surrounding subsystem was redesigned). Ambiguous basename
  matches are deliberately left here rather than guessed.

## Finding 1: via never deleted the ETH stack

Of 371 deletions, only **12** are files v29 still has — and they are CI workflow files,
two test helpers, and a Dockerfile. Everything else via "deleted" was deleted upstream
too (old multivm versions, squashed migrations, zk_toolbox). `eth_sender`, `eth_watch`
and friends are still workspace members in via-core today; via added parallel `via_*`
components instead of removing ETH ones.

**Consequence: the refork is additive.** No surgical removal pass over upstream code is
needed. The new fork keeps upstream intact and adds the via surface beside it, exactly
as via-core already does.

## Finding 2: forked crates carry small deltas

Most via node crates are copies of an upstream crate (ETH paths stripped) plus a via
delta. Measured against their v24 base:

| Via crate | Upstream base (v24) | Via delta | Strategy |
|---|---|---|---|
| `core/node/via_state_keeper` | `state_keeper` | 41 files +1,767/−1,938 | re-apply delta on v29 crate (upstream moved ±11k there) |
| `core/node/via_btc_sender` | `eth_sender` | 25 files +2,355/−4,116 | largest real delta; see seam 02 |
| `core/node/via_btc_watch` | `eth_watch` | 19 files +1,217/−1,500 | re-apply on v29 `eth_watch` shape |
| `core/lib/via_consensus` | `consensus` | 17 files +8/−3,980 | nearly pure strip-down — re-strip v29 |
| `core/node/via_fee_model` | `fee_model` | 10 files +141/−1,298 | small; see seam 04 |
| `core/node/via_consistency_checker` | `consistency_checker` | 7 files +71/−1,155 | small |
| `core/node/via_main_node_reorg_detector` | `reorg_detector` | 7 files +285/−1,191 | small |
| `core/node/via_node_storage_init` | `node_storage_init` | 12 files +174/−493 | small |
| `core/lib/via_da_dispatcher` | `da_dispatcher` | 6 files +65/−287 | small |
| `core/lib/via_mempool` | `mempool` | 4 files +140/−73 | small |
| `core/node/via_block_reverter` | `block_reverter` | 4 files +228/−224 | small |
| `core/bin/via_server` | `zksync_server` | 4 files +398/−618 | regenerate against v29 `node_builder.rs` |
| `core/bin/via_external_node` | `external_node` | 9 files +378/−238 | small |
| `core/tests/via_loadnext` | `loadnext` | 22 files +780/−1,056 | port with tests phase |

For each row the port unit is **extract the delta** (`git diff
f37b84ac75:<base> HEAD:<via crate>`) **and re-apply it to the v29 version of the base
crate** — not "copy the via crate and fix compile errors". The deltas above are the true
size of the work.

Genuinely novel code (no upstream base; lift-and-adapt):

| Crate | Added LOC | Notes |
|---|---:|---|
| `core/lib/via_btc_client` | 9,387 | inscriptions, Bitcoin RPC; depends only on `zksync_types/config/basic_types/object_store` → **pilot unit** |
| `via_verifier/lib/via_verification` | 7,086 | proof verification |
| `via_verifier/lib/via_musig2` | 5,587 | MuSig2 sessions |
| `via_verifier/node/via_verifier_coordinator` | 2,415 | coordinator API |
| `via_verifier/lib/verifier_dal` | 2,903 | + 10 migrations |
| `via_indexer/*` | ~1,900 | indexer bin/dal/storage-init |
| `core/tests/via-protocol-upgrade` + `via-playground` | ~11,400 | tooling; port late |

## Dependency waves (porting order)

From `Cargo.toml` graphs of the 39 via crates (via→via edges only; all also depend on
`zksync_*` crates, which the refork provides):

1. **Wave 1**: `via_btc_client`, `via_da_clients`, `via_da_client`, `via_mempool`, `via_reorg`, `via_consensus`, `via_block_reverter`, `via_da_dispatcher_lib`, `via_indexer_dal`
2. **Wave 2**: `via_fee_model`, `via_consistency_checker`, `via_da_dispatcher`, `via_node_storage_init`, `via_main_node_reorg_detector`, `via_verification`, `via_verifier_types`, `via_test_utils`, `via_server`*, `via_loadnext`, …
3. **Wave 3**: `via_state_keeper`, `via_btc_watch`, `via_btc_sender`†, `via_external_node`, `via_verifier_dal`, `via_withdrawal_client`, `via_indexer`
4. **Wave 4**: `via_musig2`, `via_verifier_btc_sender`, `via_verifier_btc_watch`, `via_verifier_reorg_detector`, `via_verifier_state`, `via_verifier_storage_init`
5. **Wave 5**: `via_verifier_coordinator`, `via_zk_verifier`

\* `via_server` appears early only because via's wiring layers live inside
`zksync_node_framework` in v24; in v29 layers move into the via crates themselves
(seam 01), so the server binary actually lands **last** in each milestone.
† `via_btc_sender` is wave 3 by deps but is the deepest integration; schedule after
state keeper.

## Remaining triage debt (manual pass)

- **473 B/feature rows** not identical to v29: biggest units `multivm` (63 — verify
  these are partial backports), `zkstack_cli` (40), `dal` (29), `prover` (28),
  `api_server` (25), `node_framework` (21 — superseded by seam 01),
  `state_keeper` (21), `types` (17).
- **473 unmarked additions** not identical to v29: `zkstack_cli` (104), `prover` (58),
  `config+docker` (41), `multivm` (37), `ci` (33), `infra:via` (31). Expect most
  `zkstack_cli`/`prover`/`multivm` rows to be partial backports; `infra:via` and
  `ci` are via-authored.
- The Smithers pipeline should treat each unit's B-rows + embedded A-rows + the crate
  delta as one work item with this CSV as the queue.
