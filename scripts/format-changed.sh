#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo 'Usage: scripts/format-changed.sh [staged | --base REF] [--check]'
  echo '       scripts/format-changed.sh --check-config'
}

mode=working
check=false
base=
while (($#)); do
  case "$1" in
    staged)
      [[ "$mode" == working ]] || { usage >&2; exit 2; }
      mode=staged
      ;;
    --base)
      [[ "$mode" == working && $# -ge 2 && -n "$2" ]] || { usage >&2; exit 2; }
      mode=base
      base=$2
      shift
      ;;
    --check) check=true ;;
    --check-config)
      [[ $# == 1 && "$mode" == working && "$check" == false ]] || { usage >&2; exit 2; }
      mode=config
      ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
  shift
done

cd "$(git rev-parse --show-toplevel)"

tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

# Cargo roots define ownership; via_ filenames in upstream crates do not.
is_via() {
  local dir=$1
  case "$dir" in
    via_verifier/*|via_indexer/*) return 0 ;;
    core/*)
      dir=${dir%/*}
      while [[ "$dir" != core ]]; do
        if [[ "${dir##*/}" == via_* && -f "$dir/Cargo.toml" ]]; then
          return 0
        fi
        dir=${dir%/*}
      done
      ;;
  esac
  return 1
}

if [[ "$mode" == config ]]; then
  printf 'edition = "2021"\nmax_width = 100\nuse_small_heuristics = "Max"\n' > "$tmp/profile"
  configs=(via_verifier/rustfmt.toml via_indexer/rustfmt.toml)
  git ls-files --cached --others --exclude-standard -z -- core > "$tmp/core"
  while IFS= read -r -d '' file; do
    [[ "$file" == */via_*/Cargo.toml && -f "$file" ]] || continue
    dir=${file%/*}
    [[ "${dir##*/}" == via_* ]] && configs+=("$dir/rustfmt.toml")
  done < "$tmp/core"
  failed=false
  for config in "${configs[@]}"; do
    if [[ -L "$config" ]] || ! cmp -s "$tmp/profile" "$config"; then
      printf 'ERROR: %s must contain exactly:\n' "$config" >&2
      cat "$tmp/profile" >&2
      failed=true
    fi
  done
  git ls-files --cached --others --exclude-standard -z -- core via_verifier via_indexer > "$tmp/scoped"
  while IFS= read -r -d '' file; do
    [[ -f "$file" && ( "${file##*/}" == rustfmt.toml || "${file##*/}" == .rustfmt.toml ) ]] || continue
    is_via "$file" || continue
    expected=false
    for config in "${configs[@]}"; do
      [[ "$file" != "$config" ]] || expected=true
    done
    if ! $expected; then
      printf 'ERROR: %s overrides the Via profile; remove the nested config.\n' "$file" >&2
      failed=true
    fi
  done < "$tmp/scoped"
  $failed && exit 1
  printf 'Via formatting profiles: %s checked.\n' "${#configs[@]}"
  exit 0
fi

for tool in jq rustup; do
  command -v "$tool" >/dev/null || { echo "ERROR: install $tool before formatting." >&2; exit 127; }
done
# Read the repository pin; explicit rustup selection ignores shell overrides.
toolchain=$(sed -n 's/^channel = "\([^"]*\)"$/\1/p' rust-toolchain)
[[ -n "$toolchain" ]] || { echo 'ERROR: missing channel in rust-toolchain.' >&2; exit 2; }

diff_args=(HEAD)
case "$mode" in
  staged) diff_args=(--cached) ;;
  base) diff_args=("$(git merge-base "$base" HEAD)") ;;
esac
git diff --no-ext-diff --find-renames --name-status -z --diff-filter=ACMRT "${diff_args[@]}" -- > "$tmp/files"
if [[ "$mode" != staged ]]; then
  git ls-files --others --exclude-standard -z > "$tmp/untracked"
  while IFS= read -r -d '' file; do
    printf '?\0%s\0' "$file" >> "$tmp/files"
  done < "$tmp/untracked"
fi

failed=false
count=0
while IFS= read -r -d '' status && IFS= read -r -d '' file; do
  paths=("$file")
  if [[ "$status" == R* ]]; then
    IFS= read -r -d '' file
    paths+=("$file")
  fi
  [[ "$file" == *.rs && -f "$file" ]] || continue
  is_via "$file" || continue
  if [[ -L "$file" ]]; then
    printf 'ERROR: refusing to format symlink %s.\n' "$file" >&2
    exit 2
  fi
  if [[ "$mode" == staged ]] && ! git --literal-pathspecs diff --quiet -- "$file"; then
    printf 'ERROR: %s has unstaged changes; stage them or use working-tree mode.\n' "$file" >&2
    exit 2
  fi
  : > "$tmp/ranges"
  if [[ "$status" == '?' ]]; then
    lines=$(awk 'END { print NR }' "$file")
    ((lines == 0)) || printf '[1,%s]\n' "$lines" > "$tmp/ranges"
  else
    git --literal-pathspecs diff --no-ext-diff --no-textconv --no-color --text --find-renames -U0 --inter-hunk-context=0 \
      "${diff_args[@]}" -- "${paths[@]}" > "$tmp/diff"
    # Adapted rustfmt-format-diff hunk parsing; NUL Git paths replace header parsing.
    # Via keeps only added-line ranges, leaving deletion-only hunks untouched.
    # https://github.com/rust-lang/rustfmt/blob/c69af7786cdfef5da968f53eebd6c9b1af242b5f/src/format-diff/main.rs#L131-L205
    while IFS= read -r line; do
      if [[ "$line" =~ ^@@\ -[0-9]+(,[0-9]+)?\ \+([0-9]+)(,([0-9]+))?\ @@ ]]; then
        start=${BASH_REMATCH[2]}
        length=${BASH_REMATCH[4]:-1}
        ((length == 0)) || printf '[%s,%s]\n' "$start" "$((start + length - 1))" >> "$tmp/ranges"
      fi
    done < "$tmp/diff"
  fi
  [[ -s "$tmp/ranges" ]] || continue
  ranges=$(jq -cs --arg file "$file" 'map({file: $file, range: .})' "$tmp/ranges")
  args=(--edition 2021 --unstable-features --skip-children --file-lines "$ranges")
  if [[ "$status" == A || "$status" == '?' ]]; then
    args+=(--config imports_granularity=Crate --config group_imports=StdExternalCrate)
  else
    # Rustfmt reorders whole import groups when any member intersects file-lines.
    # Via preserves existing groups; workspace import rules apply only to new files.
    # https://github.com/rust-lang/rustfmt/blob/c69af7786cdfef5da968f53eebd6c9b1af242b5f/src/reorder.rs#L305-L310
    args+=(--config 'imports_granularity=Preserve,group_imports=Preserve,reorder_imports=false')
  fi
  $check && args+=(--check)
  count=$((count + 1))
  if ! rustup run "$toolchain" rustfmt "${args[@]}" -- "$file"; then
    printf 'ERROR: rustfmt failed for %s; selected lines: ' "$file" >&2
    jq -r 'map(.range | "\(.[0])-\(.[1])") | join(", ")' <<< "$ranges" >&2
    failed=true
  fi
done < "$tmp/files"

if $failed; then
  printf 'Fix locally: scripts/format-changed.sh' >&2
  case "$mode" in
    staged) printf ' staged' >&2 ;;
    base) printf ' --base %q' "$base" >&2 ;;
  esac
  printf '\n' >&2
  exit 1
fi
printf 'Via changed-line formatting: %s file(s) %s.\n' "$count" "$($check && echo checked || echo formatted)"
