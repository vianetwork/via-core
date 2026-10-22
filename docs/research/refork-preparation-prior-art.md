# Refork preparation prior art

Status: research, 2026-10-22. The findings below inform preparation while Via remains on Era.
They do not select portable crates, adopt tools, or authorize a migration.

## Question

What can fixes in today's via-core preserve or improve before a replacement target is ready?
What did established projects change in their old code, what evidence survived, and what required rework?

The [target and upstream direction](refork-target-and-upstream-direction.md) owns the target assessment
and exit conditions. This note adds preparation evidence, dependency-boundary options, and migration
failure cases. The raw research report is local and untracked. The findings and pinned sources needed
for this question are retained here.

## Source revisions

| Source | Inspected revision |
| --- | --- |
| via-core | `9e6ede46ce3b24304921097e7c9c361032da9aaf` |
| Celo old client, Gingerbread and CIP-64 | `dbffebaecb33a58f6c3a4a6796fcda630c25827d`, `b7893f5bfbf12ef30ba99dc5866248b7110d76e2` |
| Celo op-geth and migration tool | `c69d62ac387bd3f90e76ce13702cf3f781ec0faa`, `30b66c82ed7eca00e0eed5fb7828f5ef3bb19f00` |
| Celo proposals | `27cb2489f7b8f9517962d78a3f889db790078223` |
| Mozilla Gecko, handle introduction, callers, comparison, removal | `db496f52f744b6ce7a9b16d835b5d1e3a25ea484`, `7c95c3973454ede566d0619aa3f3d4f9df16ffb8`, `983701185045ecacf489b645454b13f9a22c5722`, `4cf9aed667f3ecf03191a805b69e09eb5078f312` |
| Scroll old client, release, prover | `4bdf6d096c3837dcb9474b5edb30a0f2b286bf48`, `24757865c6bbd9becb0256e97e8492d1f73987d9`, `5854f8179d14f2afa489a499290e0ff6a953b314` |
| Optimism legacy and migration repair | `8205f678b7b4ac4625c2afe351b9c82ffaa2e795`, `c35b6e398aaec9cb34be4329535b051076e36b56` |
| Brave old client, importer, current fork tooling | `de4ff2d6931c2b9784fbe5227325cd47f21a3417`, `3e0028d9fe2df776c1407a5e12d545e7ca02d7e8`, `e0c4db7f4db5fcbd0eaf55b5f9bbc945bb48233b` |
| cargo-deny | `4a786a597daa80dcb698ca3a931dc1e199600be5` |
| Diem and guppy | `fc4714a8ea273b6efe8b13dbce72ea60aad9a16c`, `799d0f2f57e41a01a732b9b6bbab019cf9e82387` |
| Reth and execution-spec-tests | `53bf066feb7d9c39a026f45651851909bb9c1c56`, `7abf18e1ad5970232cf2646481024d8922194ec3` |
| Geth, early Merge and v1.10.26 | `3038e480f5297b0e80196c156d9f9b45fd86a0bf`, `e5eb32acee19cc9fca6a03b10283b7484246b15a` |

Source code and tests establish mechanisms, not successful local executions. Issue, forum, release,
and maintainer accounts below establish reported outcomes with narrower confidence than executable evidence.

## Findings

### Preparation in old code and what had to be redone

#### Celo reduced its protocol delta while the target changed

