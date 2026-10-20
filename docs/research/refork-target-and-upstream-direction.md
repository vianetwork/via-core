# Refork target and upstream direction

Status: research with one recorded maintainer decision, made on 2026-10-20. The latest zksync-era
`main` is no longer a refork target. ZKsync OS is the likely eventual target. The refork has not
started and waits for the exit conditions below. No implementation, migration, regenesis, or
deployment is approved by this note. Evidence accessed 2026-10-20.

## Question

Via was forked from `matter-labs/zksync-era`. Is zksync-era still the upstream that a future Via
refork should target? If not, what is the likely target, when is a refork worth starting, and what
should current via-core work do in the meantime?

## Source revisions

| Source | Revision |
| --- | --- |
| via-core `main` | `eec4bb8f59fff0c23ddc9cdfa21910882ca36be6` |
| Via fork point in zksync-era | `f37b84ac75de8606382943bb10b8d064c475b5a0`, 2024-08-28 |
| zksync-era `main` | `ff5f519b11cff863edcfa0f75af10fea113806b0`, 2026-08-25 |
| zksync-os `main` | `f91897ce4ff4ede1f8d338411d023645684d0fb7`, 2026-08-19 |
| zksync-os `v0.5.x-private` | `5610fb622832acbdf4feeb0ae458a5c3436b2b6c` |
| zksync-os-server `main` | `e29a2c0a584b7f3f985e9a414bfbe179456c093d`, 2026-08-24 |
| zksync-docs `main` | `14de8d0a8a5470f6d7210f3e7d5f6bc2844b0948` |

Forum posts, blog posts, and the rendered ZKsync docs are moving documents, accessed 2026-10-20.
Sources for the refork-method findings are pinned in their own section.

## Findings

### Via forked Era at core-v24.22.0

Via was forked from zksync-era at `core-v24.22.0`. The merge-base, `f37b84ac75`, is five commits
after the `core-v24.22.0` release commit. That history stays true whatever the next target is.

### Matter Labs is retiring EraVM

