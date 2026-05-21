# Formatting Changed Files (Experimental)

This script formats only the files you changed. It is designed to support more compact formatting in `via_*` paths while still respecting the project's standard formatting for upstream code.

**Status**: Experimental. Use for testing and feedback before wider adoption.

## Why this exists

- Auditors charge per line of production code in `via_*` areas.
- The default rustfmt style (used by zkstack dev fmt) produces very vertical code, increasing billable LOC.
- We want the ability to use more compact formatting in Via-specific code without fighting the formatter on every change.

## Usage

```bash
# Format files changed since last commit
scripts/format-changed.sh

# Format only files that are currently staged
scripts/format-changed.sh staged

# Check mode (useful in CI or before pushing)
scripts/format-changed.sh --check
```

## How it works

- It looks at files changed in the current branch / working tree.
- For files under `via_*` paths -> uses a more compact formatting configuration.
- For other (non-`via_*`) Rust files -> uses `rustfmt` directly on only the changed files.
- Non-Rust files are currently ignored (extend the script if needed).

## Recommended workflow (for testing)

1. Make your changes (or let an LLM make them).
2. Run:

   ```bash
   scripts/format-changed.sh
   ```

3. Review the diff.
4. Commit.

## Adding as a Pre-Commit Hook (Optional)

If you want to try it as a pre-commit hook later:

```bash
# From the repo root
mkdir -p .git/hooks
cat > .git/hooks/pre-commit << 'EOF'
#!/usr/bin/env bash
set -euo pipefail

staged_files=$(mktemp)
trap 'rm -f "$staged_files"' EXIT

# Only re-stage files that were originally staged. Keep the NUL-delimited list
# in a file; shell variables cannot safely store NUL bytes.
git diff --cached --name-only -z --diff-filter=ACMR > "$staged_files"

scripts/format-changed.sh staged

# Re-stage only the files that were originally staged
if [ -s "$staged_files" ]; then
  xargs -0 git add -- < "$staged_files"
fi
EOF

chmod +x .git/hooks/pre-commit
```

**Warning**: Do not enable this repo-wide until the script has been tested for a while. It is easy to accidentally reformat more than intended during the early phase.

## Current limitations

- Only handles Rust files for now.
- The compact style for `via_*` paths depends on local per-directory `rustfmt.toml` files in those paths.
- Working-tree mode includes untracked files; staged mode formats only staged files.
- zkstack dev fmt is still the recommended command for full workspace formatting.

## Feedback

Please report issues, especially around:

- Whether compact formatting is being applied correctly in `via_*` files.
- Any files that should be formatted but aren't.
- Friction when using it day-to-day.

This is intentionally being introduced gradually. We are not yet requiring it for all contributors.
