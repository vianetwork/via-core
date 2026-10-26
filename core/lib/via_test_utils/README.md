# Via test utilities

This crate contains shared Bitcoin RPC fixtures and isolated Core helpers for Via tests.

`BitcoinCore` starts and owns a disposable regtest process. Set `VIA_TEST_BITCOIND` to the absolute
path of the Core executable pinned in [bitcoin-core.json](../../../.github/test-fixtures/bitcoin-core.json).
Startup checks the executable checksum, Core version and regtest network; it does not discover a node on PATH.
The fixture owns its RPC forwarding servers and stops its process on drop.

Reader integration tests additionally need a disposable PostgreSQL test database configured through
`TEST_DATABASE_URL`. Run `cargo test -p via_btc_storage --test reader` with both fixtures available.
These tests use local fixture wallets and generated history.

The positive shallow-rewind test injects the six-block policy against isolated regtest headers to exercise
recovery below N. This is a controlled fixture input, not a production confirmation setting.
Production regtest still has N=1; network and bootstrap validation have separate tests.
