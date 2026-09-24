# Coordinator request authentication

Status: research; request and retry contract pending. Evidence inspected on 2026-09-23 and extended on 2026-09-24. No implementation, public disclosure, or rollout is authorized. Restricted findings and candidate reconciliation are retained separately.

The question is what one verifier's signature authorizes. A signature over data, a verified caller identity, and permission to mutate a signing session are separate checks. A complete design must agree on all three at the client and server.

## Current ownership and representation

Via source is pinned to `8a49f355bfe31720f21b8181db471243709194e5`. `auth::sign_request` serializes a supplied value with `serde_json::to_vec`, hashes it with SHA-256, signs with secp256k1 ECDSA, and encodes the compact signature in base64. `verify_signature` reconstructs the digest and checks it with the supplied public key. These generic functions establish the signature mechanism; their callers determine the meaning of the signed value. [Cryptographic helper][auth].

The client owner is `ViaWithdrawalVerifier::create_request_headers` and its request methods. The server owner is `auth_middleware`, which reads verifier identity and version headers, selects a verifier public key, checks time bounds, verifies a signed value, and checks protocol-version eligibility before routing. `validate_timestamp` restricts syntax and range. `check_timestamp_skew` handles request age and a separate future-clock bound. These existing checks are compatibility and behavior requirements for any client/server cutover, not incidental code to replace wholesale. [Client][client] · [Middleware][middleware].

Session state belongs to `SigningSession`, with `session_op`, `received_nonces`, `received_sigs`, and `created_at`. `NoncePair` and `PartialSignaturePair` contain `signer_index` plus encoded data; input indices are map keys. `new_session`, `submit_nonce`, and `submit_partial_signature` own different transitions. The partial-signature path also verifies a MuSig2 share against the message, key set, and nonce transcript. HTTP authentication must not be confused with that cryptographic share verification. [Types][types] · [Session handlers][handlers].

The client library's `test_signature_verification` checks a generic payload and wrong public key. The middleware includes timestamp tests. These are useful local examples, but they do not establish an end-to-end HTTP method, URI, body, principal, and session contract. This note did not run them or claim an exhaustive test inventory.

## Proposed request identity

A request should authorize one operation by one member of one verifier set against one intended coordinator and signing round. The following fields are a candidate contract, not a mandated wire schema:

- A domain and authentication-format version, separate from the sequencer protocol version.
- Network or chain identity, intended coordinator audience, and verifier-set identity or epoch where key membership can change.
- The HTTP method and the exact agreed request target, including any meaningful query parameters.
- A digest of the exact transmitted body bytes, with a defined empty-body value and content interpretation.
- Authenticated signer identity, request issue/expiry bounds, and an operation identifier where retries require one.
- For mutations, the session or round identifier and the approved proposal identity.

The protocol must choose raw-byte signing or a specified canonical serialization. Raw-byte hashing is the smaller fit for a client that serializes once and sends those same bytes. JSON object equality is not byte equality: whitespace, duplicate keys, numeric forms, and key order need deliberate treatment. Parsing and reserializing independently on each side does not define cross-language canonical JSON.

Method and request target binding prevent an authorized payload from acquiring a different operation meaning. The target must be defined at the same layer on both sides. A reverse proxy can strip a path prefix, normalize encoding, or change the host. An untrusted forwarded header must not select the verification target. A fixed logical audience may be more stable than a physical host name, but that is a deployment decision.

The middleware should produce a typed authenticated principal for the handler. If the request body still carries `signer_index`, every entry must match that principal and the active verifier-set mapping. Alternatively, the handler can derive the slot from the principal and remove the redundant body field in a versioned cutover. A valid request signature is not authorization to fill another signer's slot.

A bounded body-size policy and complete parsing precede mutation. Missing digest input, unsupported format, duplicate or malformed fields, wrong signer, wrong method or target, stale round, and invalid indices should fail without changing session state. A session lock alone does not establish this atomicity if a multi-entry request mutates earlier entries before a later entry fails.

## Retries are a state-machine contract

A timestamp limits the interval in which a request is acceptable. It does not make the request single-use, and a body hash does not prevent replay of the same body. The HTTP request identifier and the MuSig2 public nonce have different meanings; using the word nonce for both does not make them interchangeable.

A minimal proposed mutation identity is `(round, authenticated signer, input, operation)`. The payload digest determines whether a repeat is identical or conflicting. For a multi-input submission, all entries need a coherent all-or-nothing decision.

