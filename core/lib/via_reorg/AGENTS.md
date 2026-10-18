# AGENTS.md

This crate owns L1 reorg comparison logic shared by the main-node and verifier
detectors. Keep comparison helpers pure, dependency-free and testable in
isolation. Keep both detectors' call sites thin.
