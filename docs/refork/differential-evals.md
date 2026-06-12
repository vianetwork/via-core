# Differential eval rig (spec)

The frozen Via testnet (and a local devnet built from current via-core `main`) is the
**reference oracle**. Parity is proven by replaying identical operations against the
old node and the new fork and comparing observable effects. Encoded as Smithers
scorers so `smithers eval` emits a parity regression report per unit and per milestone.

## Topology

```
                      ┌── reference: via-core main devnet (btc regtest A + celestia-mock)
 recorded op trace ──►│
 (deposits, L2 txs,   └── candidate: new fork devnet     (btc regtest B + celestia-mock)
  withdrawals)              │
                            ▼
              comparators (scorers) over both nodes' outputs
```

Both stacks run from the same docker-compose pattern via-core already has
(`docker-compose-via.yml`: bitcoind regtest + celestia light node/mock + postgres).
The op trace is deterministic and versioned: a fixed seed wallet set, deposit
inscriptions, a mix of L2 transfers/deploys/calls, withdrawal requests.

## Comparators

| Scorer | Compares | Mode |
|---|---|---|
| `batch_commitment` | L1 batch commitment fields per batch number | exact, after normalizing fields regenesis legitimately changes (protocol version, genesis root, chain id) |
| `da_payload` | pubdata blob bytes handed to the DA client per batch | semantic: decode → compare state diffs; byte-exact only if seam-05 keeps via's encoding |
| `inscriptions` | inscription payloads btc_sender writes per batch/proof | semantic per field; normalized for key/chain differences |
| `fees` | fee model inputs + per-tx receipts (gas used, effective price) on the trace | exact within declared tolerance (BTC feerate inputs pinned in regtest) |
| `api_surface` | JSON-RPC responses (`eth_*`, `zks_*`, via-specific) for a fixed query corpus | structural diff with an allowlist of fields upstream changed v24→v29; allowlist is itself a reviewed artifact |
| `db_schema` | `via_*` tables/columns vs reference schema | every reference table present or mapped to a recorded successor |
| `watcher_effects` | DB rows produced from a recorded regtest block stream (deposits, votes, l1 blocks) | exact |

## Normalization is the hard part

Regenesis + v29 means byte-equality is wrong for most comparators. Each scorer
declares an explicit normalization (what may differ and why); anything outside it is a
failure. The normalization lists start empty and only grow through reviewed commits —
that's where parity is actually defined.

## Cadence

- **Per unit**: only the scorers the unit touches (seam 04 → `fees`,
  `batch_commitment`; seam 05 → `da_payload`; seam 07 → `watcher_effects`).
- **Per milestone** (server boots end-to-end): full trace replay, all scorers.
- **Phase-exit**: full replay green twice from clean genesis (determinism check).
