#!/usr/bin/env bash
set -euo pipefail

source_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

commit() {
  git -c core.hooksPath=/dev/null -c commit.gpgsign=false commit -qm fixture
}

fixture() {
  mkdir "$tmp/$1"
  cd "$tmp/$1"
  git init -q --initial-branch=main
  git config user.name "$(git -C "$source_root" config user.name || echo 'Formatter fixture')"
  git config user.email "$(git -C "$source_root" config user.email || echo 'formatter-fixture@example.invalid')"
  mkdir -p scripts via_verifier/src via_indexer core/lib/via_fixture/src core/lib/upstream/src
  cp "$source_root/scripts/format-changed.sh" scripts/
  cp "$source_root/rust-toolchain" .
  cp "$source_root/via_verifier/rustfmt.toml" via_verifier/
  cp "$source_root/via_indexer/rustfmt.toml" via_indexer/
  cp via_verifier/rustfmt.toml core/lib/via_fixture/
  printf '[package]\nname = "via_fixture"\nversion = "0.1.0"\n' > core/lib/via_fixture/Cargo.toml
  cat > via_verifier/src/lib.rs <<'RS'
fn old() {
    let value = things
        .iter()
        .copied()
        .collect::<Vec<_>>();
}
RS
  cp via_verifier/src/lib.rs "$tmp/old"
  git add -- scripts rust-toolchain via_verifier via_indexer core
  commit
}

tall() {
  cat <<'RS'
fn added() {
    let result = things
        .iter()
        .copied()
        .collect::<Vec<_>>();
}
RS
}

compact() {
  cat <<'RS'
fn added() {
    let result = things.iter().copied().collect::<Vec<_>>();
}
RS
}

fail_check() {
  if scripts/format-changed.sh --check "$@" > "$tmp/check.log" 2>&1; then
    echo 'Expected formatting check to fail.' >&2
    exit 1
  fi
  grep -F 'Fix locally: scripts/format-changed.sh' "$tmp/check.log"
}

fixture changed
printf '\nfn added() {\n    let result = 1;\n}\n' >> via_verifier/src/lib.rs
git add -- via_verifier/src/lib.rs
commit
cp "$tmp/old" via_verifier/src/lib.rs
printf '\n' >> via_verifier/src/lib.rs
tall >> via_verifier/src/lib.rs
cp via_verifier/src/lib.rs "$tmp/before"
fail_check
cmp "$tmp/before" via_verifier/src/lib.rs
scripts/format-changed.sh
{ cat "$tmp/old"; printf '\n'; compact; } > "$tmp/expected"
cmp "$tmp/expected" via_verifier/src/lib.rs
scripts/format-changed.sh --check
printf 'PASS: changed statement compacted; old tall code untouched; check mode read-only.\n'

fixture new
file=$'core/lib/via_fixture/src/new "quoted"\tname.rs'
tall > "$file"
fail_check
scripts/format-changed.sh
compact > "$tmp/expected"
cmp "$tmp/expected" "$file"
scripts/format-changed.sh --check
printf 'PASS: untracked new file formatted whole; spaces, quotes, tabs handled.\n'

fixture deletion
printf '\nfn removed() {}\n' >> via_verifier/src/lib.rs
git add -- via_verifier/src/lib.rs
commit
cp "$tmp/old" via_verifier/src/lib.rs
scripts/format-changed.sh --check > "$tmp/deletion.log"
grep -F '0 file(s) checked' "$tmp/deletion.log"
scripts/format-changed.sh
cmp "$tmp/old" via_verifier/src/lib.rs
printf 'PASS: deleted-only hunk ignored.\n'

fixture upstream
tall > core/lib/upstream/src/via_named.rs
tall > core/lib/via_fixture/src/owned.rs
scripts/format-changed.sh
compact > "$tmp/expected"
cmp "$tmp/expected" core/lib/via_fixture/src/owned.rs
tall > "$tmp/expected"
cmp "$tmp/expected" core/lib/upstream/src/via_named.rs
scripts/format-changed.sh --check
printf 'PASS: upstream file ignored despite via_ basename; Via sibling formatted.\n'

