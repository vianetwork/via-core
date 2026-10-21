# Duplication detection with jscpd

Status: research, 2026-10-20, extended 2026-10-21. The redesign of via-core's duplication tooling is tracked in the
wayfinder map [issue #410](https://github.com/vianetwork/via-core/issues/410). Each decision is recorded
on its ticket and summarized under "Decisions and open proposals". Other recommendations below are
proposals. The current gate is unchanged until the build lands.

Last verified: 2026-10-21.

## Question

How should via-core detect duplicated code on current jscpd so that it:

1. measures copies between `core/` and `via_verifier/`, the debt that
   [issue #382](https://github.com/vianetwork/via-core/issues/382) wants gone, since it requires the
   refork to use one implementation per behavior;
2. never fails a pull request because of duplication that already existed, while old duplication
   shrinks over time;
3. serves the command line, CI, and coding agents from one definition;
4. copies established practice instead of inventing configuration?

## Source revisions

| Source | Revision |
| --- | --- |
| via-core `main` | `5e837ac2911b489d953d7c96e31cd85dc23577f4`; measurement at `eec4bb8f59fff0c23ddc9cdfa21910882ca36be6` |
| jscpd v5.4.1, the measured release (2026-10-09) | `095d6ca33e9f291c144bb05ef653b2bfdc3c1453` |
| jscpd v5.4.2, released 2026-10-20; used for the scan scope measurements and the wrapper fixtures | `db415b5d7cc34d72a324fe5eae2147646dc3558b` |
| via-core `main`, scan scope measurements | `8c7246001` |
| PostHog / Telekom Sutura / Brave | `10e4218b671e8cf967c79993ba8607dae2f6eae7` / `1a33166e0042f4e5843be1b1634fc0a5de763413` / `279f12a02cbfd9d2b340ce1a3c248e63c808e87b` |
| jscpd `master` | `854df3d40a4317e71ffd4e03474b4f812e533184` (2026-10-19) |
| zksync-era `main` | `ff5f519b11cff863edcfa0f75af10fea113806b0` |
| zksync-os `v0.5.x-private` / zksync-os-server `main` | `5610fb622832acbdf4feeb0ae458a5c3436b2b6c` / `e29a2c0a584b7f3f985e9a414bfbe179456c093d` |

jscpd.dev pages and vendor documentation are moving documents, accessed 2026-10-20. Donor projects
are cited at pinned revisions where the source was inspected.

## Findings

### The current gate

- `.github/scripts/check-via-duplication.py` wraps the jscpd 5.0.12 binary with
  `--format rust --mode weak --min-lines 1 --min-tokens 50 --skip-local --reporters json`. It scans
  `core/node` against `via_verifier/node` and `core/lib` against `via_verifier/lib`, and sums
  duplicated tokens per file pair.
- `.github/lint/via-structural/duplication/baseline.tsv` holds a token budget for each of 38 pairs.
  `allowlist.tsv` holds only its header.
- `.github/workflows/via-structural-rules.yml` installs a SHA-256-checked release archive and reads
  the budgets from the pull request's base commit. A pair that grows above its budget fails.

Defects verified locally:

| Defect | Consequence |
| --- | --- |
| The TSV header embeds `jscpd=5.0.12`, checked against the base commit's copy | A jscpd upgrade cannot pass CI |
| `core/bin` and `via_verifier/bin` are not scanned | Two pairs between `via_server` and `verifier_server` (297 tokens) go unseen |
| When a pair shrinks, the script prints a note and keeps the old budget | Budgets never tighten; debt can return up to the old ceiling |
| The allowlist is parsed but not consulted in ratchet mode | It cannot exempt anything |
| An empty scan passes; `--fail-on-empty` (5.2.1+) is unused | A broken scan path looks green |
| jscpd 5.0.12 predates boundary fixes in 5.2.1 and 5.3.2 | Clone edges differ from current releases |
| The `Main` ruleset requires no status checks | The gate only advises today |

### What jscpd 5.4.1 measures on via-core

Measured on a study clone of via-core at `eec4bb8f5` with the v5.4.1 release binary (archive checksum
verified against the release `checksums.txt`). Exact clones unless noted:

| Run | Clones | Duplicated tokens |
| --- | ---: | ---: |
| Today's two scans (node, lib) | 90 | 9,845 |
| One scan of all `core/` against all `via_verifier/` | 93 | 10,142 |
| Renamed copies (`--ignore-identifiers`, 50 tokens) | 230 | 19,767 |
| Gap merging (`--max-gap-lines 3`) | 69 records | 9,954 |
| Look-alike functions (`--similarity 0.8`) | 198 | 15,301 |

Changing only the jscpd version under today's budgets would fail two pairs (+62 and +70 tokens),
although total debt drops by 757 tokens. `--semantic` and `--compare` were not run; they need a
548 MB model. Rust `--dead-code` reads `cargo check --message-format=json` output through
`--rust-diagnostics`; jscpd does not run Cargo itself.

`boundaries` is not in v5.4.1: its configuration parser ignores the key with an "unknown field"
warning. The feature was merged to `master` afterwards
([PR #1227](https://github.com/kucherenko/jscpd/pull/1227)) and shipped in
[v5.4.2](https://github.com/kucherenko/jscpd/releases/tag/v5.4.2) on 2026-10-20, whose release notes
also change identifier matching and similarity. The 5.4.1 totals above, including the renamed-copy
and look-alike runs, do not describe 5.4.2. v5.4.2 keeps the same baseline fingerprint function.

The classification of these pairs by what survives a refork is in
[Duplication paydown and the refork](duplication-paydown-and-refork.md).

### jscpd's native baseline treats edited old clones as new

jscpd 5.1 added `--baseline`, `--update-baseline`, `--baseline-from-ref <ref>` and
`--fail-on-new-clones` ([baseline guide](https://jscpd.dev/guides/baseline)). A clone counts as new
when its fingerprint is absent from the baseline. The fingerprint hashes the raw text of both
snippets and removes only carriage returns:

```rust
pub fn snippet_pair_hash(a: &str, b: &str) -> u64 {
    let a = strip_cr(a);
    let b = strip_cr(b);
    let (first, second) = if a <= b { (&a, &b) } else { (&b, &a) };
```

Source: `kucherenko/jscpd@854df3d40a4317e71ffd4e03474b4f812e533184`, `rust/crates/cpd-core/src/hash.rs`
(unchanged from v5.4.1). The baseline and SARIF `partialFingerprints` use the same value
(`rust/crates/cpd-reporter/src/shared.rs`, `identity_texts`; `baseline.rs`, `compute_fingerprints`).
The design discussion, [issue #944](https://github.com/kucherenko/jscpd/issues/944), chose
content-only fingerprints so that renames do not fire. Edits inside a clone are not discussed there.

Behavior of `--baseline-from-ref <base> --fail-on-new-clones` on v5.4.1, with a two-file Rust fixture:

| Change on the branch | Result |
| --- | --- |
| Unrelated edit; old clone's file moved on the branch or on the base | passes |
| Line-ending change only | passes |
| The same bug fix applied to both copies of an old clone | fails |
| A fix applied to one copy only | fails |
| Spaces, a comment, or a blank line inside an old clone | fails |
| An edit that shrinks an old clone | fails |
| Base removed a clone after the branch split; branch unchanged | fails |
| A third copy of an existing fragment | fails, correctly |
| A third copy inside `// jscpd:ignore-start` and `// jscpd:ignore-end` | passes |

These fixtures show why [issue #415](https://github.com/vianetwork/via-core/issues/415) uses the
native `isNew` flag to identify edited copies under the chosen policy. The wrapper still applies the
ownership rules of [issue #413](https://github.com/vianetwork/via-core/issues/413), including the
report-only treatment of edited old Via ↔ upstream copies.

With `--skip-local`, the flag today's gate uses, a third identical copy inside one scan root produced
no new cross-root pair and passed. A pair budget cannot gate a relation the detector does not report.

### How established tools decide what is new

Three families exist. Each keys an "existing finding" differently.

| Family | Identity of an existing finding | Examples | Edit inside an old finding | Tool upgrade |
| --- | --- | --- | --- | --- |
| Committed count per key | file or file pair, plus rule, with an allowed count | [ESLint bulk suppressions](https://eslint.org/docs/latest/use/suppressions), [PHPStan baseline](https://phpstan.org/user-guide/baseline), [Notion's ratchet](https://www.notion.com/blog/how-we-evolved-our-code-notions-ratcheting-system-using-custom-eslint-rules) and its open implementation [eslint-seatbelt](https://github.com/justjake/eslint-seatbelt/blob/145514953f604f7e178d5f0fbeed44d7eca2d300/src/SeatbeltFile.ts#L29-L95), [Betterer](https://phenomnomnominal.github.io/betterer/docs/results-file/) | passes while the count holds | counts move with the tool; the file must be regenerated |
| Live comparison with another revision | finding present in a scan of the other revision | [Semgrep `--baseline-commit`](https://docs.semgrep.dev/semgrep-ci/findings-ci), [golangci-lint `new-from-merge-base`](https://golangci-lint.run/docs/welcome/faq/), [PostHog's jscpd wrapper](https://github.com/PostHog/posthog/blob/25382eeac079624f0d9a20260b2cef03e869a279/bin/lint_duplication.py#L155-L297), [dupdelta](https://github.com/elecnix/dupdelta) | depends on the identity used | immune: one binary scans both sides |
| Fingerprint baseline | content or line hash per finding | jscpd's native baseline, [Qodana](https://www.jetbrains.com/help/qodana/baseline.html), [detekt](https://detekt.dev/docs/introduction/baseline/), [GitHub code scanning](https://docs.github.com/en/code-security/reference/code-scanning/sarif-files/sarif-support) | depends on the fingerprint and matching algorithm. jscpd hashes raw text, so edits read as new. Qodana adds shift- and refactor-tolerant [matching phases](https://github.com/JetBrains/qodana-sarif/blob/a0bdfce5de4970b16d7c9a35ac26e1a0061dd069/sarif/src/main/java/com/jetbrains/qodana/sarif/baseline/Baseline.kt#L100-L118). A reported GitHub case closed and reopened an alert after its line was edited ([codeql-action #986](https://github.com/github/codeql-action/issues/986)). | hash changes invalidate entries |

Facts that shape the choice:

- ESLint tracks suppressions per file and rule on purpose, because it cannot tell which of several
  violations in a file is new. A fix and a regression of equal size in one key cancel out; this is
  an open issue ([ESLint #21226](https://github.com/eslint/eslint/issues/21226)) that applies to any
  count-per-key design, including a per-pair token budget.
- A fix applied to both copies can still grow a pair. Changing `>` to `>=` in both copies of a
  108-token clone produced 109 tokens; adding one line to both produced 117.
- Comparing a branch with its merge-base attributes changes to the branch, but it does not keep the
  main branch's debt shrinking. A stale branch can replace a clone that `main` already removed with a
  different clone of the same size, merge cleanly, and pass. Comparing the candidate merge with the
  current target catches it. In a `pull_request` workflow, `actions/checkout` checks out that merge
  commit, and its first parent is the base at run time.
- Semgrep maps renamed paths back to their old names through Git before comparing
  ([rule identity](https://github.com/semgrep/semgrep/blob/51602ad27f09ef9342a5620601de5ef8f572fbc6/cli/src/semgrep/rule_match.py#L226-L265),
  [baseline matching](https://github.com/semgrep/semgrep/blob/51602ad27f09ef9342a5620601de5ef8f572fbc6/cli/src/semgrep/run_scan.py#L409-L475)).
  It treats a failed baseline scan as unable to prove a finding new, not as an empty baseline.
- No tool found uses an automatic numeric tolerance for growth of an existing finding.
- Percentage gates dilute in large repositories: a 20-line copy moved one repository from 3.27% to
  3.28% ([receptron/mulmoclaude workflow](https://github.com/receptron/mulmoclaude/blob/ef44fc8991012cb55cf931b0dca6316a6b6fbf19/.github/workflows/duplication-scan.yaml)).
- Reporting old duplication in the files a pull request touches is an established advisory pattern.
  Infinispan keeps every CPD duplication that involves a changed file and posts it to the pull
  request ([filter](https://github.com/infinispan/infinispan/blob/1d74b104c5678904d0c04ff8f9ec736ccad000a2/.github/scripts/filter-cpd-by-changed-files.py#L40-L77)).
  SonarQube counts duplicated lines on new or modified code
  ([new code](https://docs.sonarsource.com/sonarqube-server/user-guide/about-new-code)).

Duplication-specific gates found: SonarQube's new-code duplication condition, Codacy's clone-count
gate, PMD CPD's `maxAllowedViolations` with no new-only mode
([cpd-check](https://maven.apache.org/plugins/maven-pmd-plugin/cpd-check-mojo.html)), and small
repositories with hand-lowered count or percentage ratchets. This investigation did not find an
established project with the setup via-core needs, a blocking Rust gate between two source trees that
ignores old duplication, so it did not identify a 1:1 donor.

### A committed TSV budget is mainstream, not a hack

Notion moved its ratchet file from JSON to TSV on purpose: parallel fixes to the JSON file "would
conflict on whitespace, delimiters, and commas", while one line per file and rule keeps entries
independent. Raising a count needs approval, counts drop automatically, and Notion accepts that moving
a file forces a path update ([Notion engineering, 2025-03-03](https://www.notion.com/blog/how-we-evolved-our-code-notions-ratcheting-system-using-custom-eslint-rules)).
Cesium uses the open implementation
([eslint.config.js](https://github.com/CesiumGS/cesium/blob/621826db344eb7f1248f6bd50ed4770563127f12/eslint.config.js#L66-L85)).
The format matters less than the key. The specific weakness for duplication is that token counts move
whenever jscpd changes clone boundaries, while lint counts barely move between releases. A committed
token budget therefore needs a pinned jscpd and a regeneration commit for each upgrade. A live
comparison scanned by one binary does not.

### Upstream has no duplication gate

zksync-era `main`, zksync-os, zksync-os-server and era-contracts have no duplication gate and no
baseline or ratchet for any lint. Clippy and formatting fail the whole workspace with `-D warnings`
([Era lint runner](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/zkstack_cli/crates/zkstack/src/commands/dev/commands/lint.rs#L70-L95),
[OS server CI](https://github.com/matter-labs/zksync-os-server/blob/e29a2c0a584b7f3f985e9a414bfbe179456c093d/.github/workflows/ci.yml#L69-L74)).
Exceptions are inline `#[allow(...)]` attributes and `deny.toml` entries with an identifier and a reason
([Era deny.toml](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/deny.toml#L9-L50)).
ZKsync OS CI calls plain `cargo`, without the `zkstack dev lint` wrapper.

Consequence (inference): the duplication gate is Via-only tooling. Keep it in Via-owned paths and
call jscpd and plain tools directly, so a refork can drop or re-point it without touching inherited
code. Inline exceptions match the upstream habit better than a central file.

### Agents, skills and CI integration

- jscpd ships an MCP server that caches scan results and can refresh them with
  `check_current_directory` ([MCP reference](https://jscpd.dev/api/mcp-server)), an `ai` reporter,
  and five agent skills at `kucherenko/jscpd@854df3d`. The skills invoke unversioned `npx jscpd` with
  generic examples rather than Via's wrapper, so agents would see findings that disagree with the
  gate. The `dry-refactoring` skill defaults to
  extracting and updating all call sites; merging `core/` and verifier copies is protocol-sensitive
  design work.
- Project-scoped skill installs land in `.agents/skills/`
  ([vercel-labs/skills commands](https://github.com/vercel-labs/skills/blob/a80e4c9d12d607631249d4d7d059f835d4b2f13a/docs/commands.md)),
  which this repository's `.gitignore` excludes. Telemetry is on by default
  ([troubleshooting](https://github.com/vercel-labs/skills/blob/a80e4c9d12d607631249d4d7d059f835d4b2f13a/docs/troubleshooting.md)).
- The only post-edit agent hook found is a Cursor hook in zowe-mcp
  ([commit d786ff1](https://github.com/zowe/zowe-mcp/commit/d786ff16bba57b65a6506699a318960f91805a68)).
  No Claude Code or Codex jscpd hook was found.
- The GitHub Action `kucherenko/jscpd@v5` defaults its `version` input to `latest`, and MegaLinter
  overrides jscpd's exit code ([issue #2450](https://github.com/oxsecurity/megalinter/issues/2450)).
  Wrapper arguments can defeat a configured gate.
- The `jscpd` crate needs Rust 1.97; this workspace uses `nightly-2024-08-01` (rustc 1.82). The
  binary with its JSON reporter is the supported interface for callers outside Rust.

### Fork divergence is a different measurement

op-geth measures its divergence from go-ethereum as a per-topic diff against a pinned upstream hash
([fork.yaml](https://github.com/ethereum-optimism/op-geth/blob/a160d1eac9d14b6f80dcfac94c6a48b5316ba8db/fork.yaml),
[forkdiff](https://github.com/protolambda/forkdiff)). Clone detection answers a different question:
which Via code is a modified copy of upstream code. Neither proves behavioral equivalence.

### Scan scope: measurements and donors

jscpd 5.4.2 on via-core `8c7246001`, exact copies, 50 tokens, `--mode weak`, without `--skip-local`.
A path is Via-owned when it sits under `via_verifier/` or `via_indexer/`, or has a path segment that
starts with `via_`.

| Pair type | Clones | Tokens |
| --- | --- | --- |
| `core/` ↔ `via_verifier/` | 92 | 10,037 |
| Via ↔ Via inside one tree | 371 | 33,433 |
| Via fork ↔ upstream original, inside `core/` | 373 | 126,194 |
| Upstream ↔ upstream, inside `core/` | 2,357 | 358,672 |
| `via_indexer/` ↔ other trees | 24 | 1,767 |

- The current gate never scans `via_indexer/`, and it only pairs `core/{node,lib}` with
  `via_verifier/{node,lib}`.
- Most Via ↔ upstream tokens come from Via forks of upstream crates: `via_state_keeper` ↔
  `state_keeper` (60k), `via_loadnext` ↔ `loadnext` (24k), `via_external_node` ↔ `external_node`
  (16k).
- Of the 448 Via ↔ Via copies, 93 sit in test files and 135 in inline `#[cfg(test)]` modules.
  jscpd's exact pass treats inline test modules as ordinary code
  ([tokenizer](https://github.com/kucherenko/jscpd/blob/db415b5d7cc34d72a324fe5eae2147646dc3558b/rust/crates/cpd-tokenizer/src/tokenizer.rs#L805-L836));
  its AST, semantic and compare passes recognize them.
- Thresholds: moving from 1 to 5 lines changes the cross-tree count from 92 to 91. 70 tokens gives
  51, and 100 tokens gives 37.

What established projects do:

- [Telekom Sutura](https://github.com/telekom/sutura/blob/1a33166e0042f4e5843be1b1634fc0a5de763413/xtask/src/jscpd.rs#L40-L48),
  a Rust project, blocks CI on jscpd. It scans the root with `--format rust`, includes tests, and
  uses 250 tokens and 30 lines, chosen so the check started green.
- [PostHog](https://github.com/PostHog/posthog/blob/10e4218b671e8cf967c79993ba8607dae2f6eae7/bin/lint_duplication.py#L3-L46)
  wraps jscpd in a script. It scans the root with `--skip-comments` and uses 70 tokens and 10 lines,
  or [150 tokens](https://github.com/PostHog/posthog/blob/10e4218b671e8cf967c79993ba8607dae2f6eae7/bin/lint-duplication.limits.json#L1-L4)
  when both copies are tests. Its CI step is advisory.
- jscpd defaults to 50 tokens, 5 lines and `mild`, which keeps comments. `weak` drops comments
  ([tokenizer](https://github.com/kucherenko/jscpd/blob/db415b5d7cc34d72a324fe5eae2147646dc3558b/rust/crates/cpd-tokenizer/src/tokenizer.rs#L204-L233)).
  PMD CPD drops comments for Rust. SonarQube uses 100 tokens and 10 lines for most languages, with
  exceptions such as Java ([metric definitions](https://docs.sonarsource.com/sonarqube-server/user-guide/code-metrics/metrics-definition)),
  and it [does not measure duplication on test code](https://docs.sonarsource.com/sonarqube-server/quality-standards-administration/managing-quality-gates/introduction-to-quality-gates).
- In a sample of 15 committed jscpd configurations found by GitHub code search on 2026-10-20, most
  keep the default size and 10 exclude tests. The blocking gates verified in that sample use a
  whole-run percentage, and none uses jscpd's baselines. Sutura, outside that sample, gates on
  individual findings with a reasoned allowlist.
- No inspected fork runs a duplication gate that separates fork code from upstream code. Brave keeps
  [lint off vendored upstream code](https://github.com/brave/brave-core/blob/279f12a02cbfd9d2b340ce1a3c248e63c808e87b/eslint.config.js#L113-L130)
  and its [patching guide](https://github.com/brave/brave-core/blob/279f12a02cbfd9d2b340ce1a3c248e63c808e87b/docs/patching_and_chromium_src.md#L118-L120)
  discourages copying whole upstream files. op-geth and BSC lint their whole workspace and enable no
  duplication linter.

## Decisions and open proposals

The gate model was decided on 2026-10-20 in [issue #412](https://github.com/vianetwork/via-core/issues/412):
touch it, fix it. A pull request fails when it adds a new copy, or when it changes an existing copy
that still exists afterwards. Copies in untouched code are never checked. Issue #413 limited blocking
by ownership, and issue #415 defines the touched-copy test with jscpd's `isNew` flag rather than Git
line overlap. Intentional
copies carry an ignore marker with a reason; urgent fixes use a `duplication-deferred` label. Every
pull request gets a comment listing old copies in the files it touched. The issue's resolution
comment holds the full decision, the options considered, and the technical defaults.

Scan scope and detection settings were decided on 2026-10-21 in
[issue #413](https://github.com/vianetwork/via-core/issues/413):

- Scan the repository root with `--format rust`, ignoring `**/generated/**` and `target/`.
- Via ↔ Via copies block when new or touched. A new Via ↔ upstream copy blocks; a touched old one is
  only listed in the comment. Upstream ↔ upstream copies are never checked. No donor was found for
  this ownership rule in the inspected projects.
- Tests count like production code, as in Sutura.
- jscpd's default size of 50 tokens and 5 lines, with `mode: weak`.
- Exact copies can block. Renamed copies are listed in the comment. Gap, AST and semantic matching
  stay out of CI.
- Two exact-only `boundaries` rules run as a separate, non-blocking report: `core/lib/via_reorg/src/**`
  and `core/lib/via_btc_client/src/**` may not be copied into `core/node/**` or `via_verifier/**`.
  Each rule disables `renamed`, `gap`, `ast` and `semantic` itself, because a global `--kind exact`
  does not constrain boundaries.
  `boundaries` cannot be grandfathered natively, so it stays out of the blocking gate.

Installation was decided on 2026-10-21 in [issue #414](https://github.com/vianetwork/via-core/issues/414):

- CI, local runs and agents all run `npx --yes jscpd@latest`. The npm package installs the prebuilt
  binary for the platform. Node 20 is already a setup requirement. The pinned 5.0.12 archive
  download leaves CI.
- jscpd uses the latest release by default. Each job resolves one jscpd binary and uses it for both
  scans, so the comparison never mixes detector versions.
- A run stops below 5.4.2, because the selected boundary rules need that release. The earlier corpus
  and baseline measurements used 5.4.1; the scan scope measurements used 5.4.2. jscpd has no
  minimum-version setting, so this is one version comparison in via-core's tooling.
- The minimum rejects the stale cached 5.3.0 that jscpd's author reported from a bare `npx jscpd`
  ([write-up](https://hackernoon.com/ai-code-refactoring-doesnt-have-to-be-a-guessing-game)). It
  does not prove that a version at or above the minimum is the newest.
- Rejected:
  - The jscpd GitHub Action. It installs through an unverified
    `curl https://jscpd.dev/install.sh | bash` and defaults to `latest`
    ([action.yml](https://github.com/kucherenko/jscpd/blob/db415b5d7cc34d72a324fe5eae2147646dc3558b/action.yml#L152-L237)).
  - `cargo install`. jscpd 5.4.2 declares Rust 1.97, newer than the workspace's
    `nightly-2024-08-01` toolchain.
  - `cargo binstall`, `uvx` or pip. The maintainer chose the existing Node and npm setup instead of
    a separate jscpd installation.
  - A signature-checked release archive in CI. CI would install jscpd differently from local runs.
    The structural rules workflow grants only `contents: read`, disables persisted checkout
    credentials, and passes no project secrets to the scanner.

The wrapper was decided on 2026-10-21 in [issue #415](https://github.com/vianetwork/via-core/issues/415):

- The decision keeps a small Rust tool at `via_duplication/`, with its own Cargo workspace, lockfile
  and stable toolchain file, outside the product workspace. Callers select stable explicitly, because
  rustup picks the toolchain from the current directory, not from `--manifest-path`. The layout
  follows [Agave's `ci/xtask/`](https://github.com/anza-xyz/agave/blob/910af6bed4ad4f3a946f856da2a5b0618ea2f909/ci/xtask/Cargo.toml#L1-L13)
  and upstream's separate `zkstack_cli/` and `prover/` workspaces.
- jscpd does detection, the comparison with the base revision (`--baseline-from-ref` and the JSON
  `isNew` flag) and ignore markers. The tool does only what jscpd 5.4.2 cannot: ownership classes,
  Via ↔ upstream new versus old (whether the two files shared a copy in the base scan), the 5.4.2
  minimum, telling `boundaries` findings from a failed scan, and the PR comment body. It exits 0
  when the blocking check passes, 1 for blocking duplication and 2 for an error in the blocking
  check; a failed advisory boundary scan is reported without changing that result.
- Native `--baseline-from-ref` supplies `isNew` but does not export the base scan's file pairs. The
  wrapper still needs those pairs for the ownership classes, and the PR's changed paths for the
  old-copy comment. It does not parse Git hunks for the touched-copy test.
- On the 5.4.2 fixtures, line shifts, pure file renames and LF/CRLF changes kept old copies old.
  The tested whitespace, comment, code and shrinking edits marked surviving copies `isNew: true`,
  and a third identical copy produced a new occurrence.
- Gate-critical values in jscpd's JSON are decoded as required, non-optional Serde fields without
  defaults. Missing or renamed fields are tool errors; unknown extra fields are ignored. Go was rejected because
  `encoding/json` fills missing fields with zero values; Python because the same guarantees need
  extra discipline or dependencies. [Sutura's jscpd module](https://github.com/telekom/sutura/blob/1a33166e0042f4e5843be1b1634fc0a5de763413/xtask/src/jscpd.rs#L388-L477)
  is prior art for subprocess and temporary-directory handling. Its defaults for missing or invalid
  `lines` and `tokens` are not copied.
- Add a structural rule that rejects `jscpd:ignore-start` without a reason; jscpd itself accepts
  markers with or without one.

The shared configuration and commands were decided on 2026-10-21 in
[issue #416](https://github.com/vianetwork/via-core/issues/416):

- The settings live in `via_duplication/jscpd.json`, which the tool passes with `--config`. The two
  `boundaries` rules live in `via_duplication_boundaries.json` at the repository root. jscpd checks
  `boundaries` on every scan that reads a config and exits 1 on findings, and configs cannot include
  each other, so the second file repeats the few base settings. jscpd 5.4.2 matches boundary globs
  only below the config file's directory, so the boundaries file must sit at the root; the
  maintainer moved it there during the build. The name avoids `.jscpd.json`, which jscpd loads
  automatically.
- The modes are subcommands of the tool, each exposed by one flat `just` recipe:
  `via-dup-check` (the gate, which also prints the PR comment body), `via-dup-boundaries` and
  `via-dup-inventory`. Via's recipes select stable Rust and forward arguments; the pattern of one
  recipe per tool subcommand follows
  [rust-bitcoin's justfile](https://github.com/rust-bitcoin/rust-bitcoin/blob/53ee132ca2737466f942f8b7db33a9871bdd4f7b/justfile#L1-L41).
  The three recipes share the `via` help group, a first `default` recipe lists the commands, and
  argument forwarding needs `just` 1.29 or later. `via-check-strict` keeps running the gate.
- Locally, the gate compares the working tree with the merge-base of `HEAD` and `origin/main`. CI
  installs a pinned `just` and runs `just via-dup-check --base HEAD^1` on the PR merge commit.
  Reproducing a CI result locally needs that same clean checkout, jscpd version and environment.
  Installing a pinned `just` in CI and calling recipes follows
  [commonware's CI](https://github.com/commonwarexyz/monorepo/blob/7e9deea92bc977f088146e6ad9a3c0c96a901278/.github/workflows/fast.yml#L52-L71).

Agent use was decided on 2026-10-21 in [issue #417](https://github.com/vianetwork/via-core/issues/417):

- jscpd's agent skills and MCP server are not used. The skills invoke unversioned `npx jscpd` with
  generic examples rather than Via's wrapper, and `dry-refactoring` defaults to merging copies,
  which is design work for main-node and verifier code. The MCP server can refresh its scan, but it
  does not apply Via's ownership and blocking rules. Neither gives the gate's verdict.
- No hook runs after agent edits. Agents run the gate before handoff, to avoid repeated Cargo
  launches and repository-wide scans after each edit.
- AGENTS.md will require agents to run `just via-dup-check` for every Rust change, so the ownership
  and clone-kind rules of issue #413 apply across the scan scope. It will list the three recipes and
  the failure workflow: for blocking findings (exit 1), merge into the shared owner, mark an
  intentional copy with a reason, or stop and ask when the copies are main-node versus verifier or
  protocol-sensitive code; tool errors (exit 2) are diagnosed separately. Naming the exact
  check command follows
  [Sutura's AGENTS.md](https://github.com/telekom/sutura/blob/1a33166e0042f4e5843be1b1634fc0a5de763413/AGENTS.md#L38-L45).

CI reporting and rollout were decided on 2026-10-21 in
[issue #418](https://github.com/vianetwork/via-core/issues/418):

- At cutover, `Via Duplication` becomes a required check with no advisory trial period. It runs on every pull request, without path
  filters or conditional skips, because a path-filtered workflow leaves a required check pending
  and a skipped job reports success. Branches must be up to date before merging.
- Runs that reach reporting write a job summary with the jscpd version that ran, or "version not
  resolved" and an error if resolution failed. A separate `workflow_run`
  publisher with write access posts one sticky comment, so fork pull requests get comments too. It
  treats the uploaded report as untrusted data, following
  [GitHub Security Lab's guidance](https://securitylab.github.com/resources/github-actions-preventing-pwn-requests/)
  and the producer/publisher pair of
  [FastPFOR-rs](https://github.com/fast-pack/FastPFOR-rs/blob/ebca273acf1de7644bd01bcbd79b03c61c1348ed/.github/workflows/pr-comments.yml#L1-L67).
- A deferral needs the current `duplication-deferred` label, a reason line in the PR body, and an
  admin as the most recent applier of that label, checked through the API because GitHub lets triage users apply
  labels. It waives only blocking duplication (exit 1), never a tool error (exit 2).
- One cutover pull request replaces the Python gate, its baseline and allowlist, and adds the
  ignore-marker reason rule as a blocking step of `Via Duplication`.


Paydown was decided on 2026-10-21 in [issue #419](https://github.com/vianetwork/via-core/issues/419):

- Old copies shrink through "touch it, fix it", and through a short ranked list of planned merges
  kept as GitHub issues linked to [issue #382](https://github.com/vianetwork/via-core/issues/382).
  Duplicated correctness fixes come first. The ranking and completion rules follow the proposals in
  [duplication-paydown-and-refork.md](duplication-paydown-and-refork.md#proposals-for-the-paydown-policy).
  Each new shared crate also gets an exact-only advisory `boundaries` rule.
- The first `just via-dup-inventory` run after the cutover seeds the ranked list; later inventories
  run on demand without a committed scan baseline. Exceptions are inline markers with a reason.
- Trend tracking over time with `--history` is out of scope.

## Limits

- The baseline probes used synthetic fixtures and the released v5.4.1 binary; the wrapper fixtures
  used v5.4.2. Those tested file baselines and checked the shared ref-baseline code in source; they
  did not run `--baseline-from-ref`. No donor test suite was run.
- Several adopter claims rest on search results or partial page reads; claims tied to a pinned
  source were checked against that source.
- `--semantic`, `--compare`, and `--history` were not measured on via-core.
- The Via-owned path rule is a heuristic. Via changes inside upstream files count as upstream.
- Many adopter configurations were not traced to an active CI step, and required-check settings were
  not audited.
- No Claude Code or Codex jscpd integration precedent was found; the absence is a bounded finding.

## Open questions

None for the tooling design. All decisions of [issue #410](https://github.com/vianetwork/via-core/issues/410)
are made; the build follows an ExecPlan.
