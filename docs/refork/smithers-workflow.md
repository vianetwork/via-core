# Smithers port-unit workflow (spec)

Spec for the orchestration that executes the mechanical bulk of the port. Authored in
Phase 0; instantiated locally with `smithers init` in the new fork repo.

## Work queue

`etc/refork/inventory.csv`, grouped by `port_unit`, ordered by the dependency waves in
`port-inventory.md`. A unit's work item bundles:

- its Category A rows (crate files / embedded additions),
- its B/feature + B/wiring rows not marked `identical-to-v29`,
- its slice of seam doc(s) — the unit prompt links the relevant
  `docs/refork/seams/*.md` section,
- the extracted v24 delta for forked crates
  (`git diff f37b84ac75:<base> HEAD:<via>` captured as a patch artifact).

## Pipeline per unit

```
pick unit ──► agent in worktree ──► cargo build -p <crate> ──► cargo test -p <crate>
                   │ (port step:            │ backpressure: failures loop
                   │  re-apply delta        │ back to the agent, max N iters
                   │  per seam doc)         ▼
                   │                differential eval vs reference node
                   │                (skipped for units with no runtime
                   │                 surface; required for seam-04/05/07 units)
                   ▼                        │
            unit notes written              ▼
            to cross-run memory ◄── smithers approve  ──► merge to integration branch
                                    (human gate: diff + eval report)
```

- **Port step**: the agent re-applies the unit's delta onto the v29 code per the seam
  doc, in its own worktree. It must not touch files owned by other units (the CSV's
  `port_unit` column is the ownership map); cross-unit needs are recorded as blockers
  in memory instead.
- **Backpressure**: `cargo build`/`cargo test` scoped to the crate, then workspace
  `cargo check`. A unit that can't go green in N iterations is parked for interactive
  work, not force-merged.
- **Approval gate**: every unit ends in `smithers approve` with the diff, the build/test
  log, and (where applicable) the differential-eval report attached. No auto-merge.

## Cross-run memory seed

Seeded from Phase 0 before the first unit runs:

- the v24→v29 mapping facts (one entry per seam doc conclusion, e.g. "env_config and
  protobuf_config no longer exist; via configs are smart_config derives registered in
  full_config_schema"),
- the parallel-stack decision (never modify `SettlementLayer`; via components are
  additive),
- the forked-crate strategy table (thin-crate-over-upstream preferred; fork-with-delta
  fallback recorded per unit),
- regenesis rules (latest protocol version only; migrations renumbered after
  `20260519000000`).

Each completed unit appends: surprises, v29 API facts discovered, blockers filed
against later units.

## Pilot

`via_btc_client` (wave 1): only zksync deps are `zksync_types`, `zksync_config`,
`zksync_basic_types`, `zksync_object_store`; no DB, no node wiring beyond its layer.
Exit criterion for the scaffolding: pilot goes port → build → test → eval (its eval is
inscription round-trip vectors against regtest) → approve without manual intervention
in the pipeline itself.