An identical retry should return the previously accepted result without replacing a nonce, creating another signature, or resetting the round. A different nonce or share for an already occupied identity should be a conflict, not a replacement. A request for an abandoned round must not mutate its successor, even when the transaction proposal happens to be the same. A fresh timestamp on a retry does not create new authority to overwrite an existing operation.

The signer should cache a produced partial signature for retransmission rather than rerun signing after a lost response. Secret-nonce consumption and persistence belong to the local signer, while public nonce and share acceptance belong to the coordinator. Their round identifiers must agree. [Current client signer owner][client] · [BIP327][bip327].

BIP327 permits application context such as a session identifier in nonce generation's `extra_in`,
but describes that protection as defense in depth. It does not replace the rule against signing
twice with the same secret nonce. A proposal digest alone also cannot distinguish fresh attempts
over the same proposal. Restart-safe round identity and retained responses remain separate design choices.

An invalid share also needs an explicit mutation policy. Authentication failure should not reset unrelated work or attribute failure to a body-selected signer. An authenticated invalid share may justify aborting the round, but that is a distinct, attributable protocol decision. Restart and coordinator failover require either retained operation results and round identities or an unambiguous new epoch that makes stale submissions ineffective. An in-memory replay cache alone cannot establish restart safety.

At the inspected Via revision, `submit_partial_signature` calls `reset_session` when share verification
fails, then returns a bad-request error. A new request contract must deliberately retain or replace
that abort behavior. Rejecting a request without mutation and aborting its signing round are different
outcomes. [Current session handler][handlers].

## Comparable request-signing implementations

### AWS SigV4 binds the HTTP operation and credential scope

In botocore 1.40.0, commit `a3bbf61a0a3548c6bc7b68dd0a23bb6242a8e630`, `SigV4Auth::canonical_request` includes the uppercase method, normalized path, canonical query, canonical signed headers, and body checksum. `string_to_sign` adds time and credential scope; `signature` derives a key scoped to date, region, and service. `add_auth` removes previous authorization before signing a retry. [Botocore implementation][botocore].

This is a concrete reason to bind request semantics rather than only body content. It does not supply application idempotency. The same file includes `UNSIGNED-PAYLOAD` and service-specific exceptions, including S3 path handling. Those exceptions depend on AWS protocol rules and must not become Via defaults. SigV4 uses shared-secret HMAC credentials, not Via's verifier public keys.

### Matrix passes the authenticated origin to the handler

In Synapse v1.138.0, commit `fcffd2e897aaef1583bcb1f93893f254330ec81c`, `Authenticator::authenticate_request` constructs a signed object containing `method`, `uri`, `destination`, `origin`, and optional `content`. It verifies through `keyring.verify_json_for_server`, sets `request.requester`, and returns the authenticated origin. `BaseFederationServlet::_wrap` passes that origin into the endpoint handler. [Synapse federation authentication][synapse].

The transferable mechanism is principal propagation after verifying the operation and audience. It prevents handler code from needing to rediscover who authenticated. Matrix's server-name key discovery, JSON signing, and federation transaction semantics differ from Via's fixed verifier key set and MuSig2 rounds. This inspected file does not prove a complete replay or exactly-once policy for Matrix; those are separate protocol mechanisms there too.

### CometBFT preserves the result of an already authorized signing operation

At `f4d73cd5a091a997d1f040850710b6937e650125`, `FilePVLastSignState::CheckHRS` rejects height, round, or step regression. `FilePV::signVote` compares current sign bytes with retained sign bytes and returns the previous signature for an identical operation. Conflicting data is rejected. `saveSigned` persists the result before returning it. [CometBFT validator][comet].

That is a useful restart and retry contract even though it is not HTTP authentication. Its allowed timestamp-only equivalence is specific to canonical consensus votes, and vote-extension signatures have separate behavior. Via must not copy that exception into an exact Bitcoin message/transcript binding. CometBFT also does not manage MuSig2 secret nonces in this code.

## Applicable standards and their limits

[RFC 9421, HTTP Message Signatures][rfc9421], provides a vocabulary for covered components such as `@method`, `@target-uri`, signature creation/expiry, key identity, and nonce parameters. A Via-specific profile would still have to choose required components, algorithm, key resolution, replay handling, and proxy interpretation. Citing the RFC alone does not make a custom header scheme conformant.

