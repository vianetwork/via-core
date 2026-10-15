# Formatting changed Via lines

Via Rust code uses `max_width = 100` and `use_small_heuristics = "Max"`, with the default Tall parameter layout.
Calls and method chains can stay on one line when they fit within 100 columns. This keeps new code compact for
review and physical-line audit counts without widening side-by-side diffs.

The profile covers all Rust files under `via_verifier/` and `via_indexer/`, plus crates whose directory names start
with `via_` under `core/`. A `via_` filename inside an upstream crate does not bring that crate into scope.
Each core Via crate has the same `rustfmt.toml`; the verifier and indexer each use one root config.

## Local commands

Requires Bash, Git, jq, rustup, and the rustfmt component for the channel in `rust-toolchain`.
The script explicitly selects that pinned toolchain, including when `RUSTUP_TOOLCHAIN` is set in the shell.

```bash
# Check all required profiles, including newly added Via crates.
scripts/format-changed.sh --check-config

# Format changes against HEAD, including staged, unstaged, and untracked files.
scripts/format-changed.sh

# Check without changing files.
scripts/format-changed.sh --check

# Format every change on the branch, including already committed changes.
git fetch origin main
scripts/format-changed.sh --base origin/main

# Run the same comparison as pull-request CI.
scripts/format-changed.sh --check --base origin/main

# Select staged changes. Re-stage formatted files before committing.
scripts/format-changed.sh staged
```

`--base REF` compares the working tree against `git merge-base REF HEAD`, so it finds committed PR changes even
in a clean checkout. Replace `origin/main` with the PR target branch when needed. Without `--base`, the reference
is `HEAD`. Staged mode uses the index diff and rejects selected files with unstaged changes: index line numbers
cannot safely identify a different working-tree statement. Use working-tree mode or stage the complete file first.

A failed check prints rustfmt's diff, the selected file and line ranges, and the command to fix them. The script
never stages files automatically. Review the diff after formatting.

## Changed lines and existing code

The script reads added and changed line ranges from a zero-context Git diff and passes only those files and ranges
to rustfmt's `--file-lines`. Old tall code outside the ranges stays as it is. Deleted-only hunks have no new lines to
format. New files, including untracked files, are formatted whole. Git-detected renames retain their old-line history;
a rename alone does not reformat the destination. Non-Via files are ignored, including files inherited from zkSync Era.

Do not run whole-file formatters on Via paths. `cargo fmt`, `zkstack dev fmt` without `--check`, and editor formatting
rewrite existing 120-column code under the new 100-column profiles. Use the changed-line script to preserve untouched
lines, blame, and unrelated PR diffs.

New files use the workspace import settings: `imports_granularity=Crate` and `group_imports=StdExternalCrate`.
Existing files preserve import grouping and order. With the pinned rustfmt, selecting one added `use` line can merge
and reorder untouched imports in the same group, even with `--file-lines`; default import sorting also crosses that
boundary. Both check and write modes therefore disable import merging, regrouping, and sorting for existing files.

`--file-lines` is unstable and needs `--unstable-features`. The script also disables child-module traversal. A
statement only partly covered by the diff may stay unchanged; rustfmt determines which syntax can be formatted
within the selected range. This check is a changed-range gate, not proof that an entire modified statement or file
has the compact style. Re-run the fixture tests whenever `rust-toolchain` changes:

```bash
bash scripts/tests/format-changed.sh
```

## CI and other Rust checks

The **Via Formatting** pull-request workflow runs on `ubuntu-latest`. It installs the pinned rustfmt, checks profile
coverage, runs the temporary-repository tests, and checks changed lines against the PR base branch. No Rust workspace
build is needed. Missing or divergent core profiles and nested overrides fail coverage, so new Via crates cannot
silently use defaults. Copy the three-line profile from `via_verifier/rustfmt.toml` into a new core Via crate.

For Via formatting, use the changed-line commands above. For upstream-owned paths, keep `zkstack dev fmt --check`;
its findings on existing untouched Via files are expected because their old layout differs from the new profiles.
This script does not replace Rust lint, crate tests, or structural checks.