fixture ci
git branch baseline
printf '\n' >> via_verifier/src/lib.rs
tall >> via_verifier/src/lib.rs
git add -- via_verifier/src/lib.rs
commit
[[ -z "$(git status --porcelain)" ]]
scripts/format-changed.sh --check > "$tmp/default.log"
grep -F '0 file(s) checked' "$tmp/default.log"
fail_check --base baseline
printf '\nCI-like failure before formatting (committed change):\n'
cat "$tmp/check.log"
scripts/format-changed.sh --base baseline
{ cat "$tmp/old"; printf '\n'; compact; } > "$tmp/expected"
cmp "$tmp/expected" via_verifier/src/lib.rs
git add -- via_verifier/src/lib.rs
commit
[[ -z "$(git status --porcelain)" ]]
printf '\nCI-like success after formatting (clean checkout):\n'
scripts/format-changed.sh --check --base baseline
printf 'PASS: explicit base finds committed changes; formatted commit passes.\n'

fixture staged
tall > via_indexer/new.rs
git add -- via_indexer/new.rs
fail_check staged
scripts/format-changed.sh staged
compact > "$tmp/expected"
cmp "$tmp/expected" via_indexer/new.rs
# Formatting leaves the index unchanged; contributors explicitly re-stage.
git add -- via_indexer/new.rs
scripts/format-changed.sh staged --check
printf '\nfn unstaged() {}\n' >> via_indexer/new.rs
cp via_indexer/new.rs "$tmp/before"
if scripts/format-changed.sh staged > "$tmp/staged.log" 2>&1; then
  echo 'Expected mixed staged/unstaged file rejection.' >&2
  exit 1
fi
grep -F 'has unstaged changes' "$tmp/staged.log"
cmp "$tmp/before" via_indexer/new.rs
printf 'PASS: staged additions formatted; mixed index/worktree coordinates rejected.\n'

fixture coverage
scripts/format-changed.sh --check-config
mkdir -p core/node/via_new
cp core/lib/via_fixture/Cargo.toml core/node/via_new/
if scripts/format-changed.sh --check-config > "$tmp/coverage.log" 2>&1; then
  echo 'Expected missing profile rejection.' >&2
  exit 1
fi
grep -F 'core/node/via_new/rustfmt.toml must contain exactly' "$tmp/coverage.log"
cp via_verifier/rustfmt.toml core/node/via_new/
scripts/format-changed.sh --check-config
printf 'max_width = 140\n' > core/node/via_new/rustfmt.toml
if scripts/format-changed.sh --check-config > "$tmp/coverage.log" 2>&1; then
  echo 'Expected divergent profile rejection.' >&2
  exit 1
fi
grep -F 'core/node/via_new/rustfmt.toml must contain exactly' "$tmp/coverage.log"
cp via_verifier/rustfmt.toml core/node/via_new/
cp via_verifier/rustfmt.toml via_verifier/src/.rustfmt.toml
if scripts/format-changed.sh --check-config > "$tmp/coverage.log" 2>&1; then
  echo 'Expected nested override rejection.' >&2
  exit 1
fi
grep -F 'via_verifier/src/.rustfmt.toml overrides the Via profile' "$tmp/coverage.log"
printf 'PASS: new crates, divergent profiles, and hidden nested overrides checked.\n'

fixture rename
git mv via_verifier/src/lib.rs via_verifier/src/renamed.rs
scripts/format-changed.sh --check > "$tmp/rename.log"
grep -F '0 file(s) checked' "$tmp/rename.log"
scripts/format-changed.sh
cmp "$tmp/old" via_verifier/src/renamed.rs
printf 'PASS: pure rename preserves old tall code.\n'

fixture single
printf '\nfn changed() {\n    let value = 1;\n}\n' >> via_verifier/src/lib.rs
git add -- via_verifier/src/lib.rs
commit
sed 's/let value = 1;/let value=2;/' via_verifier/src/lib.rs > "$tmp/edited"
cp "$tmp/edited" via_verifier/src/lib.rs
fail_check
scripts/format-changed.sh
{ cat "$tmp/old"; printf '\nfn changed() {\n    let value = 2;\n}\n'; } > "$tmp/expected"
cmp "$tmp/expected" via_verifier/src/lib.rs
scripts/format-changed.sh --check
printf 'PASS: omitted hunk count selects exactly one changed line.\n'

fixture modules
cp via_verifier/src/lib.rs via_verifier/src/child.rs
git add -- via_verifier/src/child.rs
commit
{ printf 'mod child;\n\n'; tall; } > via_verifier/src/new.rs
scripts/format-changed.sh
{ printf 'mod child;\n\n'; compact; } > "$tmp/expected"
cmp "$tmp/expected" via_verifier/src/new.rs
cmp "$tmp/old" via_verifier/src/child.rs
printf 'PASS: formatting a parent leaves an unchanged child module untouched.\n'