[RFC 9530, Digest Fields][rfc9530], defines content digest representation. A digest provides integrity only when the digest itself is authenticated and checked against the relevant bytes. It supplies neither signer-slot authority nor application idempotency. [RFC 8785, JSON Canonicalization Scheme][rfc8785], is relevant only if canonical JSON is selected instead of signing the transmitted bytes; ordinary `serde_json` serialization is not a claim of JCS compliance.

[BIP327][bip327] governs MuSig2 signing and nonce use, not HTTP serialization or coordinator replay storage. [EIP-712][eip712] supplies typed-data domain separation for Ethereum applications, but is not an HTTP request authentication standard and is not a reason to change Via's Bitcoin-key signature protocol.

[The Idempotency-Key Internet-Draft, revision 07][idempotency-draft], distinguishes an identical
completed retry, a concurrent retry, and reuse of a key with a different payload. It proposes returning
the previous result for the first case and separate errors for the other two. It also requires the
resource to define key lifetime and expiry policy. This is work in progress, not an RFC; the cited
revision lists an expiry of 18 April 2026.

The useful comparison is a documented operation identity with explicit retry behavior. It does not
authenticate that identity, prevent MuSig2 secret-nonce reuse, or make a proposal hash identify a
fresh signing attempt. It complements HTTP Message Signatures rather than replacing request binding.

## What newer local zkSync does and does not provide

The inspected newer local zkSync is `ff5f519b11cff863edcfa0f75af10fea113806b0`, not verified latest upstream. `OperatorSigner` implements `EthereumSigner` with local or GCP KMS keys. `sign_typed_data` accepts an `Eip712Domain`; `sign_transaction` derives a chain-aware transaction hash before signing. This separates key custody from the data's domain, but it does not authorize coordinator HTTP mutations. [Newer operator signer][zk-signer].

The consensus registry adapter decodes validator keys and proof-of-possession values, then verifies the proof against the key in `decode_weighted_validator`. That validates a different identity relationship, not permission to write a Via signer slot. The external-node consensus path also checks configuration transitions. [Newer registry owner][zk-registry] · [Newer external-node consensus][zk-en].

No drop-in Via coordinator REST authentication or MuSig2 round implementation was established in these inspected newer modules. The closest useful extension points are the signer/key adapter, typed domain support, node-framework wiring, and explicit validator configuration. Via's request profile and coordinator state machine remain Via-owned. This is a bounded source comparison, not a claim that no other upstream authentication mechanism exists.

## Compatibility and the minimal coherent handoff

The minimal handoff has three owners that must change together: client request construction, middleware verification, and mutation handlers.
The middleware also supplies the authenticated principal.
Adding a body digest in isolation does not settle this contract.
Any existing proposal needs comparison with the complete contract before replacement or new implementation.

A clean versioned cutover is preferable to silently accepting both formats for the same mutation. If a transition period is chosen, its expiry, allowed endpoints, and per-version guarantees need an explicit decision. An unauthenticated downgrade or opportunistic fallback would defeat the stronger format. Changing only the server can stop old clients; changing only clients can leave guarantees unenforced. Sequencer protocol version alone need not identify the HTTP authentication format.

The proposed acceptance evidence consists of observable outcomes: altered body, method, URI, audience, signer slot, or round is rejected without mutation; identical delivery is idempotent; conflicting delivery is rejected; malformed later entries do not leave earlier writes; expired and too-far-future requests fail; stale requests after restart cannot affect a new round; and a lost response does not cause another signing operation. Mixed client/server versions need an explicit compatibility result. These are requirements for a future candidate, not claims of tests run here.

## Re-fork requirements and remaining choice

The stable Via contract is the exact signed request meaning, authenticated-principal propagation, signer-slot ownership, same-operation retry behavior, and round-bound one-time signing. Current owners are `auth.rs`, `auth_middleware.rs`, `api_impl.rs`, coordinator types, and the verifier HTTP client. Axum extractors, reqwest request construction, and node-framework configuration are replaceable adapters, but their byte and routing behavior is observable protocol behavior.

The contract depends on verifier-set ordering, bridge key aggregation and tweak, network identity, and local session lifetime. Reusing newer zkSync key types or node wiring must not change those meanings. Golden signed-request bytes and verifier results should include empty bodies, map ordering, URI encoding, query handling, proxy prefixes, and authentication-version rejection. Round transcripts and restart records need their own evidence; HTTP golden vectors cannot prove nonce safety.