Matter Labs announced on 2026-09-04 that EraVM will be retired. The
[EraVM security update](https://www.zksync.io/blog/eravm-security-update) says that within six
months "EraVM chains will begin a transition". It also lists five security measures for EraVM
chains, including publication of Era protocol code three months after each upgrade ships. The
transition does not apply to chains running
[Atlas](https://www.zksync.io/blog/introducing-the-zk-stacks-atlas-upgrade), the ZK Stack upgrade
with a new sequencer and Airbender proving.

The [Matter Labs Q3 2026 deliverables report](https://forum.zknation.io/t/matter-labs-q3-2026-deliverables-report/1172),
posted on 2026-10-12, confirms the direction. EraVM chains "keep receiving security work", and ZKsync
OS is where "all new ZKsync protocol development happens". The
[October 14 review-call summary](https://forum.zknation.io/t/next-proposal-review-call-wednesday-october-14th-2026/1160)
calls ZIP-18 "likely to be the final feature upgrade for EraVM". The checked official sources give
no general procedure for moving an existing Era chain, with its contracts, state, and history, onto
ZKsync OS.

The zksync-era repository matches that state. `main` has had no commits since `ff5f519b1` on
2026-08-25, and no pull request in the repository has merged since 2026-09-01. The latest release is
`core-v31.5.0`, from 2026-08-08. The repository is not archived. Inference: Era is in security
maintenance until EraVM retires. Because of the publication delay, public Era source can also lag
production by design.

### What ZKsync OS is, and what is public

ZKsync OS is a Rust state transition function. It runs natively in the sequencer and is compiled to
RISC-V so that the Airbender prover can prove it
([README](https://github.com/matter-labs/zksync-os/blob/f91897ce4ff4ede1f8d338411d023645684d0fb7/README.md)).
The README names several execution environments as a design goal. The
[ZKsync OS FAQ](https://github.com/matter-labs/zksync-docs/blob/14de8d0a8a5470f6d7210f3e7d5f6bc2844b0948/content/00.zksync-network/65.zksync-os/10.faqs.md)
is narrower. It says ZKsync OS "will replace EraVM", that EVM is "the first and only execution
environment for the initial version", and that "EraVM and native EraVM account abstraction are not
supported". The FAQ also says there is no public mainnet. The Q3 report supersedes that line. ZKsync
OS 0.5.0, in protocol version v33, reached the mainnets Matter Labs operates in September 2026.

ZKsync OS ships with a separate server, `zksync-os-server`. It is a new node with its own pipeline,
not the Era server with a different VM. The public source has three limits that matter for an
independent chain:

- The newest release branch is not buildable from public source. The
  [`v0.5.x-private` manifest](https://github.com/matter-labs/zksync-os/blob/5610fb622832acbdf4feeb0ae458a5c3436b2b6c/Cargo.toml)
  takes prover crates from `matter-labs/zksync-airbender-private`, which the GitHub API does not
  return. The default branch still uses the public `zksync-airbender` tag `v0.5.2`.
- The server is quiet in public. `zksync-os-server` `main` is from 2026-08-24, no pull request in
  the repository has merged since 2026-09-01, and the newest public branch, `di/1m-tps-dvp`, is a demonstration
  branch. Which source produced the v33 production server is not established.
- Development is visible elsewhere. For example, `zksync-airbender` merged 72 pull requests since
  2026-09-01, mostly into non-default branches. A quiet default branch is not a quiet project.

ZKsync OS assumes Ethereum settlement. The server's
[`SendToL1`](https://github.com/matter-labs/zksync-os-server/blob/e29a2c0a584b7f3f985e9a414bfbe179456c093d/lib/l1_sender/src/commands/mod.rs)
command builds Solidity calldata through `fn solidity_call(&self, operator: &Address) -> Bytes;`.
The [pubdata modes](https://github.com/matter-labs/zksync-os-server/blob/e29a2c0a584b7f3f985e9a414bfbe179456c093d/lib/types/src/pubdata_mode.rs)
are `Blobs`, `Calldata`, and `Validium`. The searched server source has no Celestia client and no
Bitcoin adapter.

### What stays Via-owned on any target

Bitcoin settlement and Celestia data availability were never upstream features. Via built them on
top of Era, and Era also assumes Ethereum. Its
[`SettlementLayer`](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/basic_types/src/settlement.rs)
is `L1` or `Gateway`, and both load EVM contracts through Ethereum clients. ZKsync OS lacking
Bitcoin and Celestia support is therefore not a difference between the targets. Via owns those
parts on either one.

The targets differ in four places:

| Area | zksync-era | ZKsync OS |
| --- | --- | --- |
| VM | EraVM, with native account abstraction | EVM only in the initial version |
| Proofs | Boojum, EraVM proof statement | Airbender proofs of the RISC-V program |
| Server | Era node framework, state keeper | New `zksync-os-server` pipeline |
| History | Continuous with Via's current chain | Contracts, accounts, state, and proof history need an explicit transition |

### Four options, and why the refork waits

| Option | For | Against |
| --- | --- | --- |
| Refork onto the last Era `main` | Closest to today's code. Existing interface research applies. | Builds on a VM its owner is retiring. Via would carry a second end-of-life alone. |
| Refork onto ZKsync OS | Where upstream development continues. | EVM-only today. The newest release depends on private source. The server lineage is unclear. Migration of contracts and history is undefined. |
| Era now, ZKsync OS later | Could relieve near-term constraints. | Pays the integration cost twice and adds a second history transition. |
| Wait, with defined exit conditions | Avoids committing to a retiring stack or to an incomplete public stack. | Today's fork keeps its divergence. Waiting needs explicit triggers to stay bounded. |

The maintainer chose to wait. An Era refork no longer catches up with a continuing mainstream. It
moves Via onto a stack in security maintenance. ZKsync OS has the strongest evidence of long-term
upstream alignment, but a refork needs a complete, public, buildable stack, and that is not
established today.

### Exit conditions before a refork starts

The refork stays deferred until all three conditions hold:

1. A public, maintained, and reproducibly buildable ZKsync OS cohort exists: server, OS, interface,
   prover, and contracts at compatible revisions, with proof verification.
2. Via's compatibility requirements are written down explicitly: contracts, accounts, state,
   history, and pending obligations such as withdrawals not yet paid.
3. Upstream source-publication and support arrangements are workable for an independent chain.

Then a bounded vertical spike comes first: Bitcoin ingestion, execution, Celestia publication, and
proof verification, including one restart and one Bitcoin reorg. A full refork commitment follows
only if that spike succeeds.

### What current via-core work does meanwhile

Fixes continue on today's via-core. Each fix should leave something a future target can reuse:

- A behaviour contract: what Via must do, in observable terms, apart from the code that does it.
- Regression evidence: a test or fixture that fails if the behaviour breaks.
- Preserved golden bytes, proof statements, signing rules, and restart and reorg cases.
- Consolidation of duplicated behaviour where current work already touches it, as
  [issue #382](https://github.com/vianetwork/via-core/issues/382) requires for the refork.

### Refork-method findings that hold for any target

These findings come from an earlier study of how established projects resync heavily diverged
forks. They do not depend on which upstream Via picks.

- An inventory is coverage evidence, not a plan. ChromeOS rebased about 5,644 kernel changes by
  splitting them into topic branches first
  ([rebase notes](https://chromium.googlesource.com/website/+/86aee6baf468b192aa09091d5ed4064ab486e47a/site/chromium-os/developer-library/guides/kernel/kernel-rebase-notes/index.md)).
  Optimism classifies each fork consumer by what it does before it changes a dependency
  ([decoupling analysis](https://github.com/ethereum-optimism/optimism/blob/55a3061885a5b26b1c22cca89a996d35038f250e/docs/ai/opgeth-decoupling.md)).
  The PR #378 file census is useful in that role. Its v29 execution plan is superseded.
- Acceptance comes from behaviour, not compilation. The same Optimism analysis states that
  "Compile-compat is necessary, not sufficient". It pins equivalence with live differential tests
  and keeps shared golden vectors after those tests are deleted.
- A library and extension-point model limits the next divergence. Reth composes nodes from
  replaceable builders
  ([node components](https://github.com/paradigmxyz/reth/blob/f88813adb966324ecfc530a2f78df38ed0df5750/docs/vocs/docs/pages/sdk/node-components.mdx)).
  Linea retired its Besu fork for upstream Besu plus plugins
  ([archived fork](https://github.com/Consensys/linea-besu/blob/bcf132cf09dd455a6fb08382e4cf03a6deffb5f8/README.md)).
  The model does not remove every fork. Base still depends on its own `base/reth` tag
  ([manifest](https://github.com/base/base/blob/536bcfdf2b7ec646e50a3936375009e54a85b606/Cargo.toml)).
- Research stays valid when each claim names its revision. op-geth's
  [`fork.yaml`](https://github.com/ethereum-optimism/op-geth/blob/a160d1eac9d14b6f80dcfac94c6a48b5316ba8db/fork.yaml)
  pins the upstream base hash for its diff. Android's
  [patch series](https://android.googlesource.com/kernel/common-patches/+/40e2b065ba56b565a9a3461dc28c70c4acecfdbf/android-mainline/series)
  records the upstream base and the target it matches, and tags each patch by origin. For Via,
  state each upstream claim with a SHA and the paths that would invalidate it. Re-check those
  claims against current upstream before acting on them.

## Limits

- This note uses public documents and source only. It does not prove a deployed revision, an audit
  scope, or production throughput for any chain.
- The FAQ and the Q3 report disagree on the mainnet status of ZKsync OS. This note follows the newer
  report.
- Whether `zksync-airbender-private` is private or deleted is unknown. Either way it is unavailable
  to an independent chain.
- No ZKsync OS build, Bitcoin prototype, or proof verification was run.
- Per-chain ZKsync OS adoption by other ZK Stack chains was not confirmed.

## Open questions

1. Which complete source cohort does Matter Labs support for new independent ZKsync OS chains,
   including the production server and prover dependencies?
2. Will ZKsync OS execute EraVM bytecode or support native account abstraction, and when?
3. Which Via contracts, accounts, state, history, and pending obligations must survive a
   transition, and which may change at a defined boundary?
4. Can Bitcoin settlement and Celestia publication live outside the upstream server as owned
   pipeline components, or do they need upstream changes?
5. What does the EraVM transition require of third-party chains that run their own EraVM nodes?

Related notes: [Via contracts and a future refork](via-contracts-and-a-future-refork.md) and
[Withdrawal authorization](withdrawal-authorization-and-refork.md) hold Era-specific evidence that
remains useful as historical comparison.
