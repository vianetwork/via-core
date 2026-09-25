---
status: research
---

# Alloy engineering patterns and possible Via use

## Question and conclusion

Which Alloy implementations can improve Via's existing Bitcoin-settled system or a future EVM-facing adapter? Source comparison supports reusing several ownership, decoding and resource-accounting patterns now, but establishes no need for a new Alloy dependency. A future independently built diagnostic or synthetic-probe adapter is a narrower experiment than replacing inherited workspace types. Neither that experiment nor a re-fork is authorized by this research.

The inspected Via revision is `269b81cf056b2bb13294a042b93720b4f5500efe`. Alloy and Alloy-core are MIT OR Apache-2.0 at the revisions below. Source and manifests were inspected; no dependency resolution, build, benchmark, network transaction or fault-injection scenario was executed.

## Version boundary

| Source | Inspected revision | Manifest requirement |
| --- | --- | --- |
| [Alloy][alloy-manifest], workspace 2.5.0 | `459b32ccba163e2a3f2df0c6c35e738142dab5c3` | Rust 1.94.1, edition 2021 |
| [Alloy-core][core-manifest], workspace 1.7.3 | `26c40597149f249f8425dc58227d2d4b7597ceb5` | Rust 1.85, edition 2024 |
| [Via toolchain][via-toolchain] | `269b81cf056b2bb13294a042b93720b4f5500efe` | nightly-2024-08-01 |

Those Alloy workspace versions cannot simply be adopted under Via's pinned compiler. This is not a claim about every repository or old release in the Alloy organization: Via's lockfile already contains `alloy-rlp` 0.3.10 transitively. No compatible older provider/core release was selected here.

## Mechanisms worth transferring

### Keep network types coherent at the adapter boundary

Alloy's [`Network`][network] associates transaction envelopes, unsigned transactions, receipts, headers and RPC requests/responses. This prevents a provider from casually mixing one network's consensus types with another network's response types. Its bounds include EIP-2718 and other EVM concepts: it is not a generic Bitcoin network model.

Via already has [`EthInterface` and `BoundEthInterface`][eth-client], with web3/ethabi types and jsonrpsee errors. A later adapter should sit at that boundary, inventory Via-specific RPC and transaction formats, and leave withdrawal authority and Bitcoin parsing in their existing owners. Inherited [`raw_ethereum_tx.rs`][eth-signer] is a possible maintenance target, not evidence that Alloy's transaction encodings are byte-compatible. Compare signed bytes, supported transaction variants, errors and every consumer before a cutover.

### Serialize the narrow owner, and distinguish reservation from submission

[`CachedNonceManager::get_next_nonce`][nonce] stores an `Arc<DashMap<Address, Arc<Mutex<u64>>>>`. It clones the address-specific `Arc` under the map guard, releases that guard, then awaits the address mutex and initial pending-count RPC. Subsequent reservations avoid that RPC. Independent addresses do not share one long-lived map lock.

The cache advances before broadcast. It has no durable reservation journal or automatic reconciliation after failed submission, restart, a competing sender or reorg. `SimpleNonceManager` asks for the pending count each time but does not reserve distinct nonces for concurrent callers. An explicitly supplied nonce is preserved by the filler.

The transferable pattern is narrow lock ownership and explicit lifecycle naming. A synthetic probe still needs a durable cycle, submission identity, spending limits and reconciliation of unknown outcomes. Ethereum account nonces are not MuSig2 secret nonces; this cache cannot replace Via's durable signing rounds or uncertain holds.

### Account for cancellation without confusing retries with idempotence

[`RetryBackoffService`][retry] clones and retries the whole RPC `RequestPacket`. Retrying a batch can resend members that already succeeded. Its policy classifies errors and may use delay hints; the inspected base-plus-budget delay is not exponential backoff.

`QueuedRequest` increments a shared atomic counter and decrements it in `Drop`, including cancellation. This is useful resource-accounting structure. It is not an admission limit: `poll_ready` delegates to the inner service. A retry count also does not impose a total deadline on a hung inner call.

Transport recovery belongs below domain decisions. A successful remote side effect followed by a lost response remains an unknown outcome, not permission to fund another probe cycle or repeat a secret signing operation. Preserve enriched errors and make unavailable evidence explicit rather than converting failures into success or zero.

### Index due work without promoting an in-memory watcher into authority

Alloy's [`Heartbeat`][heart] uses a lookbehind `VecDeque`, a transaction-keyed unconfirmed map, confirmation-height `BTreeMap<u64, Vec<TxWatcher>>`, and deadline `BTreeMap<Instant, B256>`. Ordered splits collect due confirmations and timeouts; oneshots deliver results. These structures avoid repeatedly scanning every waiter for each deadline.

The admission channel has capacity 64, but that does not bound the resident watcher maps. The inspected heartbeat retains ten blocks, handles height discontinuities and moves affected waiters back to unconfirmed; it does not itself establish full canonical ancestry. Its state is in memory. A delivered oneshot is not a durable fulfillment record with later reorg revocation.

A future probe may use such a watcher for convenience while preserving durable reconciliation above it. It is neither the independently executing Bitcoin observer required by [ADR 0001][isolation] nor an external dead-man receiver. These three mechanisms share a word, not a failure contract.