The human decision remains raw bytes versus a canonical format, domain and audience fields, round-ID authority, durable retry retention, atomic mutation policy, invalid-share abort policy, and cutover coordination. No live exposure or exploitation conclusion follows from this source research. No tests, builds, runtime requests, or deployment checks were performed.

## Concrete lifecycle recommendations from the second research pass

The following S1-S6 recommendations remain proposals. They do not change the accepted withdrawal
guarantee, fee policy, or separation of expected obligations and observed payments.

### S1: Choose a byte profile, not only a digest header

RFC 9421 appendix B.2.6 signs `date`, `@method`, `@path`, `@authority`, `content-type`, and
`content-length`, with `created` and `keyid`, using Ed25519. That example does not cover a body digest
or query. Appendix B.2.2 covers a digest and one named query parameter, but not every operation
component. Section 2.4 includes a request example covering method, authority, path, full query,
content digest, content type, and content length. Appendix B.4 demonstrates that adding an uncovered
query parameter preserves signature validity. Copying a published example without its limits is not
a complete application profile. [RFC 9421][rfc9421].

A full Via RFC profile would also require an application `tag`, network, logical coordinator audience,
authentication version, wallet epoch, verifier-set identity, protocol version, bounded creation and
expiry times, and operation identity. The configured key set resolves `keyid`; an arbitrary key URL
must not establish membership. RFC 9421's initial algorithm registry includes P-256 ECDSA, not
secp256k1. Via must not label its current curve `ecdsa-p256-sha256`.

The smaller recommendation remains a versioned raw-body envelope with the existing key mechanism.
Serialize once, hash the transmitted bytes, and specify the envelope's encoding independently of
ordinary JSON serialization. Fixed-order fields with explicit lengths avoid ambiguous concatenation.
Canonical JSON is a viable alternative when independent implementations need equivalent object
spellings, but adds canonicalization rules and allocations. A complete RFC profile adds standardized
header parsing and vectors, but also algorithm and credential compatibility work.

Use one bounded body buffer; hashing an existing byte buffer does not require copying it into another
vector. Missing extraction is an error, not an empty-body default. Preserve current timestamp syntax,
range, future-skew, and protocol-version checks. Reject unexpected queries if the endpoint contract
does not use them. Authenticate the routing target at an agreed proxy layer, and retain authenticated
transport for responses. Request signatures alone do not authenticate session responses.

Future evidence must distinguish changed method, query, audience, body bytes, key epoch, principal,
and operation. Include duplicate JSON fields, empty bodies, missing extraction, clock bounds, and
mixed format versions. S2-S4 determine what a correctly authenticated mutation may do.

### S2: Allocate a durable attempt separately from its proposal

Recommend that the existing coordinator owner allocate `(wallet_epoch, round_sequence)` durably with
the fixed proposal and admitted request membership. A persisted random round ID is also viable, but
requires retired-ID rejection. A timestamp or proposal hash alone is not a fresh-attempt identity.
Neither choice protects an old database restore or two active writers without recovery fencing.

The authorization digest binds the network, ordered verifier keys, aggregate key and tweak, proposal
bytes, ordered input outpoints and prevout amount/script bytes, expected-record references and
revisions, fee interpretation, sighash type, and locally derived per-input messages. An input index
has meaning only within this round and ordering. Every session response identifies its round and
authorization digest. Hashes refer to retained facts; they do not replace local authorization.

This requires one durable allocation/admission transaction, not another ID service. Withdrawal
admission owns request membership and holds. The signing contract does not add canonical-at-signing
validity or fresh `gettxout`. Future proof repeats the same proposal in two attempts, restarts within
one clock second, and changes one prevout, key order, or tweak. Stale traffic must not affect a successor.

### S3: Persist public results and make restart behavior explicit

The actual dependency is `musig2 0.2.1`, with Cargo checksum
`1eaa74fd6a0747bd589b36abce26cd4cd1cd2abedb2fcba26c4eed694dd85487`.
The installed package's `.cargo_vcs_info.json` records upstream revision
`7defa07868ce6bd16609eded23b85c3aec5717b1`; this pass inspected package source, not a separately fetched
upstream checkout. In `rounds.rs`, `FirstRound::finalize(self, ...)` consumes the first round and
`SecondRound::our_signature()` returns the stored contribution. `FirstRound` prevents cloning, but
the lower-level `SecNonce` derives `Clone`. Rust ownership at one API does not establish persistent
nonce safety. [Versioned round source][musig-rounds] · [Versioned nonce source][musig-nonces].

