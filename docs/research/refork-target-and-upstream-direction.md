# Refork target and upstream direction

Status: research with one recorded maintainer decision, made on 2026-10-20. The latest zksync-era
`main` is no longer a refork target. ZKsync OS is the likely eventual target. The refork has not
started and waits for the exit conditions below. No implementation, migration, regenesis, or
deployment is approved by this note. Evidence accessed 2026-10-20.

Last verified: 2026-10-20. Before acting on a fact bound to a revision or date, re-check it as
[How to re-check this note](#how-to-re-check-this-note) describes.

## Question

Via was forked from `matter-labs/zksync-era`. Is zksync-era still the upstream that a future Via
refork should target? If not, what is the likely target, when is a refork worth starting, how large
is the port, and what should current via-core work do in the meantime?

## Source revisions

| Source | Revision |
| --- | --- |
| via-core `main` (Celestia comparison) | `eec4bb8f59fff0c23ddc9cdfa21910882ca36be6` |
| via-core `main` (line counts, duplication classification) | `5e837ac2911b489d953d7c96e31cd85dc23577f4` |
| Via fork point in zksync-era | `f37b84ac75de8606382943bb10b8d064c475b5a0`, 2024-08-28 |
| zksync-era `main` | `ff5f519b11cff863edcfa0f75af10fea113806b0`, 2026-08-25 |
| zksync-os `v0.5.x-private`, the active line on 2026-10-20 | `5610fb622832acbdf4feeb0ae458a5c3436b2b6c`, 2026-10-19 |
| zksync-os `main`, historical comparison only | `f91897ce4ff4ede1f8d338411d023645684d0fb7`, 2026-08-19 |
| zksync-os-server `di/fix/87680-fri-fix`, the public 0.5-family branch | `66ba2731bc8d6ebce1c85dd0fa67054ca2855a40`, 2026-08-25 |
| zksync-os-server `main` | `e29a2c0a584b7f3f985e9a414bfbe179456c093d`, 2026-08-24 |
| zksync-os-interface `main`, release v0.2.0 | `c0f70563ddf78c3be2655b1c698ac8fc80a3d64e`, 2026-10-19 |
| airbender-platform `di/fix/87680-fri-fix` | `322e96becb75ef46953d4a8d6dc015a665ba894d` |
| zksync-docs `main` | `14de8d0a8a5470f6d7210f3e7d5f6bc2844b0948` |

Forum posts, blog posts, and the rendered ZKsync docs are moving documents, accessed 2026-10-20.
Sources for the refork-method findings are pinned in their own section. The duplication
classification used below is in
[Duplication paydown and the refork](duplication-paydown-and-refork.md).

## Findings

### Via forked Era shortly after core-v24.22.0

Via's recorded fork point is `f37b84ac75de8606382943bb10b8d064c475b5a0` (2024-08-28). It is five
upstream commits after the `core-v24.22.0` release commit `a6b6e829550a8f84dc4b96748028caee07624dec`
([comparison](https://github.com/matter-labs/zksync-era/compare/core-v24.22.0...f37b84ac75de8606382943bb10b8d064c475b5a0)).
That history stays true whatever the next target is.

### Matter Labs is retiring EraVM

Matter Labs announced on 2026-09-04 that EraVM will be retired. The
[EraVM security update](https://www.zksync.io/blog/eravm-security-update) says that within six
months "EraVM chains will begin a transition". It also lists five security measures for EraVM
chains, including publication of covered Era protocol code three months after each upgrade ships.
The transition does not apply to chains running
[Atlas](https://www.zksync.io/blog/introducing-the-zk-stacks-atlas-upgrade). Atlas is Matter Labs'
name for the ZKsync OS stack: a new server, ZKsync OS, and the Airbender prover.

Via is an independent Bitcoin-settled chain whose code descends from Era's v24 line. Via runs
neither Atlas nor Matter Labs' EraVM network. The announcement does not itself define a migration
schedule for Via, and Via owns any upgrade decision. The retirement of Via's upstream code line and
the relevant security changes still need assessment.

The [Matter Labs Q3 2026 deliverables report](https://forum.zknation.io/t/matter-labs-q3-2026-deliverables-report/1172),
posted on 2026-10-12, confirms the direction. EraVM chains "keep receiving security work", and
ZKsync OS is where "all new ZKsync protocol development happens". The
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
supported". The FAQ keeps preview-era wording that there is no public mainnet. Separately, Matter
Labs reports in the Q3 report that v33, which contains ZKsync OS 0.5.0, reached "the mainnets Matter
Labs operates" in September 2026. That is reported production use. It does not establish whether
those mainnets are public or which source revisions they run.

ZKsync OS ships with a separate server, `zksync-os-server`. It is a new node with its own pipeline,
not the Era server with a different VM.

Development continues off the default branches:

- zksync-os `main` has had no commits since 2026-08-19. The `v0.5.x-private` branch is 219 commits
  ahead of `main` and 0 behind. Its tip, from 2026-10-19, implements zksync-os-interface 0.2.0,
  which was released the same day. The repository also has the tags `v0.5.1-private` through
  `v0.5.4-private`. The three zksync-os pull requests merged since 2026-09-01 all went into other
  branches: [#747](https://github.com/matter-labs/zksync-os/pull/747) into `v0.5.x-private`,
  [#748](https://github.com/matter-labs/zksync-os/pull/748) into `draft-0.5.0`, and
  [#751](https://github.com/matter-labs/zksync-os/pull/751) into `av_integrate_v3`.
- `zksync-airbender` merged 74 pull requests between 2026-09-01 and 2026-10-20, while its `main`
  stayed at an April commit.
- A quiet default branch is therefore not a quiet project. Which branch carries current work is a
  dated fact: on 2026-10-20 it was `v0.5.x-private`, and the findings below are based on it. Work can
  move back to `main` or to another branch, so identify the active line again at each re-check before
  reusing these findings.

The public source has three limits that matter for an independent chain:

- **The current OS line is not buildable from public source.** The
  [`v0.5.x-private` manifest](https://github.com/matter-labs/zksync-os/blob/5610fb622832acbdf4feeb0ae458a5c3436b2b6c/Cargo.toml#L107-L126)
  takes the transpiler, prover, verifier and related crates from `matter-labs/zksync-airbender-private`,
  which the GitHub API does not return. The gap is wider than an optional GPU prover: OS
  `forward_system` depends on airbender-platform's `airbender-crypto`, which has a non-optional
  dependency on the same private repository
  ([forward_system manifest](https://github.com/matter-labs/zksync-os/blob/5610fb622832acbdf4feeb0ae458a5c3436b2b6c/forward_system/Cargo.toml#L14-L34),
  [airbender-crypto manifest](https://github.com/matter-labs/airbender-platform/blob/322e96becb75ef46953d4a8d6dc015a665ba894d/crates/airbender-crypto/Cargo.toml#L14-L22)).
  The host code, interface, DA commitment generator and public-input construction are readable.
  The prover circuits, verifier internals and proof-artifact compatibility are not.
- **No inspected public server pairs with the current OS tip.** The public server branch
  [`di/fix/87680-fri-fix`](https://github.com/matter-labs/zksync-os-server/blob/66ba2731bc8d6ebce1c85dd0fa67054ca2855a40/Cargo.toml#L119-L199)
  integrates OS `v0.5.1-private` with interface 0.1.3. A sweep of all 187 public server branch heads
  on 2026-10-20 found none whose declared dependencies require interface 0.2.0 or name
  `v0.5.x-private`. No build was attempted, so an unadvertised pairing is not ruled out. The latest
  server release is v0.23.0, from 2026-08-24. Which source produced the v33 production server is
  not established.
- **Server architecture is stable across the public branches.** The 0.5-family branch and the
  server `main` have byte-identical `PipelineComponent`, `SendToL1`, `PubdataMode`, database
  documentation and root configuration. The findings below about the server therefore hold for both.

### Source publication

Matter Labs has announced a publication delay, not a move to closed source. Verified against the
[EraVM security update](https://www.zksync.io/blog/eravm-security-update) on 2026-10-20:

- "Under the new policy, covered Era protocol code will be published three months after the
  corresponding upgrade ships."
- "We believe in open source and we are committed to it. However being open source today also hands
  attackers a disproportionate advantage on frozen code."
- Independent auditors keep continuous access, and bug-bounty researchers work against published
  code "that may not match what is running".

The post does not define "covered", gives no end date, and does not say whether the delay applies to
ZKsync OS, Atlas or Airbender. The
[Security Council notice of 2026-09-01](https://forum.zknation.io/t/notice-of-2026-instant-upgrades-security-patches/1099)
adds that "no Security Patch Reports will be published at this time". No governance vote on the
publication delay was found in the sources checked. Repository licenses have not changed: the inspected zksync-era, era-contracts, zksync-os,
zksync-os-server and zksync-airbender repositories remain Apache-2.0 or MIT.

The private dependencies of the current OS line have no published explanation. One public,
unmerged pull request,
[zksync-airbender-prover #157](https://github.com/matter-labs/zksync-airbender-prover/pull/157),
repoints the Airbender, wrapper and crypto dependencies "to the private branches carrying the FRI
batch degree-lift fix". Inference: at least part of the private code carries an embargoed security
fix. Whether it will be published, and when, is unknown.

Consequence for Via: today the newest ZKsync OS line cannot be built or fully reviewed from public
source, and covered Era protocol code reaches the public at least three months after it ships. Both bear directly on
exit conditions 1 and 3 below.

### What stays Via-owned on any target

Bitcoin settlement was never an upstream feature. Era assumes Ethereum. Its
[`SettlementLayer`](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/basic_types/src/settlement.rs)
is `L1` or `Gateway`, and both load EVM contracts through Ethereum clients. The ZKsync OS server is
Ethereum-shaped as well. Its
[`SendToL1`](https://github.com/matter-labs/zksync-os-server/blob/66ba2731bc8d6ebce1c85dd0fa67054ca2855a40/lib/l1_sender/src/commands/mod.rs#L73-L91)
command builds Solidity calldata through `fn solidity_call(&self, operator: &Address) -> Bytes;`, and
its [priority watcher](https://github.com/matter-labs/zksync-os-server/blob/66ba2731bc8d6ebce1c85dd0fa67054ca2855a40/lib/l1_watcher/src/tx_watcher.rs#L1-L29)
watches Ethereum mailbox events. Via owns Bitcoin settlement on either target.

Via built its own Celestia client because Era had none at the fork point. Upstream added one on
2024-10-29 ([PR #2983](https://github.com/matter-labs/zksync-era/pull/2983)), and current Era `main`
has it in [`core/node/da_clients/src/celestia/client.rs`](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/da_clients/src/celestia/client.rs#L48-L102).
It is not a drop-in replacement. It encodes the blob ID as hex of a bincode
`BlobId { commitment, height }` and returns empty inclusion data.
[Via's client](https://github.com/vianetwork/via-core/blob/eec4bb8f59fff0c23ddc9cdfa21910882ca36be6/core/lib/via_da_clients/src/celestia/client.rs#L94-L159)
encodes eight big-endian height bytes followed by the commitment. Its inclusion data is the blob
content, reassembled from chunks, which the Via verifier and withdrawal client read. Via's Celestia
wire format and retrieval semantics therefore stay Via-owned on either target.

ZKsync OS also changes the DA model, so the old server's modes are not the whole picture. The server
has three [pubdata modes](https://github.com/matter-labs/zksync-os-server/blob/66ba2731bc8d6ebce1c85dd0fa67054ca2855a40/lib/types/src/pubdata_mode.rs#L4-L11):
`Blobs`, `Calldata`, and `Validium`. The current OS line separately defines a
[commitment scheme and pubdata content](https://github.com/matter-labs/zksync-os/blob/5610fb622832acbdf4feeb0ae458a5c3436b2b6c/zk_ee/src/common_structs/da_commitment_scheme.rs#L11-L83),
and its [DA documentation](https://github.com/matter-labs/zksync-os/blob/5610fb622832acbdf4feeb0ae458a5c3436b2b6c/docs/da_commitment_schemes.md#L12-L53)
describes version-3 full and logs-only streams. Neither has a Celestia client or a Bitcoin adapter.
Via's Celestia transport can survive, but its payload and commitment boundary must adapt, and old
Era pubdata decoders cannot read version-3 payloads.

The targets differ in four places:

| Area | zksync-era | ZKsync OS |
| --- | --- | --- |
| VM | EraVM, with native account abstraction | EVM only in the initial version |
| Proofs | Boojum, EraVM proof statement. EraBender, an Airbender-based second EraVM prover, is planned alongside Boojum. The [Q3 report](https://forum.zknation.io/t/matter-labs-q3-2026-deliverables-report/1172) says its contract audit started and that end-to-end validation and production rollout are still ahead. | Airbender proofs of the RISC-V program |
| Server | Era node framework, state keeper, PostgreSQL plus RocksDB | New `zksync-os-server` pipeline, RocksDB only ([database design](https://github.com/matter-labs/zksync-os-server/blob/66ba2731bc8d6ebce1c85dd0fa67054ca2855a40/docs/src/design/db.md#L1-L10)) |
| History | Closer execution ancestry. Preserving Via state, proofs, history, and pending obligations still needs a compatibility and cutover plan. | A changed execution, account, and proof model. Preserving or migrating the same Via obligations needs an explicit plan. |

### How large a ZKsync OS port is

A ZKsync OS refork does not rewrite everything Via has. Most of the code that changes is upstream
code that Via would adopt rather than write. Rust line counts on via-core `main` at `5e837ac29`
(`find <dir> -name '*.rs' | xargs cat | wc -l`; physical lines, including tests and comments):

| Code | Lines | On a ZKsync OS refork |
| --- | ---: | --- |
| Inherited Era code in `core/`, excluding `via_*` directories | about 294,000 | Replaced by Matter Labs' server and OS |
| Inherited `prover/` | about 22,000 | Replaced by Airbender |
| Via's `via_*` directories in `core/` | about 42,000 | Carried over: ported or adapted |
| `via_verifier/` | about 25,000 | Carried over: still Via's responsibility |

The counts omit Via edits inside inherited files, and Solidity contracts. Of the Via-owned code, the
glue to Era's framework is rewritten: node-framework wiring layers, the state-keeper hookup, Era proof
and batch formats, and the binary configuration loaders. Via behavior carries over: Bitcoin
ingestion, deposits and wallets, reorg rules, inscription delivery, signing and the verifier
network. The [duplication classification](duplication-paydown-and-refork.md) puts 77.5% of
today's cross-tree duplicated tokens in that carried-over class and 2.9% in the rewritten class.

The difficult parts are protocol decisions, not code volume:

1. **Execution model.** ZKsync OS is EVM-only today. Via's EraVM contracts, accounts and state must
   be migrated, or the chain restarts from a new genesis. Either choice affects users and funds.
2. **Proof statement.** The Bitcoin verifier network must verify Airbender proofs instead of Boojum
   proofs, with a new program, key and public-input binding. The native verifier backend is private
   today.
3. **Settlement adapter.** Bitcoin inscriptions, confirmations and payouts replace the
   Ethereum-shaped sender and watcher.
4. **DA payload.** The Celestia transport is reused, and the payload and commitment adapt to version-3
   pubdata.
5. **Upstream availability.** A supported, fully public, buildable server, OS and prover set
   matching the current OS line has not been established.

Inference: items 1 to 4 are comparable to completed migrations by funded teams. Celo moved to the
OP Stack with a new client and preserved history, reporting that it removed a 365,000-line diff
([launch account](https://forum.celo.org/t/returning-home-to-ethereum-the-launch-of-celo-l2-mainnet/10466)),
and Optimism replaced its architecture with Bedrock
([proposal](https://gov.optimism.io/t/final-upgrade-1-bedrock-protocol-upgrade-v2/5548)). Item 5 is
outside Via's control and is why the refork waits.

### Four options, and why the refork waits

| Option | For | Against |
| --- | --- | --- |
| Refork onto the last Era `main` | Closest to today's code. Existing interface research applies. | Builds on a VM its owner is retiring. Via would carry a second end-of-life alone. |
| Refork onto ZKsync OS | Where upstream development continues. | EVM-only today. The current line depends on private source. No inspected public server pairs with it. Migration of contracts and history is undefined. |
| Era now, ZKsync OS later | Could relieve near-term constraints. | Pays the integration cost twice and can require two cutovers. |
| Wait, with defined exit conditions | Avoids committing to a retiring stack or to an incomplete public stack. | Today's fork keeps its divergence. Waiting needs explicit triggers to stay bounded. |

The maintainer chose to wait. An Era refork no longer catches up with a continuing mainstream. It
moves Via onto a stack in security maintenance. ZKsync OS has the strongest evidence of long-term
upstream alignment, but a refork needs a complete, public, buildable stack, and that is not
established today.

If the private dependencies stay private for many months, or the publication delay is extended to
ZKsync OS without an end date, ZKsync OS stops being a realistic target. The target question would
then reopen, including staying on Via's own Era fork.

### Exit conditions before a refork starts

The refork stays deferred until all three conditions hold:

1. A public, maintained, and reproducibly buildable ZKsync OS cohort exists: server, OS, interface,
   prover, and contracts at compatible revisions, with proof verification. Public code that names a
   private dependency does not meet this condition. The evaluation includes the native verifier path,
   the program and verification-key identity, and proof and public-input compatibility.
2. Via's compatibility requirements are written down explicitly: contracts, accounts, state,
   history, and pending obligations such as withdrawals not yet paid.
3. Upstream source-publication and support arrangements are workable for an independent chain.

Then a bounded vertical spike comes first: Bitcoin ingestion, execution, Celestia publication, and
proof verification, including one restart and one Bitcoin reorg. A full refork commitment follows
only if that spike succeeds.

### What current via-core work does meanwhile

See [Refork preparation prior art](refork-preparation-prior-art.md) for preparation examples and
their limits.

Fixes continue on today's via-core. This work is target-neutral: it pays off today, on ZKsync OS,
and on Via's own Era fork. Each fix should leave something a future target can reuse:

- A behavior contract: what Via must do, in observable terms, apart from the code that does it.
- Regression evidence: a test or fixture that fails if the behavior breaks.
- Preserved golden bytes, proof statements, signing rules, and restart and reorg cases.
- Consolidation of shared Via behavior where correctness risk, repeated maintenance cost, or the port
  boundary justifies it. Prioritize Bitcoin ingestion, wallet and authority history, inscription
  delivery, and reorg and restart contracts, which stay Via-owned on any execution target. Keep
  role-specific scheduling and Era adapters outside those shared owners. Current work touching a
  duplicate is one trigger, not the limit. Defer cleanup of code with an evidenced replacement, such
  as the Era binary bootstrap. Preserve versioned historical formats and their regression evidence
  until their removal is explicitly justified. See
  [Duplication paydown and the refork](duplication-paydown-and-refork.md) for the evidence, and
  [issue #382](https://github.com/vianetwork/via-core/issues/382) for the refork's no-sibling-trees
  requirement.

Do not start ZKsync OS-specific work, such as adapting to its interfaces or writing a Bitcoin
settlement adapter, before the exit conditions hold. The public code trails the current line, and
the target can still change.

### Refork-method findings that hold for any target

These findings come from an earlier study of how established projects resync heavily diverged
forks. They do not depend on which upstream Via picks.

- An inventory is coverage evidence, not a plan. ChromeOS rebased about 5,644 kernel changes by
  splitting them into topic branches first
  ([rebase notes](https://chromium.googlesource.com/website/+/86aee6baf468b192aa09091d5ed4064ab486e47a/site/chromium-os/developer-library/guides/kernel/kernel-rebase-notes/index.md)).
  Optimism classifies each fork consumer by what it does before it changes a dependency
  ([decoupling analysis](https://github.com/ethereum-optimism/optimism/blob/55a3061885a5b26b1c22cca89a996d35038f250e/docs/ai/opgeth-decoupling.md)).
  The PR #378 file census is useful in that role. Its v29 execution plan is superseded.
- Acceptance comes from behavior, not compilation. The same Optimism analysis states that
  "Compile-compat is necessary, not sufficient". It pins equivalence with live differential tests
  and keeps shared golden vectors after those tests are deleted.
- A library and extension-point model limits the next divergence. Reth composes nodes from
  replaceable builders
  ([node components](https://github.com/paradigmxyz/reth/blob/f88813adb966324ecfc530a2f78df38ed0df5750/docs/vocs/docs/pages/sdk/node-components.mdx)).
  Linea retired its Besu fork for upstream Besu plus plugins
  ([archived fork](https://github.com/Consensys/linea-besu/blob/bcf132cf09dd455a6fb08382e4cf03a6deffb5f8/README.md)).
  The model does not remove every fork. Base still depends on its own `base/reth` tag
  ([manifest](https://github.com/base/base/blob/536bcfdf2b7ec646e50a3936375009e54a85b606/Cargo.toml)).
- Shared behavior can be consolidated while the base changes. Before moving onto libevm, Ava Labs
  extracted the nearly identical state-sync code of Coreth and Subnet-EVM into one shared engine
  because fixes had to be duplicated
  ([PR #4921](https://github.com/ava-labs/avalanchego/pull/4921), merged at
  `40177fc694909820e4de100d9b99cc0264c73c5b`), while it deferred packages that a planned change
  could make redundant ([issue #4596](https://github.com/ava-labs/avalanchego/issues/4596)).
- Research stays valid when each claim names its revision. op-geth's
  [`fork.yaml`](https://github.com/ethereum-optimism/op-geth/blob/a160d1eac9d14b6f80dcfac94c6a48b5316ba8db/fork.yaml)
  pins the upstream base hash for its diff. Android's
  [patch series](https://android.googlesource.com/kernel/common-patches/+/40e2b065ba56b565a9a3461dc28c70c4acecfdbf/android-mainline/series)
  records the upstream base and the target it matches, and uses patch-name prefixes to record
  provenance and status. For Via,
  state each upstream claim with a SHA and the paths that would invalidate it. Re-check those
  claims against current upstream before acting on them.

## How to re-check this note

The decision and the method findings are stable. The facts below are bound to revisions and dates,
and they go stale as upstream moves:

- Which branch of each repository carries current work. On 2026-10-20 it was zksync-os
  `v0.5.x-private` and, for the server, the 0.5-family branches; it can move back to `main` or on to
  another branch.
- Repository tips and dates for zksync-era, zksync-os, zksync-os-server and zksync-os-interface,
  on both the default branch and the active line.
- Merged pull-request counts since 2026-09-01, and the branches they merged into.
- Whether `zksync-airbender-private` is publicly reachable, and whether the `-private` tags have
  public equivalents.
- Whether any public server branch pairs with the current OS line.
- The scope and end date of the publication delay, and whether it covers ZKsync OS or Airbender.
- The ZKsync OS pubdata modes, the DA commitment scheme, and the Ethereum-shaped `SendToL1` command.
- The ZKsync OS FAQ wording on execution environments and mainnet status.
- The status of the EraVM transition and of any published migration procedure.

To re-check them:

1. Identify the active line of each repository without assuming a branch name. List the pull
   requests merged since the last check with `gh api -X GET search/issues` and a
   `repo:<repo> is:pr is:merged merged:>=<date>` query, then read each one's base branch with
   `gh api repos/<repo>/pulls/<number>`. Check which branch implements the newest
   zksync-os-interface release. Compare candidates with
   `gh api repos/<repo>/compare/main...<branch>` for the ahead and behind counts. A quiet default
   branch alone does not show that a project stopped, and an active `main` can replace a release
   branch again.
2. Resolve the tip SHA of the default branch and of the active line.
3. Check `gh api repos/matter-labs/zksync-airbender-private`. A 404 means the dependency is still
   unavailable. Also check whether the active line's manifest still names private repositories.
4. Repeat the source checks at the new SHAs: the manifest, `SendToL1`, `PubdataMode`, the DA
   commitment scheme, and a search for Celestia or Bitcoin support.
5. Re-read the ZKsync OS FAQ, the newest Matter Labs deliverables report, the EraVM security update,
   and forum posts on the EraVM transition.
6. Update each changed fact with its new SHA, or supersede the finding if its conclusion changes.
   Then update the "Last verified" date.

Via's own proposal, not a practice taken from another project: re-check monthly, and always before
refork planning or the vertical spike starts. If an exit condition appears to hold, re-check at
once.

## Limits

- This note uses public documents and source only. It does not prove a deployed revision, an audit
  scope, or production throughput for any chain.
- Public-mainnet availability is not resolved. The FAQ addresses public access, while the Q3 report
  describes unnamed mainnets that Matter Labs operates.
- `zksync-airbender-private` returns 404 to an authenticated account. Whether it is private or
  deleted is unknown. Either way it is unavailable to an independent chain.
- No ZKsync OS build, Bitcoin prototype, or proof verification was run. The build conclusions come
  from the dependency graph, not from an observed compiler failure.
- The line counts are physical lines and include tests. They measure scale, not porting effort.
- Per-chain ZKsync OS adoption by other ZK Stack chains was not confirmed.

## Open questions

1. Which complete source cohort does Matter Labs support for new independent ZKsync OS chains,
   including the production server and prover dependencies?
2. Will the private dependencies of the current OS line be published, and when?
3. What does "covered Era protocol code" include, does the delay end, and does it extend to ZKsync OS?
4. Will ZKsync OS execute EraVM bytecode or support native account abstraction, and when?
5. Which Via contracts, accounts, state, history, and pending obligations must survive a
   transition, and which may change at a defined boundary?
6. Can Bitcoin settlement and Celestia publication live outside the upstream server as owned
   pipeline components, or do they need upstream changes?
7. What does the EraVM transition require of third-party chains that run their own EraVM nodes?

Related notes: [Duplication paydown and the refork](duplication-paydown-and-refork.md) classifies
today's duplicated Via code by what survives a refork.
[Via contracts and a future refork](via-contracts-and-a-future-refork.md) and
[Withdrawal authorization](withdrawal-authorization-and-refork.md) hold Era-specific evidence that
remains useful as historical comparison.
