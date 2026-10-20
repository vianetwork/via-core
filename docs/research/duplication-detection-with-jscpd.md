# Duplication detection with jscpd

Status: research, 2026-10-20. The redesign of via-core's duplication tooling is tracked in the
wayfinder map [issue #410](https://github.com/vianetwork/via-core/issues/410). Decisions are recorded
on its tickets, not here. Recommendations below are proposals unless a ticket says otherwise. The
current gate is unchanged.

Last verified: 2026-10-20.

## Question

How should via-core detect duplicated code on current jscpd so that it:

1. measures copies between `core/` and `via_verifier/`, the debt that
   [issue #382](https://github.com/vianetwork/via-core/issues/382) requires removing before a refork;
2. never fails a pull request because of duplication that already existed, while old duplication
   shrinks over time;
3. serves the command line, CI, and coding agents from one definition;
4. copies established practice instead of inventing configuration?

## Source revisions

| Source | Revision |
| --- | --- |
| via-core `main` | `5e837ac2911b489d953d7c96e31cd85dc23577f4`; measurement at `eec4bb8f59fff0c23ddc9cdfa21910882ca36be6` |
| jscpd v5.4.1, the measured release (2026-10-09) | `095d6ca33e9f291c144bb05ef653b2bfdc3c1453` |
| jscpd v5.4.2, released 2026-10-20 after the measurements; not measured | `db415b5d7cc34d72a324fe5eae2147646dc3558b` |
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

The first two failures conflict with this repository's sibling rule, which asks that a fix reach both
copies. The native baseline therefore cannot be the gate by itself.

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

- jscpd ships an MCP server whose answers come from its startup scan
  ([MCP reference](https://jscpd.dev/api/mcp-server)), an `ai` reporter, and five agent skills at
  `kucherenko/jscpd@854df3d`. The skills call `npx jscpd` with default flags and no configuration, so
  agents would see findings that disagree with the gate. The `dry-refactoring` skill defaults to
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

## Proposals and recorded answers

Gate model, [issue #412](https://github.com/vianetwork/via-core/issues/412), open at the time of
writing:

- Recorded maintainer answer (2026-10-20): a pull request fails only when two files that shared no
  duplication now do. Growth of an existing pair is reported with exact numbers and never blocks.
- Proposed: scan the candidate merge and its first parent in one job with the same jscpd binary;
  delete the committed `baseline.tsv`; map renamed paths through Git one-to-one; treat a failed or
  missing base scan as an error, never as an empty baseline; allow deliberate copies through
  `jscpd:ignore` markers with a reason comment. `boundaries` shipped in v5.4.2 after this research;
  whether it belongs in the gate needs an evaluation of the released implementation.
- Proposed: report every clone that touches a file the pull request changes, old ones included, so
  the author can choose to pay it down while working there. Whether this is a pull request comment or
  a job summary, and whether the gate becomes a required check, belongs to
  [issue #418](https://github.com/vianetwork/via-core/issues/418).

## Limits

- The probes used synthetic fixtures and the released v5.4.1 binary. No donor test suite was run.
- Several adopter claims rest on search results or partial page reads; claims tied to a pinned
  source were checked against that source.
- `--semantic`, `--compare`, and `--history` were not measured on via-core.
- No Claude Code or Codex jscpd integration precedent was found; the absence is a bounded finding.

## Open questions

The remaining decisions are the open tickets of [issue #410](https://github.com/vianetwork/via-core/issues/410):
scan scope and detection settings, installation and invocation, whether any wrapper remains, the
shared configuration, how agents use jscpd, CI reporting and rollout, and how old duplication gets
paid down.
