# justfile for via-core
# https://github.com/casey/just
#
# Install just:
#   macOS:   brew install just
#   Linux:   curl --proto '=https' --tlsv1.2 -sSf https://just.systems/install.sh | bash -s -- --to /usr/local/bin
#   Windows: winget install --id Casey.Just -e
#
# Usage examples:
#   just fmt
#   just check
#   just via-check
#   just via-check-strict

# Format code using zkstack
fmt:
    zkstack dev fmt

# Run general linting
check:
    zkstack dev lint

# Run Via-specific structural rules (advisory mode by default)
via-check:
    VIA_STRUCTURAL_RULES_MODE=advisory .github/scripts/check-via-structural-rules.sh

# Run Via-specific structural rules in strict (blocking) mode
via-check-strict:
    VIA_STRUCTURAL_RULES_MODE=strict .github/scripts/check-via-structural-rules.sh