`SecNonceSpices::with_extra_input` can add round, authorization digest, and input index as defense in
depth. BIP327 still prohibits signing twice with one secret nonce. Its special last-signer
`DeterministicSign` is not permission to derive every signer's ordinary nonce from proposal bytes.

Recommend public durable state without persistent secret nonces initially. Record authorization and
public nonce bytes before publication. Freeze the complete ordered peer transcript and verify the
signer's own nonce, participant coverage, and local messages. Durably mark the batch as potentially
signed before consuming any nonce. Persist the complete public share batch before sending it.
Retransmit that batch after a lost response without another signing call.

After restart, a complete cached share batch can be retransmitted. A round with published nonces but
no complete durable share result must be retired. A crash midway through local multi-input signing
also retires that batch. Resuming first-round state instead requires protected secret storage,
exclusive ownership, consumed/result ordering, and backup/restore rules. Encryption alone does not
prevent restoring a reusable nonce. Choose that extra complexity only for an established availability
requirement.

CometBFT's `WriteFileAtomic` uses a same-directory temporary file, `O_SYNC`, and rename. The inspected
helper shows no directory fsync or distributed writer fencing. Its persist-before-response ordering
is useful evidence, not unconditional power-loss or rollback safety. [Atomic writer][comet-atomic].
Future proof must interrupt every durable/publication boundary and observe cached output or retirement,
never another use of the old nonce.

LDK supplies a closer persistence comparison at
`711bcefbcc0999543d9622c030ff7dc8118fc26f`. Its `ChannelMonitorUpdateErr::TemporaryFailure`
freezes channel progress and requires retaining the update before writing newer manager state;
the manager will not regenerate that update. `PermanentFailure` forbids releasing revocation
material. `Watch` requires applied, persisted updates before successful completion. This older
pinned API supports persistence-before-publication and explicit uncertainty. Lightning's force-close
recovery does not transfer: Via cannot recall an exported Bitcoin signature with that mechanism.
[Pinned LDK persistence contract][ldk-watch].

### S4: Reuse the library's slot rule and strengthen the batch boundary

`musig2::Slots::place` makes an identical occupied-slot contribution a no-op, rejects a changed value
with `inconsistent_contribution`, and rejects an out-of-range index. `SecondRound::receive_signature`
verifies before insertion. Transfer these rules to durable HTTP acceptance, not just process memory.

For the current whole-input batch shape, recommend one operation identity
`(round_id, authenticated principal, operation)` and require the complete expected input set.
Each slot remains `(round_id, principal, operation, input_index)`. Avoid arbitrary overlapping
partial batches until a real caller requires them. Decode and check every entry before mutation;
verify against one frozen transcript, then atomically compare slots, commit the batch, and retain its
response. A malformed final entry must not leave earlier entries accepted.

The Idempotency-Key draft distinguishes completed retries, concurrent retries, and changed-payload
reuse. Its suggested 409 and 422 errors are precedents, not mandatory Via status codes. Response
retention may expire; round authority must not reset when it does. Retain terminal identity or a
trusted epoch/high-watermark so an old request cannot become a first request again. A digest alone
cannot reproduce a lost partial-signature response.