### Separate ABI acceptance from resource limits

[`AbiDecoderConfig`][decoder] separates recursion, memory accounting, validation, strict layout and trailing-byte acceptance. At this pin the defaults are recursion depth 16, memory accounting limit 1 GiB, `validate = false`, and `strict = false`. Strict mode implies validation. Child decoders share accounting; reservation uses checked addition and element sizing uses checked multiplication.

These limits do not establish a whole-process memory bound. Their useful lesson is to expose resource policy separately from accepted wire grammar. Typed `sol!` codecs or dynamic ABI decoding may eventually help around [`L2CanonicalTransaction`][abi], but replacing a codec requires exact acceptance, byte and hash comparisons. Bitcoin deposit decoding is not Solidity ABI decoding; [ADR 0003][deposit] deliberately permits specific trailing data.

An independently built ABI-vector generator is a possible research tool for upgrade calldata comparisons, not a historical governance oracle. It would need pinned versions and golden comparisons against the current producer and consumers before its output became evidence.

### Improve a demonstrated algorithm, not a hypothetical hotspot

[Alloy-core's traversal change][traversal] replaces repeated membership scans of the ordered resolution vector with a separate visited `HashSet`, while retaining the output vector's order. Expected constant-time membership replaces repeated linear scans at the cost of allocating a set. That is a concrete algorithmic mechanism, not a measured Via speedup or a claim about the complexity of the whole EIP-712 pipeline.

No equivalent Via hotspot was established. Use the pattern only if actual graph access patterns justify it; do not add a set to a small fixed collection merely because another project did.

## Applicability and boundaries

| Use | Recommendation | Required proof before adoption |
| --- | --- | --- |
| Current bridge and monitoring decisions | Reuse lifecycle, lock, error and resource patterns; no dependency needed | Preserve existing accepted contracts and sibling behavior |
| Independently versioned L2 diagnostic/probe adapter | Plausible later experiment | Via RPC/encoding compatibility, durable unknown-outcome handling, custody and budget policy |
| ABI and Ethereum signer maintenance | Later scoped investigation at existing owners | Toolchain plan, exact bytes/hashes, all callers, examples and public errors |
| MuSig2 admission, Bitcoin inclusions, proof verification or dead-man monitoring | Wrong abstraction | Alloy's EVM helpers do not supply these domain guarantees |

The verifier currently uses both ethers 1 and ethers 2 in different crates. Replacing a `U256` import alone would not remove them: public error conversions and the verification CLI example use ethers provider/contract types, and the withdrawal client uses its ABI types in multiple files. A dependency cleanup needs its own consumer inventory; it does not inherently require Alloy.

No evidence here selects new protocol acceptance rules, a fulfillment depth, a heartbeat period, a provider vendor or a workspace migration. The concrete benefit is better implementation questions and smaller, correctly owned future experiments.

[alloy-manifest]: https://github.com/alloy-rs/alloy/blob/459b32ccba163e2a3f2df0c6c35e738142dab5c3/Cargo.toml
[core-manifest]: https://github.com/alloy-rs/core/blob/26c40597149f249f8425dc58227d2d4b7597ceb5/Cargo.toml
[via-toolchain]: https://github.com/vianetwork/via-core/blob/269b81cf056b2bb13294a042b93720b4f5500efe/rust-toolchain
[network]: https://github.com/alloy-rs/alloy/blob/459b32ccba163e2a3f2df0c6c35e738142dab5c3/crates/network/src/lib.rs#L40-L106
[eth-client]: https://github.com/vianetwork/via-core/blob/269b81cf056b2bb13294a042b93720b4f5500efe/core/lib/eth_client/src/lib.rs
[eth-signer]: https://github.com/vianetwork/via-core/blob/269b81cf056b2bb13294a042b93720b4f5500efe/core/lib/eth_signer/src/raw_ethereum_tx.rs
[nonce]: https://github.com/alloy-rs/alloy/blob/459b32ccba163e2a3f2df0c6c35e738142dab5c3/crates/provider/src/fillers/nonce.rs#L25-L205
[retry]: https://github.com/alloy-rs/alloy/blob/459b32ccba163e2a3f2df0c6c35e738142dab5c3/crates/transport/src/layers/retry.rs#L196-L365
[heart]: https://github.com/alloy-rs/alloy/blob/459b32ccba163e2a3f2df0c6c35e738142dab5c3/crates/provider/src/heart.rs#L465-L742
[decoder]: https://github.com/alloy-rs/core/blob/26c40597149f249f8425dc58227d2d4b7597ceb5/crates/sol-types/src/abi/decoder.rs#L20-L178
[abi]: https://github.com/vianetwork/via-core/blob/269b81cf056b2bb13294a042b93720b4f5500efe/core/lib/types/src/abi.rs
[traversal]: https://github.com/alloy-rs/core/commit/26c40597149f249f8425dc58227d2d4b7597ceb5
[isolation]: ../adr/0001-isolate-btc-inscription-observability.md
[deposit]: ../adr/0003-decode-op-return-deposit-pushes.md