fixture separated
{ printf 'fn first() {\n    let value = 1;\n}\n\n'; cat "$tmp/old"; printf '\nfn last() {\n    let value = 1;\n}\n'; } > via_verifier/src/lib.rs
git add -- via_verifier/src/lib.rs
commit
git config diff.interHunkContext 999
printf '*.rs -diff\n' > .gitattributes
sed 's/let value = 1;/let value=2;/' via_verifier/src/lib.rs > "$tmp/edited"
cp "$tmp/edited" via_verifier/src/lib.rs
fail_check
scripts/format-changed.sh
{ printf 'fn first() {\n    let value = 2;\n}\n\n'; cat "$tmp/old"; printf '\nfn last() {\n    let value = 2;\n}\n'; } > "$tmp/expected"
cmp "$tmp/expected" via_verifier/src/lib.rs
scripts/format-changed.sh --check
printf 'PASS: separate hunks preserve old middle code despite Git context/binary settings.\n'

fixture merge_base
git switch -qc target
printf 'fn old() {\n    let value = things.iter().copied().collect::<Vec<_>>();\n}\n' > via_verifier/src/lib.rs
git add -- via_verifier/src/lib.rs
commit
git switch -q main
printf '\n' >> via_verifier/src/lib.rs
compact >> via_verifier/src/lib.rs
git add -- via_verifier/src/lib.rs
commit
scripts/format-changed.sh --check --base target
{ cat "$tmp/old"; printf '\n'; compact; } > "$tmp/expected"
cmp "$tmp/expected" via_verifier/src/lib.rs
printf 'PASS: diverged target uses merge base, excluding target-only changes.\n'

fixture imports
{ printf 'use std::fmt;\nuse external::old;\nuse crate::old;\n\n'; cat "$tmp/old"; } > via_verifier/src/lib.rs
git add -- via_verifier/src/lib.rs
commit
{ printf 'use std::fmt;\nuse std :: io;\nuse external::old;\nuse crate::old;\n\n'; cat "$tmp/old"; } > via_verifier/src/lib.rs
cp via_verifier/src/lib.rs "$tmp/before"
cp via_verifier/src/lib.rs "$tmp/upstream-imports.rs"
range=$(jq -cn --arg file "$tmp/upstream-imports.rs" '[{file: $file, range: [2, 2]}]')
toolchain=$(sed -n 's/^channel = "\([^"]*\)"$/\1/p' rust-toolchain)
rustup run "$toolchain" rustfmt --edition 2021 --unstable-features --skip-children \
  --file-lines "$range" --config imports_granularity=Crate --config group_imports=StdExternalCrate \
  "$tmp/upstream-imports.rs"
{ printf 'use std::{fmt, io};\n\nuse external::old;\n\nuse crate::old;\n\n'; cat "$tmp/old"; } > "$tmp/expected"
cmp "$tmp/expected" "$tmp/upstream-imports.rs"
fail_check
cmp "$tmp/before" via_verifier/src/lib.rs
scripts/format-changed.sh
{ printf 'use std::fmt;\nuse std::io;\nuse external::old;\nuse crate::old;\n\n'; cat "$tmp/old"; } > "$tmp/expected"
cmp "$tmp/expected" via_verifier/src/lib.rs
scripts/format-changed.sh --check
printf 'PASS: added use line formatted alone; workspace import settings demonstrably rewrite old imports.\n'

fixture new_imports
printf 'use crate::old;\nuse std::io;\nuse external::old;\nuse std::fmt;\n\nfn fresh() {}\n' > via_indexer/new.rs
fail_check
scripts/format-changed.sh
printf 'use std::{fmt, io};\n\nuse external::old;\n\nuse crate::old;\n\nfn fresh() {}\n' > "$tmp/expected"
cmp "$tmp/expected" via_indexer/new.rs
scripts/format-changed.sh --check
git add -- via_indexer/new.rs
scripts/format-changed.sh staged --check
printf 'PASS: new-file imports use workspace grouping and merging in write and check modes.\n'

fixture errors
if scripts/format-changed.sh --check --base absent-ref > "$tmp/error.log" 2>&1; then
  echo 'Expected missing base failure.' >&2
  exit 1
fi
if scripts/format-changed.sh --unknown > "$tmp/error.log" 2>&1; then
  echo 'Expected unknown argument failure.' >&2
  exit 1
fi
printf 'PASS: missing base and invalid arguments fail closed.\n'

printf '\nAll changed-line formatter scenarios passed.\n'