[EIP-3076 at a fixed inspected revision](https://github.com/ethereum/EIPs/blob/10bc64e2ea592ac7ec8ee87812ca8a8955a68bac/EIPS/eip-3076.md)
requires lower-bound refusal after importing signing history and warns about unknown gaps between
imports. Its advice for minimal databases retains maxima rather than lowering stored protection.
This offers a compact retirement-watermark pattern when Via uses monotone rounds. It does not
recover forgotten payment attempts or protect a signer whose watermark was also rolled back.
Ethereum's repeat-signing exception is not permission to regenerate a MuSig2 share: return retained
public bytes or refuse the retired round. A watermark replaces old admission authority, not the
result bytes required for a supported retry window.

Future evidence must exercise lost responses, concurrent identical and conflicting batches, retention
expiry, and abort racing with successor creation. Public result persistence adds bounded storage and
one transaction per accepted batch; it does not need a separate replay service.

### S5: Abort an invalid protocol round without revoking payment evidence

The pinned BIP327 version 1.0.4 explicitly permits omitting disruptive-signer identification while
stating that abort itself remains mandatory. Identifiable abort requires authenticated contributions,
honest nonce aggregation, and individual share verification. HTTP authentication alone does not prove
that the coordinator presented the same transcript to all signers. [BIP327][bip327].

Recommend rejecting malformed, unauthenticated, wrong-slot, and stale-round requests without round
mutation. A current, authenticated contribution that fails cryptographic verification against the
frozen transcript terminally aborts that exact round. Preserve the principal, input, contribution
digest, and transcript digest. Replace an unqualified reset with a conditional exact-round transition,
so a delayed error cannot reset a successor.

The library returning an invalid-signature error without changing its slot does not define the
application's abort policy. Excluding one signer cannot complete the unchanged N-of-N aggregate key.
Changing to threshold signing or a governance path changes authorization assumptions and remains
outside this recommendation.

An abort never proves that signatures did not escape and never releases a withdrawal payment hold.
Human policy owns escalation and separately authorized recovery. Future proof distinguishes malformed
HTTP input from a verified invalid contribution, races abort against a successor, and checks that
holds remain after abort.

### S6: Cut over the existing owners together

The minimal operations are allocate/read round, submit nonce batch, read frozen transcript, submit
share batch, read result, and abort exact round. Existing session types, verifier client, middleware,
handlers, `Signer`, and withdrawal DAL own these operations. No general signing framework is required.

Recommended semantic errors distinguish authentication failure, unsupported format, principal mismatch,
invalid request, stale round, conflicting contribution, operation in progress, exact-round invalid-share
abort, and unavailable durable state. A lost response or generic server error is an unknown outcome.
It does not permit a new signing operation. Reauthenticating an identical semantic retry may use a new
timestamp without creating new slot authority.

Reuse existing serialize-once body work and cryptographic helpers, but preserve current timestamp
checks and replace missing-body defaults, absent principal propagation, and unbound round mutation.
Durable admission, round identity, local authorization, nonce lifecycle, and request verification must
all be ready before signing resumes. Client and server format changes need one explicit activation
boundary with no opportunistic fallback. Intermediate changes must remain fail-closed.

The remaining human choices are the byte profile and audience/proxy contract, ID/recovery authority,
restart availability versus secret persistence, response retention, and coordinated cutover ownership.
Future evidence combines S1-S5 scenarios with mixed-version refusal. Source inspection does not supply
historical expected-row clearance, deployment fencing, or signing-activation approval.

[auth]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_verifier_coordinator/src/auth.rs
[client]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_verifier_coordinator/src/verifier/mod.rs
[middleware]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_verifier_coordinator/src/coordinator/auth_middleware.rs
[types]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_verifier_coordinator/src/types.rs
[handlers]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_verifier_coordinator/src/coordinator/api_impl.rs
[botocore]: https://github.com/boto/botocore/blob/a3bbf61a0a3548c6bc7b68dd0a23bb6242a8e630/botocore/auth.py
[synapse]: https://github.com/element-hq/synapse/blob/fcffd2e897aaef1583bcb1f93893f254330ec81c/synapse/federation/transport/server/_base.py
[comet]: https://github.com/cometbft/cometbft/blob/f4d73cd5a091a997d1f040850710b6937e650125/privval/file.go
[zk-signer]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/lib/operator_signer/src/lib.rs
[zk-registry]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/consensus/src/registry/mod.rs
[zk-en]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/node/consensus/src/en.rs
[rfc9421]: https://www.rfc-editor.org/rfc/rfc9421.html
[rfc9530]: https://www.rfc-editor.org/rfc/rfc9530.html
[rfc8785]: https://www.rfc-editor.org/rfc/rfc8785.html
[eip712]: https://eips.ethereum.org/EIPS/eip-712
[bip327]: https://github.com/bitcoin/bips/blob/eba8e50cb66d436c65c6bc8b0a175b643effe9d3/bip-0327.mediawiki
[idempotency-draft]: https://datatracker.ietf.org/doc/html/draft-ietf-httpapi-idempotency-key-header-07
[musig-rounds]: https://docs.rs/crate/musig2/0.2.1/source/src/rounds.rs
[musig-nonces]: https://docs.rs/crate/musig2/0.2.1/source/src/nonces.rs
[comet-atomic]: https://github.com/cometbft/cometbft/blob/f4d73cd5a091a997d1f040850710b6937e650125/internal/tempfile/tempfile.go
[ldk-watch]: https://github.com/lightningdevkit/rust-lightning/blob/711bcefbcc0999543d9622c030ff7dc8118fc26f/lightning/src/chain/mod.rs
