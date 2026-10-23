# Via test fixtures

`chain::Chain` retains competing branches and serves explicit missing-data and
unavailable-source responses through `BitcoinRpcServer`.

`bitcoin_core::BitcoinCore` owns a disposable regtest process through test-only
`bitcoind = 0.43.2`, with download features disabled. Provision the Core archive
and verify both checksums from
[`bitcoin-core.json`](../../../.github/test-fixtures/bitcoin-core.json), as the
[`Provision pinned Bitcoin Core` CI step](../../../.github/workflows/tests-via.yml)
does. Then run from the repository root:

```sh
VIA_TEST_BITCOIND=/absolute/path/to/bitcoin-28.2/bin/bitcoind \
  cargo test --locked -p via_test_utils bitcoin_core::tests
```

The fixture validates the executable bytes before startup and uses a temporary
data directory, random ports and cookie authentication. Keep its owner alive
until every client finishes. Missing provisioning fails the test; the fixture
does not discover a node on PATH or connect to an existing wallet.

Inscription fixtures use a fixed fee estimate because a fresh Core estimator
has no fee history. Their transaction and broadcast calls still reach their
isolated Core. Header/source tests use the direct client.
