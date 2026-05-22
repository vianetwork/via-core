#!/usr/bin/env bash
#
# Via Structural Rules Checker
#
# Runs Via-specific structural safety rules (currently powered by ast-grep).
# These rules are advisory review aids for high-risk ordering/identity patterns.
#
# Usage:
#   .github/scripts/check-via-structural-rules.sh
#
#   # Advisory mode (default): prints findings but exits 0
#   VIA_STRUCTURAL_RULES_MODE=advisory .github/scripts/check-via-structural-rules.sh
#
#   # Strict mode: exits non-zero on scanner errors or new unbaselined findings
#   VIA_STRUCTURAL_RULES_MODE=strict .github/scripts/check-via-structural-rules.sh
#

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RULES_DIR="$REPO_ROOT/.github/lint/via-structural/ast-grep/rules"
BASELINE_FILE="$REPO_ROOT/.github/lint/via-structural/ast-grep/baseline.txt"
MODE="${VIA_STRUCTURAL_RULES_MODE:-advisory}"
FINDING_SEVERITY_RE='(\[(warning|error)\]|(warning|error)\[)'

case "$MODE" in
    advisory|strict) ;;
    *)
        echo "Error: VIA_STRUCTURAL_RULES_MODE must be 'advisory' or 'strict' (got: $MODE)" >&2
        exit 2
        ;;
esac

if ! command -v ast-grep >/dev/null 2>&1; then
    echo "Error: ast-grep is not installed." >&2
    echo "Install it from https://ast-grep.github.io/guide/quick-start.html" >&2
    exit 127
fi

if [[ ! -d "$RULES_DIR" ]]; then
    echo "Error: Rules directory not found: $RULES_DIR" >&2
    exit 1
fi

echo "Running Via structural rules (mode: $MODE)..."
echo

# ast-grep config file
AST_GREP_DIR="$(dirname "$RULES_DIR")"
SGCONFIG="$AST_GREP_DIR/sgconfig.yml"

if [[ ! -f "$SGCONFIG" ]]; then
    echo "Error: ast-grep config not found at $SGCONFIG" >&2
    exit 1
fi

HAS_ISSUES=false
HAS_UNBASELINED_FINDINGS=false
HAS_SCANNER_ERRORS=false
OUTPUT=""
UNBASELINED_FINDINGS=""
SCANNER_ERRORS=""
ACTUAL_FINDINGS=""

finding_counts() {
    sed -nE 's#^([^#][^|]*\|[^|]+)(\|[0-9]+\|[0-9]+)?$#\1#p' \
        | sort \
        | uniq -c \
        | awk '{ count = $1; $1 = ""; sub(/^[[:space:]]+/, ""); print $0 "|" count }'
}

compare_findings_to_baseline() {
    local actual_counts
    local baseline_counts
    local unbaselined_findings
    actual_counts="$(mktemp)"
    baseline_counts="$(mktemp)"
    unbaselined_findings="$(mktemp)"

    printf '%s' "$ACTUAL_FINDINGS" | finding_counts > "$actual_counts"
    if [[ -f "$BASELINE_FILE" ]]; then
        finding_counts < "$BASELINE_FILE" > "$baseline_counts"
    else
        : > "$baseline_counts"
    fi

    awk -F'|' '
        NR == FNR {
            baseline[$1 "|" $2] = $3
            next
        }
        {
            key = $1 "|" $2
            baseline_count = (key in baseline) ? baseline[key] : 0
            if ($3 > baseline_count) {
                print $1 "|" $2 " (actual: " $3 ", baseline: " baseline_count ")"
            }
        }
    ' "$baseline_counts" "$actual_counts" > "$unbaselined_findings"

    if [[ -s "$unbaselined_findings" ]]; then
        HAS_UNBASELINED_FINDINGS=true
        UNBASELINED_FINDINGS="$(cat "$unbaselined_findings")"$'\n'
    fi

    rm -f "$actual_counts" "$baseline_counts" "$unbaselined_findings"
}

