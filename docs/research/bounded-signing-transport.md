# Bounded withdrawal signing transport

Accepted for issues [421](https://github.com/vianetwork/via-core/issues/421) and
[422](https://github.com/vianetwork/via-core/issues/422), 2026-10-23.
[ADR 0017](../adr/0017-bound-signing-transport-without-changing-admitted-evidence.md) records the transport/evidence
boundary. [Testnet rehearsal 423](https://github.com/vianetwork/via-core/issues/423) remains separate from source design
and validation.

## Problem and ownership

Full parents appeared in both the proposal and admitted content, as nested JSON numeric arrays. The coordinator bounded
the inner response while the signer bounded the larger authenticated envelope. A valid witness-heavy parent could
therefore strand a round after durable exposure.

The coordinator crate owns transport for both node roles. New proposals use the existing parent-fetching authorization
fallback, which checks txids, prevouts, amounts, scripts and Taproot ALL messages. Selection, fees, the DAL's final
admission checks, exact cached contributions and holds remain with their existing owners. Watcher, sender and DA
siblings have no second signing-envelope implementation. The MuSig2 example server has a different protocol.

## Research and copy fit

Exa discovery covered FROST/custody coordinators, Fedimint, Liquid, sBTC, LDK/Core, JSON-RPC/protobuf/gRPC and
Kubernetes. The final hunt split the remaining integration:

1. “Rust Axum signed request capability offer per request response serialization legacy default format negotiation”
2. “Kubernetes apiserver NegotiateMediaTypeOptions missing Accept supported default tests source”
3. “Rust serde legacy compact same model HTTP wire DTO Base64 borrowed projection serde_json RawValue”
4. “serde_with SerializeAs DeserializeAs borrowed remote derive Base64 legacy representation tests”
5. “Fedimint API negotiation client bootstrap reconnect version cache lifecycle tests”
6. “Lightning LDK feature negotiation no persistent capability state optional features init tests”

Every selected donor was read from a pinned local study clone, including code and tests. Context7 was quota-exhausted.
No local donor suite execution or donor-wide green CI result is claimed.

| Part                   | Exact source and inspected tests                                                                                                                                                                                                                                                                                       | Fit                                                                                                                                                                       |
| ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Parent references      | [Core PSBT](https://github.com/bitcoin/bitcoin/blob/152a55397e451fed9c10041bddf01aa6839847d5/src/psbt.cpp),760–796; [BIP371](https://github.com/bitcoin/bips/blob/c0c5aea41e6a334d34db4ca87ae53bbfd7f268d1/bip-0371.mediawiki), UTXO types                                                                             | Partial mechanism: omit NEW Taproot ALL parents, retain independent Via verification/history. No PSBT migration.                                                          |
| Byte codec             | [serde_with 1.14 Base64](https://github.com/jonasbb/serde_with/blob/8c59954436b139b238dc23830946f04a3e331862/serde_with/src/base64.rs),77–133; tests/base64.rs:23–80                                                                                                                                                   | Unchanged library. Port padding/alphabet/fixed-length cases. HTTP remote derive leaves Binding bincode untouched.                                                         |
| Final writer           | [jsonrpsee 0.23.2 BoundedWriter](https://github.com/paritytech/jsonrpsee/blob/c6344f9ee57229ca182956c408b0f84d49ba56ea/core/src/server/helpers.rs),35–83,181–196                                                                                                                                                       | Unchanged library through workspace server-core.                                                                                                                          |
| Whole-frame sizing     | [sBTC packaging](https://github.com/stacks-sbtc/sbtc/blob/fb469a32a48f47d2a6e46e13e72c944ab2c96755/signer/src/bitcoin/packaging.rs),13–95,1354–1389; network/libp2p/swarm.rs:1214–1278                                                                                                                                 | Own Via arithmetic around the actual snapshot serializer; transfer signed-wrapper accounting and behavioral tests, not request-dropping policy.                           |
| Per-request output     | [apiserver negotiation](https://github.com/kubernetes/apiserver/blob/62c14eb1e5291f23d56d4610bba00c41b6a3b771/pkg/endpoints/handlers/negotiation/negotiate.go),49–99,269–301; negotiate_test.go:90–175                                                                                                                 | Partial pattern: absent offer keeps old writer, output choice independent of input. Go media/quality registry does not fit a signed scalar; own exhaustive selector.      |
| Reader-first rollout   | [client-go format gates](https://github.com/kubernetes/client-go/blob/6f4e5e8e847a0abdb361d3c7d93c3643f45c837f/rest/client.go),280–336; request_test.go:2180–2225                                                                                                                                                      | Mostly lifecycle fit: readers precede preferred writes, decode actual response. Do not port 415 mutation fallback.                                                        |
| Capability lifecycle   | [LDK init](https://github.com/lightningdevkit/rust-lightning/blob/e37d406f8d7e5e26f9420a1eabf8ff9b3c28dee0/lightning/src/ln/peer_handler.rs),2130–2175,4174–4204; [Core feature ordering](https://github.com/bitcoin/bitcoin/blob/152a55397e451fed9c10041bddf01aa6839847d5/src/net_processing.cpp),3972–4001,4197–4233 | Partial pattern: authenticated offer before use, local state, reconnect handshake. Socket/feature registries do not replace Via's cached nonce/share lifecycle.           |
| API versus consensus   | [Fedimint versions](https://github.com/fedimint/fedimint/blob/3e99fa9d1cc3f042ded02b1c8f31af22ebbdfc42/fedimint-core/src/module/version.rs),1–55,218–340; client-module discovery:1–53,55–184                                                                                                                          | Partial pattern: keep API version separate. Greatest compatible peer count cannot guarantee every required Via signer; no discovery/cache port.                           |
| Bounded dual decode    | [serde_json 1.0.133 RawValue](https://github.com/serde-rs/json/blob/0903de449c52c1b4a2271e909b7afb18909dc379/src/raw.rs),39–79; tests/test.rs:2335–2365                                                                                                                                                                | Unchanged library plus nearly verbatim borrowed-field pattern; port whitespace/roundtrip cases. Select complete DTO before typed decode, without untagged heap buffering. |
| Offer parsing          | [Axum 0.7.9 Query](https://github.com/tokio-rs/axum/blob/9983bc1da460becd3a0f08c513d610411e84dd43/axum/src/extract/query.rs),70–97,110–200; path_router.rs:334–366                                                                                                                                                     | Unchanged typed extractor. Path routing excludes query; OriginalUri preserves the signed target. Adapt missing/valid/invalid examples and duplicate-scalar tests.         |
| Core process/streaming | [corepc 0.43.2](https://git.rust-bitcoin.org/rust-bitcoin/corepc/src/commit/eca585605f6fbf79a1c8ce81ce86e2e4205cdf31/bitcoind/src/lib.rs),293–345,596–615,697–790; [Tokio iter](https://github.com/tokio-rs/tokio/blob/67355c6d2316d58a4cf6fa822268b781d3bec402/tokio-stream/src/iter.rs),33–61 and stream_iter tests  | Unchanged libraries. Pinned Core 28.2, private process/datadir/ports; existing tokio-stream 0.1.17 for chunked fixture replies. No own process manager or Stream.         |

These donors qualify through established use or named Core/LDK, Fedimint, Kubernetes, serde/tokio, jsonrpsee and corepc
maintainers. Licenses were read; no substantial external source body is copied. Liquid's public message file is
historical AGPL comparison only. sBTC height activation changes signing/wallet protocol. Protobuf binary evolution does
not convert JSON arrays to strings; gRPC codec fallback tests rename identical proto bytes. A new LSP initialize
endpoint cannot reach old coordinators.

The last hunt cloned bltavares/axum-content-negotiation@5762a48f760d65de652cbefda8d03c25d266fbc2. Its
src/lib.rs:220–260/tests479–523 and default cases use unsigned Accept, CBOR/JSON and Axum 0.8; no established downstream
use was verified. Rejected as a dependency/donor. No whole donor implements Via's admitted authority and nonce
lifecycle. Selector, projections, capacity policy and iteration preference remain explicit Via integration, with pinned
pattern comments.

## Compatibility and removal

L is the baseline peer. R is the new binary with compact writes disabled, the default. W enables writes. All new readers
accept coherent legacy and compact forms.

| Coordinator / signer | L      | R      | W                                |
| -------------------- | ------ | ------ | -------------------------------- |
| L                    | Legacy | Legacy | Legacy after verified bootstrap  |
| R                    | Legacy | Legacy | Legacy                           |
| W                    | Legacy | Legacy | Compact after verified bootstrap |

Every snapshot bootstrap is legacy-encoded. W offers transport_version=2 in the signed target query. A W coordinator
selects compact per request; no offer or disabled writes selects legacy. Only the authenticated compact snapshot's inner
transport_version=2 selects compact follow-up writes. Auth version 1 and sequencer version retain their meaning. No
persistent capability cache/global session format. In-flight compact requests remain readable when writes are disabled.

Legacy duplicates exact admitted proposal bytes; compact omits the duplicate. New readers hash before bincode, compare
the legacy duplicate and independently authorize the transaction. Failures end that iteration. The next normal iteration
starts legacy bootstrap with a fresh challenge and exact cached contributions. No automatic re-encoding or mutation
replay follows timeout, parse/auth failure or 415.

Retire V1 only after every configured/offline/standby/rollback peer is capable, old images cannot re-enter without
explicit operator action, legacy work has drained, and active/public recovery records are compact-servable with exact
bytes or explicitly reconciled with holds preserved. Match participant/wallet configuration and rehearse mixed
operation/rollback. Silence and timers are insufficient. Remove HTTP V1, write flag/fallback and legacy admission budget
together in a later reviewed PR; keep persisted decoders. Capability/epoch admission is deferred; compact-only new
capacity waits for removal.

## Capacity and behavioral evidence

Final body limit: 8,388,608 bytes, inclusive. Empty-body envelope H≤65,536 includes delimiters, binding, signature and
syntax; validate it before handler mutation. NEW admission must pass both complete future bounds. Checked
input/participant arithmetic counts future public maps around actual snapshot serialization. Final bounded serialization
independently guards every accepted reply and signed fallback.

Historical records are checked per requested format, without globally requiring legacy fit on restoration. Compact-only
recovery may succeed while legacy receives signing_round_unservable/503 with holds kept. A new client cannot enlarge an
unchanged OLD coordinator's legacy capacity.

| Acceptance | Owner tests                                                                                                                                                            |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A1         | bounded_signing_valid_heavy_parent_downloads_signs_and_recovers: valid near-400 kB Core parent, real HTTP signing, fallback, child Core acceptance and public recovery |
| A2         | bounded_signing_admission_refuses_before_reserving_real_requests_and_inputs: nearby success, real DB counts/refusal/counter; full-future/dual-budget cases             |
| A3         | Heavy-parent test rejects unavailable/wrong parents at coordinator and signer, advances after restoration                                                              |
| A4         | bounded_signing_historical_parent_bytes_recover_compactly_and_keep_legacy_holds: old output exceeds cap, exact content/public shares/final bytes, holds kept           |
| A5         | bounded_signing_actual_http_final_limit_and_signed_size_fallback; bounded_signing_actual_client_counts_chunked_bytes_before_decode: limit−1/equality/+1                |
| A6         | Base64/RawValue cases, independent binary goldens, digest before real admission, matching-digest expected/payment substitution refusal                                 |
| A7         | All nine peer directions, frozen real-old coordinator path, mixed heavy round, signed query tamper/duplicates, lost acknowledgments, writer disable/old rebootstrap    |
| A8         | GET/creation/nonce/share acknowledgments/errors, both writers, signed oversized success/error fallback, max-width/escaped metadata equality/refusal                    |

The old oracle freezes baseline ab8e9166426b23df14ec2fbe32524136a4a08c6c handlers, middleware, withdrawal producer,
snapshot DTO and errors. Production never uses it. Independent Python little-endian bincode goldens protect
auth/admitted bytes from transport annotations.

The exported counter is via_verifier_coordinator_signing_size_refusals, without \_total, with only
stage=admission|serving|response. Alert: sum(increase(via_verifier_coordinator_signing_size_refusals[5m])) > 0. External
activation and fired-alert rehearsal remain in 423.