Celo's [2023 old-client plan](https://github.com/celo-org/celo-blockchain/discussions/2057) proposed
cleanup, a checkpoint, and reimplementation on Erigon, while allowing the target to change.
[Gingerbread v1.8.0](https://github.com/celo-org/celo-blockchain/releases/tag/v1.8.0) shipped header
and gas-cost alignment, specified in
[CIP-59](https://github.com/celo-org/celo-proposals/blob/27cb2489f7b8f9517962d78a3f889db790078223/CIPs/cip-0059.md)
and [CIP-58](https://github.com/celo-org/celo-proposals/blob/27cb2489f7b8f9517962d78a3f889db790078223/CIPs/cip-0058.md).
The [2024 stack-selection update](https://forum.celo.org/t/l2-stack-selection-update/7314) still
compared alternatives before the eventual OP Stack replacement. Its patch-queue requirement is a
maintainer statement; a concrete queue and shipped Erigon implementation were not established.

[CIP-64](https://github.com/celo-org/celo-proposals/blob/27cb2489f7b8f9517962d78a3f889db790078223/CIPs/cip-0064.md#rationale)
retained fee currencies but removed unused gateway-fee fields. The old client gained the
[new transaction type](https://github.com/celo-org/celo-blockchain/blob/b7893f5bfbf12ef30ba99dc5866248b7110d76e2/core/types/celo_dynamic_fee_tx_v2.go#L9)
and [old and new encoding tests](https://github.com/celo-org/celo-blockchain/blob/b7893f5bfbf12ef30ba99dc5866248b7110d76e2/core/types/types_test.go#L121)
before cutover. The replacement retained that representation but
[stopped new legacy admission at Cel2](https://github.com/celo-org/op-geth/blob/c69d62ac387bd3f90e76ce13702cf3f781ec0faa/core/types/celo_transaction_signing_forks.go#L49).
Producers could migrate before client replacement while historical decoding remained available.

The migration still needed [RLP representation changes](https://github.com/celo-org/optimism/blob/30b66c82ed7eca00e0eed5fb7828f5ef3bb19f00/op-chain-ops/cmd/celo-migrate/transform.go#L45-L108),
[state conservation and collision tests](https://github.com/celo-org/optimism/blob/30b66c82ed7eca00e0eed5fb7828f5ef3bb19f00/op-chain-ops/cmd/celo-migrate/state_test.go#L24-L146),
and [continuity tests](https://github.com/celo-org/optimism/blob/30b66c82ed7eca00e0eed5fb7828f5ef3bb19f00/op-chain-ops/cmd/celo-migrate/continuity_test.go#L24-L143).
For Via, the transferable idea is independently justified cleanup and staged compatibility.
CIP-58 warns that gas changes can break contracts. Similarity to upstream alone does not justify
removing Via behavior, and no porting-hours saving was measured.

#### Mozilla used a seam with a running second implementation

Mozilla introduced [StyleSetHandle dispatch](https://github.com/mozilla/gecko-dev/blob/db496f52f744b6ce7a9b16d835b5d1e3a25ea484/layout/style/StyleSetHandleInlines.h#L13)
and [migrated existing callers](https://github.com/mozilla/gecko-dev/commit/7c95c3973454ede566d0619aa3f3d4f9df16ffb8)
so Gecko and the emerging Servo style engine could coexist.
[Bug 1244068](https://bugzilla.mozilla.org/show_bug.cgi?id=1244068) records small regressions from an
initial virtual-interface approach. Tagged dispatch replaced it. The reftest runner then
[rendered both backends](https://github.com/mozilla/gecko-dev/blob/983701185045ecacf489b645454b13f9a22c5722/layout/tools/reftest/reftest.jsm#L1326)
and [compared output](https://github.com/mozilla/gecko-dev/blob/983701185045ecacf489b645454b13f9a22c5722/layout/tools/reftest/reftest.jsm#L1694).

After shipping, [handle removal](https://github.com/mozilla/gecko-dev/commit/4cf9aed667f3ecf03191a805b69e09eb5078f312)
touched 66 files. [Bug 1449400](https://bugzilla.mozilla.org/show_bug.cgi?id=1449400) records header
and compiler problems during removal. This supports measured coexistence when a second backend
actually runs. It does not establish a benefit from speculative Rust traits for an unavailable target.
Temporary dispatch has performance, maintenance, and eventual deletion costs.

#### Scroll captured witnesses before replacing the prover

Before Euclid, Scroll's old client added
[execution witnesses](https://github.com/scroll-tech/go-ethereum/blob/4bdf6d096c3837dcb9474b5edb30a0f2b286bf48/eth/api.go#L400),
[disk-root mapping](https://github.com/scroll-tech/go-ethereum/blob/4bdf6d096c3837dcb9474b5edb30a0f2b286bf48/core/rawdb/accessors_state.go#L98),
and [fork-aware root validation](https://github.com/scroll-tech/go-ethereum/blob/4bdf6d096c3837dcb9474b5edb30a0f2b286bf48/core/block_validator.go#L227).
The OpenVM prover retains [historical JSON witnesses](https://github.com/scroll-tech/zkvm-prover/blob/5854f8179d14f2afa489a499290e0ff6a953b314/crates/integration/testdata/phase1/witnesses/12508460.json)
and [phase-aware consumers](https://github.com/scroll-tech/zkvm-prover/blob/5854f8179d14f2afa489a499290e0ff6a953b314/crates/integration/src/testers/chunk.rs#L13).

The [v5.8.33 release](https://github.com/scroll-tech/go-ethereum/releases/tag/scroll-v5.8.33), pinned
above, requires a full resync into MPT mode. Old zkTrie mode can follow the chain but cannot verify
post-Euclid header roots or serve those state proofs. A running node therefore did not imply preserved
proof service. Via can adapt witness capture and explicit compatibility matrices, but proof statements,
state conversion, resync, and historical services remain separate questions. Live shadow proving and
unchanged copying of old fixtures were not established.

#### Optimism exported old-client witnesses and checked completeness

Bedrock preparation added an old-client [StateDumper](https://github.com/ethereum-optimism/optimism-legacy/blob/8205f678b7b4ac4625c2afe351b9c82ffaa2e795/l2geth/statedumper/dumper.go#L12-L60)
called from the [EVM path](https://github.com/ethereum-optimism/optimism-legacy/blob/8205f678b7b4ac4625c2afe351b9c82ffaa2e795/l2geth/core/vm/evm.go#L202-L219).
It wrote address and message witnesses. The replacement's
[PreCheckWithdrawals](https://github.com/ethereum-optimism/optimism-legacy/blob/8205f678b7b4ac4625c2afe351b9c82ffaa2e795/op-chain-ops/crossdomain/precheck.go#L19-L112)
compared witnesses with legacy storage, rejected omissions, and filtered extras.
[Migration ordering](https://github.com/ethereum-optimism/optimism-legacy/blob/8205f678b7b4ac4625c2afe351b9c82ffaa2e795/op-chain-ops/genesis/db_migration.go#L123-L201)
places checks before conversion, but includes a `noCheck` bypass. Binary completion alone proves less.

An [audit finding](https://github.com/sherlock-audit/2023-01-optimism-judging/issues/113) identified
direct message-passer calls that invalidated witness assumptions.
[Repair #5058](https://github.com/ethereum-optimism/optimism/pull/5058), pinned above,
added invalid-slot checks and tests and removed a separate TypeScript generation step.
The [upgrade clarification](https://gov.optimism.io/t/final-upgrade-1-bedrock-protocol-upgrade-v2/5548)
limits replay to L1 transactions because old and new L2 execution diverge. For Via, witnesses need
independent completeness and obligation checks. Reproducing only successful current outputs can
preserve omissions or shared bugs.

#### Brave retained old-format fixtures but encountered lifecycle failures

Brave's final Muon client added [installation and once-only launch handling](https://github.com/brave/browser-laptop/blob/de4ff2d6931c2b9784fbe5227325cd47f21a3417/app/darwinInit.js#L27)
and [Windows handoff](https://github.com/brave/browser-laptop/blob/de4ff2d6931c2b9784fbe5227325cd47f21a3417/app/windowsInit.js#L118).
Core's [importer tests](https://github.com/brave/brave-core/blob/3e0028d9fe2df776c1407a5e12d545e7ca02d7e8/utility/importer/brave_importer_unittest.cc#L28)
consume [profiles generated with the old browser](https://github.com/brave/brave-core/blob/3e0028d9fe2df776c1407a5e12d545e7ca02d7e8/test/data/import/brave-browser-laptop/README).
The assets include JSON sessions and database files.

Brave staff [reported halting rollout after two hours](https://community.brave.app/t/for-users-who-had-issues-migrating-data-from-brave-muon-to-brave-core/39669).
[Sequential import failure](https://github.com/brave/brave-browser/issues/2438),
[process and stale-lock ordering](https://github.com/brave/brave-browser/issues/2503), and
[malformed old state](https://github.com/brave/brave-browser/issues/2517) exposed gaps beyond happy-path
value assertions. Separate profiles preserved recovery options. Via can retain old-format fixtures
and explicit retry, partial-failure, and recovery expectations. Local data preservation cannot reverse
an already activated external protocol change. No general target-neutral Muon abstraction was verified.

#### The Merge's early interfaces still changed

Geth's [early execution-layer API](https://github.com/ethereum/go-ethereum/blob/3038e480f5297b0e80196c156d9f9b45fd86a0bf/eth/catalyst/api.go#L174-L218)
preceded the production Merge. By [v1.10.26](https://github.com/ethereum/go-ethereum/blob/e5eb32acee19cc9fca6a03b10283b7484246b15a/eth/catalyst/api.go#L154-L340),
versioned payload and fork-choice methods had different signatures.
[Interop discussion #23607](https://github.com/ethereum/go-ethereum/pull/23607) records field and
response-shape mismatches on a draft branch. Even a public shared protocol and working consumers
required rework. This supports versioned specifications and executable interop once a target exists,
not premature OS-specific interface design.

### Dependency reachability is separate from shared ownership

At the inspected Via revision, [via_reorg has no dependencies](https://github.com/vianetwork/via-core/blob/9e6ede46ce3b24304921097e7c9c361032da9aaf/core/lib/via_reorg/Cargo.toml).
[via_btc_client](https://github.com/vianetwork/via-core/blob/9e6ede46ce3b24304921097e7c9c361032da9aaf/core/lib/via_btc_client/Cargo.toml)
depends on Era types, configuration, and object storage.
[via_da_clients](https://github.com/vianetwork/via-core/blob/9e6ede46ce3b24304921097e7c9c361032da9aaf/core/lib/via_da_clients/Cargo.toml)
also depends on the node framework. A shared crate is therefore not necessarily portable.
Which crates should be portable remains an open decision.

For named forbidden packages, cargo-deny's [root selection](https://github.com/EmbarkStudios/cargo-deny/blob/4a786a597daa80dcb698ca3a931dc1e199600be5/src/cargo-deny/main.rs#L115)
and [graph construction](https://github.com/EmbarkStudios/cargo-deny/blob/4a786a597daa80dcb698ca3a931dc1e199600be5/src/cargo-deny/common.rs#L283)
allow a run scoped to a real member manifest without `--workspace`.
Ordinary package bans can then reject Era dependencies reachable from that designated root while
other workspace roots retain legitimate Era dependencies. A virtual workspace manifest expands roots.
This is a candidate to adopt unchanged, subject to validating Via's pinned toolchain and resolution.

The configuration has material limits:

- [wrappers](https://github.com/EmbarkStudios/cargo-deny/blob/4a786a597daa80dcb698ca3a931dc1e199600be5/src/bans.rs#L608)
  permits approved immediate dependents of a banned crate. It does not prevent a portable ancestor
  from reaching that crate through the wrapper.
- [allow-workspace](https://github.com/EmbarkStudios/cargo-deny/blob/4a786a597daa80dcb698ca3a931dc1e199600be5/src/bans.rs#L598)
  bypasses explicit bans for workspace members. It would defeat bans on local Era crates.
- [Graph exclusions and feature selection](https://github.com/EmbarkStudios/cargo-deny/blob/4a786a597daa80dcb698ca3a931dc1e199600be5/docs/src/checks/cfg.md#L60)
  constrain the measured graph. Hiding nodes does not prove independence. Supported targets, features,
  build dependencies, and development dependencies need explicit coverage.

A [full allowlist](https://github.com/EmbarkStudios/cargo-deny/blob/4a786a597daa80dcb698ca3a931dc1e199600be5/src/bans.rs#L669)
is another existing option, with external-package maintenance costs. If automatic workspace-layer
classification is needed, Diem's [MoveCratesDontDependOnDiemCrates](https://github.com/diem/diem/blob/fc4714a8ea273b6efe8b13dbce72ea60aad9a16c/devtools/x/src/lint/guppy.rs#L579)
is closer prior art. It checks direct edges for every crate in a closed layer, with exceptions, so
a transitive escape crosses a checked forbidden edge. Guppy's [graph queries](https://github.com/guppy-rs/guppy/blob/799d0f2f57e41a01a732b9b6bbab019cf9e82387/guppy/src/graph/query.rs#L99)
can supply traversal, but Via would still own the classification policy and its tests.
Finite bans avoid that custom policy implementation. No dependency gate is adopted here.

### Behavior evidence a current fix can leave behind

The reusable evidence is source input plus expected decisions under stated versions and initial state.
That includes accepted and rejected input, duplicate delivery, retry, reorg, and restart points.
The existing [withdrawal evidence](withdrawal-authorization-and-refork.md) and
[cross-target evidence](via-contracts-and-a-future-refork.md#evidence-that-remains-useful-after-the-code-moves)
identify Via's byte and lifecycle obligations. The proposal is to preserve evidence for the touched
behavior, using its existing owner. It does not select a universal fixture schema or shared test crate.

Reth's [external fixture consumer](https://github.com/paradigmxyz/reth/blob/53bf066feb7d9c39a026f45651851909bb9c1c56/testing/ef-tests/src/models.rs#L18-L40)
and EEST's [fill-consume model](https://github.com/ethereum/execution-spec-tests/blob/7abf18e1ad5970232cf2646481024d8922194ec3/docs/filling_tests/index.md#L3-L30)
separate generated JSON expectations from client internals. Existing Ethereum fixtures fit only matching
execution semantics. They do not establish Bitcoin settlement or EraVM compatibility.

| Case | Durable input and expectations | Limit |
| --- | --- | --- |
| Bedrock | Line-oriented witnesses; JSON addresses, messages, receipts, traces, state diffs; migration roots | No old/new L2 execution equality; bypassed checks do not establish completeness |
| Celo | Historical transactions; RLP transformation input; JSON configuration; state and continuity assertions | No identical database representation or full historical replay guarantee |
| Ethereum clients | Versioned JSON pre-state, blocks, post-state, and rejection fixtures; RLP; RPC request/response files | No EraVM, Bitcoin, or whole-node restart guarantee |
| Stylo | HTML, CSS, references, manifests, and compared rendering | No universal site or performance guarantee |
| Brave | Old JSON sessions and databases; imported-value assertions | No complete lock, malformed-state, or recovery coverage |
| Scroll | Historical JSON witnesses and phase-specific consumers | No unchanged proof statements or old-mode proof-service guarantee |

The table condenses the cases above. Bedrock's [receipt and trace assets](https://github.com/ethereum-optimism/optimism-legacy/blob/8205f678b7b4ac4625c2afe351b9c82ffaa2e795/op-chain-ops/crossdomain/testdata/README.md)
and execution-apis' [RPC fixture](https://github.com/ethereum/execution-apis/blob/16caee274b0d4fb7a0316f294029adac88a75a09/tests/eth_blockNumber/simple-test.io)
show additional concrete formats. A saved output is an oracle only when its correctness is justified
independently. Differential agreement can preserve a shared bug, and normalization must preserve
meaningful bytes and ordering.

### Brave's ongoing Chromium fork tooling

These tools belong to later fork maintenance, not preparation for Muon's replacement.
All are prior art, not proposed for adoption now. Rust applicability is narrower than Brave's layout.

| Mechanism and pinned source | Verified behavior | What transfers to Rust and what does not |
| --- | --- | --- |
| [chromium_src overrides](https://github.com/brave/brave-core/blob/e0c4db7f4db5fcbd0eaf55b5f9bbc945bb48233b/docs/patching_and_chromium_src.md#L9) and [checker](https://github.com/brave/brave-core/blob/e0c4db7f4db5fcbd0eaf55b5f9bbc945bb48233b/tools/chromium_src/check_chromium_src.py#L8) | Shadow files isolate changes; checks detect missing targets, symbols, and include problems. Whole-file copies can miss upstream fixes. False positives are allowed. | An explicit coupling inventory can help. C++ include shadowing, macros, and GN wiring do not transfer unchanged; Rust compilation may already detect missing symbols. |
| [Patch checksums](https://github.com/brave/brave-core/blob/e0c4db7f4db5fcbd0eaf55b5f9bbc945bb48233b/build/commands/lib/gitPatcher.js#L176) and [tests](https://github.com/brave/brave-core/blob/e0c4db7f4db5fcbd0eaf55b5f9bbc945bb48233b/build/commands/lib/gitPatcher.test.js#L107) | Track patch, target, and application metadata; test repeated application, restoration, and invalid metadata. | Reproducible application is language-independent. A patch stack adds representation and metadata costs to Via's ordinary Git fork. |
| [Plaster](https://github.com/brave/brave-core/blob/e0c4db7f4db5fcbd0eaf55b5f9bbc945bb48233b/docs/plaster.md#L95) and [match-count tests](https://github.com/brave/brave-core/blob/e0c4db7f4db5fcbd0eaf55b5f9bbc945bb48233b/tools/cr/plaster_test.py#L943) | Transform pristine source into auditable patches with checked match counts and regeneration. | Text operations transfer in principle. No Rust AST integration was verified. The transformation is another program to maintain; match counts do not prove behavior. |
| [Brockit](https://github.com/brave/brave-core/blob/e0c4db7f4db5fcbd0eaf55b5f9bbc945bb48233b/tools/cr/brockit.py#L24) and [advisory-failure test](https://github.com/brave/brave-core/blob/e0c4db7f4db5fcbd0eaf55b5f9bbc945bb48233b/tools/cr/brockit_lift_test.py#L917) | Separates bump, conflicts, regeneration, and strings into commits with resumable states. Some final checks can fail without failing the lift. | Staged provenance and recovery can transfer. Commands and repository assumptions do not; completion cannot stand in for required gates. |
| [Rebase review guide](https://github.com/brave/brave-browser/wiki/Chromium-rebases/ac39f04cbe25b80bbf19f2cead5b38be1860f85a) | Requires substantive fixes and removals to be separate, links to upstream causes, broader behavior review, tests, and QA. | Review of accepted messages, defaults, persistence, proofs, and lifecycle transfers. Browser permission and network audits do not transfer unchanged. |

The [newer Plaster guidance](https://github.com/brave/brave-core/blob/e0c4db7f4db5fcbd0eaf55b5f9bbc945bb48233b/docs/best-practices/plaster.md#L84)
prohibits new macros that edit upstream and prefers transformations. Older override advice is not one
timeless prescription. Via would need demonstrated recurring conflicts before adapting such machinery.

### What Via will probably need on ZKsync OS (provisional)

The [Via-owned responsibilities](refork-target-and-upstream-direction.md#what-stays-via-owned-on-any-target)
remain the baseline. The additions suggested by this research are provisional and can change with the target:

- Semantic input and expected-decision corpora that survive different runtime types, with completeness
  checks for pending obligations. Bedrock shows why witness capture alone is insufficient.
- Explicit compatibility claims for historical decoding, state conversion, proof verification, and
  proof-serving availability. Scroll and Celo show that a working replacement can change each differently.
- Old-format state fixtures and rehearsals covering interrupted migration, stale locks, retries,
  duplicate processing, restart, and recovery. Brave shows why imported-value tests are insufficient.
- Measured adapters only when a working consumer exists, with performance and removal criteria.
  Stylo and the Merge show both the value and rework of concrete coexistence.
- A decision about portable Via owners and their supported dependency graphs, separate from copy
  detection. The current manifest evidence does not justify declaring every shared crate portable.

These are adaptations of the cited mechanisms, not unchanged ports or accepted designs. Offline
fixtures and local checks are reversible. Changed wire formats, activated protocol rules, and committed
state constrain rollback. No saving in Via porting effort has been measured.

The evidence supports no exception to the recorded
[no ZKsync OS-specific work before the exit conditions](refork-target-and-upstream-direction.md#what-current-via-core-work-does-meanwhile)
rule. Public target protocols and working consumers made the positive examples possible. They do not
justify an OS adapter before Via's existing conditions hold.

## Limits

- This condenses source inspection, not executed donor tests, migrations, or local cargo-deny validation.
- Release and issue reports do not establish total engineering cost, complete coverage, or current
  operational success. No numerical Via savings estimate follows from their diff sizes.
- No unchanged transfer of Celo's or Scroll's old test files, live Scroll shadow prover, or general
  target-neutral Muon abstraction was established. Those are bounded findings.
- The public OS cohort and publication assessment is reused from the target note, not re-audited here.

## Open questions

- Which Via crates should be portable, under which features, targets, and dependency kinds?
- Would named bans suffice, or would an approved closed layer require classification and exceptions?
- Which touched behaviors already have complete reproducible inputs, and which need additional evidence?
- Once the existing exit conditions hold, which state, history, and obligations must the bounded
  vertical spike preserve, transform, or explicitly retire?
