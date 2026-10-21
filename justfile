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

# Adopt explicit leaves/default listing; Via leaves select the isolated stable crate.
# https://github.com/rust-bitcoin/rust-bitcoin/blob/53ee132ca2737466f942f8b7db33a9871bdd4f7b/justfile
# List available commands
default:
    @just --list

# Format code using zkstack
fmt:
    zkstack dev fmt

# Run general linting
check:
    zkstack dev lint

# Run Via-specific structural rules (advisory mode by default)
via-check:
    VIA_STRUCTURAL_RULES_MODE=advisory .github/scripts/check-via-structural-rules.sh

# Check new or changed exact copies in Via-owned Rust
[group('via')]
[positional-arguments]
via-dup-check *args:
    cargo +stable run --quiet --locked --manifest-path via_duplication/Cargo.toml -- check "$@"

# Report copies outside shared owners (advisory)
[group('via')]
[positional-arguments]
via-dup-boundaries *args:
    cargo +stable run --quiet --locked --manifest-path via_duplication/Cargo.toml -- boundaries "$@"

# Report existing duplication debt
[group('via')]
[positional-arguments]
via-dup-inventory *args:
    cargo +stable run --quiet --locked --manifest-path via_duplication/Cargo.toml -- inventory "$@"

# Run Via-specific structural rules in strict (blocking) mode
via-check-strict: via-dup-check
    VIA_STRUCTURAL_RULES_MODE=strict .github/scripts/check-via-structural-rules.sh
