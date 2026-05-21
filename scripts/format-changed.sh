#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   scripts/format-changed.sh              # format files changed vs HEAD
#   scripts/format-changed.sh staged       # format only staged files
#   scripts/format-changed.sh --check      # CI-style: exit 1 if formatting needed

MODE="working"
CHECK=false
RUST_EDITION="2021"

for arg in "$@"; do
  case "$arg" in
    staged)  MODE="staged" ;;
    --check) CHECK=true ;;
  esac
done

cd "$(git rev-parse --show-toplevel)"

if ! command -v rustfmt >/dev/null 2>&1; then
  echo "Error: rustfmt is not installed or not on PATH." >&2
  exit 127
fi

get_changed_files() {
  if [[ "$MODE" == "staged" ]]; then
    git diff --cached --name-only -z --diff-filter=ACMR
  else
    git diff HEAD --name-only -z --diff-filter=ACMR
    git ls-files --others --exclude-standard -z
  fi
}

via_rust_files=()
non_via_rust_files=()

while IFS= read -r -d '' file; do
  [[ -f "$file" ]] || continue

  if [[ "$file" == *.rs ]]; then
    basename="${file##*/}"

    # Detect any via_* path segment or Rust basename.
    if [[ "$file" == via_*/* || "$file" == */via_*/* || "$basename" == via_*.rs ]]; then
      via_rust_files+=("$file")
    else
      non_via_rust_files+=("$file")
    fi
  fi
done < <(get_changed_files)

run_rustfmt() {
  local files=("$@")
  local rustfmt_args=("--skip-children" "--edition" "$RUST_EDITION")

  # Via files use their local per-directory rustfmt.toml (if present).
  # rustfmt discovers these automatically when run on the files. The explicit
  # edition matches the Via rustfmt.toml files and keeps direct rustfmt calls
  # aligned with modern Rust syntax when no crate metadata is consulted.

  if $CHECK; then
    rustfmt_args=("--check" "${rustfmt_args[@]}")
  fi

  printf '%s\0' "${files[@]}" | xargs -0 rustfmt "${rustfmt_args[@]}" --
}

echo "=== Formatting changed files ==="

if ((${#via_rust_files[@]})); then
  echo "→ Via Rust files (compact style)"
  run_rustfmt "${via_rust_files[@]}"
fi

if ((${#non_via_rust_files[@]})); then
  echo "→ Non-Via Rust files (targeted only — never full workspace from this script)"
  # Format only the specific changed files using rustfmt directly.
  # This avoids accidentally triggering workspace-wide formatting.
  run_rustfmt "${non_via_rust_files[@]}"
fi

if ((${#via_rust_files[@]} == 0 && ${#non_via_rust_files[@]} == 0)); then
  echo "No Rust files to format."
fi

echo "✅ Done."