run_scoped_rule() {
    local rule_id="$1"
    shift

    local args=("scan" "--config" "$SGCONFIG" "--filter" "$rule_id" "--report-style" "short" "--color=never")
    local glob
    for glob in "$@"; do
        args+=("--globs" "$glob")
    done

    local scan_output
    local scan_error
    scan_error="$(mktemp)"
    local exit_code
    set +e
    # Run from repo root so CLI globs are evaluated against repository paths.
    scan_output=$(cd "$REPO_ROOT" && ast-grep "${args[@]}" 2>"$scan_error")
    exit_code=$?
    set -e

    if [[ -n "$scan_output" ]]; then
        OUTPUT+="$scan_output"$'\n\n'
    fi
    if [[ -s "$scan_error" ]]; then
        OUTPUT+="stderr from $rule_id:"$'\n'
        OUTPUT+="$(cat "$scan_error")"$'\n\n'
    fi

    # Treat non-zero exit from ast-grep itself as a scanner error
    # (invalid config, invalid rule, runtime failure, etc.).
    if [[ $exit_code -ne 0 ]]; then
        HAS_ISSUES=true
        HAS_SCANNER_ERRORS=true
        SCANNER_ERRORS+="$rule_id exited with status $exit_code"$'\n'
    fi
    rm -f "$scan_error"

    # Use the tool's own signaling for robustness. ast-grep 0.42 short output
    # is `warning[rule-id]`; accept `[warning]` too in case the format changes.
    if grep -qE "$FINDING_SEVERITY_RE" <<< "$scan_output"; then
        HAS_ISSUES=true
    fi

    local finding_path
    while IFS= read -r finding_path; do
        [[ -n "$finding_path" ]] || continue
        ACTUAL_FINDINGS+="$rule_id|$finding_path"$'\n'
    done < <(
        sed -nE "s#^([^:]+):[0-9]+:[0-9]+: ${FINDING_SEVERITY_RE}.*#\\1#p" <<< "$scan_output"
    )
}

# ast-grep 0.42 does not apply the intended rule-level globs from sgconfig.yml
# in this repo setup, so the runner enforces scope via CLI globs. Keep this list
# in sync with .github/lint/via-structural/ast-grep/sgconfig.yml.
run_scoped_rule "via-reorg-buffer-unordered" \
    "core/node/via_main_node_reorg_detector/**/*.rs" \
    "via_verifier/node/via_reorg_detector/**/*.rs"

run_scoped_rule "via-reorg-height-association-required" \
    "core/node/via_main_node_reorg_detector/**/*.rs" \
    "via_verifier/node/via_reorg_detector/**/*.rs"

run_scoped_rule "via-reorg-empty-l1-blocks-nonfatal" \
    "core/node/via_main_node_reorg_detector/**/*.rs" \
    "via_verifier/node/via_reorg_detector/**/*.rs"

run_scoped_rule "via-da-batch-before-proof" \
    "core/node/via_da_dispatcher/**/*.rs"

run_scoped_rule "via-avoid-duplicate-export" \
    "core/node/via_*/**/*.rs" \
    "via_verifier/**/*.rs" \
    "core/lib/via_*/**/*.rs" \
    "core/lib/types/src/**/*.rs"

if [[ -n "$OUTPUT" ]]; then
    echo "$OUTPUT"
    echo
fi

compare_findings_to_baseline

if [[ "$HAS_ISSUES" == false ]]; then
    echo "✓ No structural issues found."
    exit 0
fi

# Issues were found
if [[ "$MODE" == "strict" ]]; then
    if [[ "$HAS_SCANNER_ERRORS" == true ]]; then
        echo "✗ Via structural scanner error(s) found (strict mode)."
        echo "$SCANNER_ERRORS"
        echo "Failing the check."
        exit 1
    fi

    if [[ "$HAS_UNBASELINED_FINDINGS" == true ]]; then
        echo "✗ New Via structural rules findings found (strict mode)."
        echo "Unbaselined findings:"
        echo "$UNBASELINED_FINDINGS"
        echo "Failing the check."
        exit 1
    fi

    echo "✓ Only baselined structural warnings found (strict mode)."
    echo "Known warnings remain visible above; new findings will fail this check."
    exit 0
else
    echo "⚠ Via structural rules warnings found (advisory mode)."
    echo "These are review prompts — not automatic defects."
    echo "Please review the findings above and justify the ordering/identity assumptions."
    echo "See .github/lint/via-structural/ast-grep/README.md for details."
    exit 0
fi
