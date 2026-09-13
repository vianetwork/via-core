This library contains multiple modules that provide different functionalities for the sequencer/verifier node.

## Modules

1. **client**: provides communication tools with the Bitcoin network. (broadcast, get block, get transaction
   confirmation, etc.)
2. **inscriber**: provides tools for creating, signing, and broadcasting inscriptions transactions.
3. **indexer**: provides tools for fetching and parsing Bitcoin blocks. (filter inscriptions transactions, get specific
   block inscriptions messages, etc.)
4. **transaction_builder**: provides tools for creating unsigned transaction for withdrawal (UTXO selection).
5. **signer**: provides tools for signing transactions.

## Responsibilities of shared files

- **traits.rs**:
  - contains traits.
  - these traits should be implemented by modules.
  - some of the modules are dependent to each other, we can use these traits to accept another module instance as a
    parameter and use its functions.
- **types.rs**:
  - contains types that are shared between modules. (like inscription types, inscription messages, etc.)
  - these types should be used by modules.
  - result and custom errors are defined here.
  - Bitcoin Specific types are defined here.
  - and data structure related to bitcoin like address, private key, etc should have their own type from community
    standards library.
  - data validation, serialization functions should be implemented here.
- **lib.rs**:
  - contains the public interface of the library.
  - this file should be used for re-exporting the modules.

## Internal Dependencies

- Inscriber depends on Client and Signer modules and should accept them as parameters in the constructor.
  - for broadcasting transactions, fetching UTXOs, setting valid fee, etc Inscriber should use Client module.
  - for signing transactions, Inscriber should use Signer module.
- Indexer depends on Client module and should accept it as a parameter in the constructor.
  - for fetching blocks Indexer should use Client module.
- TransactionBuilder depends on Client module and should accept it as a parameter in the constructor.
  - for fetching UTXOs, setting valid fee, etc TransactionBuilder should use Client module.

## Usage

Check [README.md](./README.md) for usage examples.

## Testing

Unit tests should be implemented for each module in their own file.

For checking the integration and seeing the result of the whole system, we can use the `tests` directory. This directory
is binary and we can import the library and use it in the main function to see the result of the functions.

For running the example, use the following command:

`cargo run --bin via_btc_test`

## Development

Before starting implementation of every module, we should define or modify the module's trait in the `traits.rs` file.
And also define or modify the types that are shared between modules in the `types.rs` file.

It's possible that these two file contain trait or type that they are not accurate or needed, don't hesitate to modify
or remove them.

Write unit tests for each module in their own file.

Write integration tests in the `examples` directory.

**Note:**

- Only make methods public that are needed by external users.

## Deposit decoding

The [domain glossary](../../../CONTEXT.md) defines bridge payments, deposit messages, accepted deposits,
and rejected bridge payments.

The shared [`MessageParser`](src/indexer/parser.rs) supports two deposit encodings:

| Encoding | Receiver | Contract address | Call data |
| --- | --- | --- | --- |
| Inscription | Exactly 20 bytes | Exactly 20 bytes | Bytes in the call data push |
| OP_RETURN | First 20 bytes of the first data push | Zero address | Empty |

A plain inscription deposit encodes the contract address as 20 zero bytes and call data as an empty push.

`MessageParser::parse_op_return_deposit` selects the first output whose script begins with OP_RETURN. The next
instruction must be a complete data push with at least 20 bytes. The parser uses `Script::instructions()` to decode
direct pushes and PUSHDATA1, PUSHDATA2, and PUSHDATA4 encodings, including nonminimal pushes.

A push instruction's shortest form depends on the data length. For 1 to 75 bytes the opcode byte is the length
itself, so the shortest form for a 20-byte receiver begins `6a 14`. Longer forms can encode the same payload.
The length ranges below describe the shortest form. The last three examples deliberately use longer forms.

| Shortest form for this data length | Encoding | A 20-byte receiver |
| --- | --- | --- |
| 1 to 75 bytes | one opcode byte equal to the length, then the data | `6a 14 <20 bytes>` |
| 76 to 255 bytes | `OP_PUSHDATA1`, a 1-byte length, then the data | `6a 4c 14 <20 bytes>` |
| 256 to 65,535 bytes | `OP_PUSHDATA2`, a 2-byte length, then the data | `6a 4d 14 00 <20 bytes>` |
| larger | `OP_PUSHDATA4`, a 4-byte length, then the data | `6a 4e 14 00 00 00 <20 bytes>` |

All four forms decode to the same receiver.

The pushed payload excludes the opcode and its length bytes. Its first 20 bytes are the receiver, in their original
order. Remaining bytes inside that push are ignored. Instructions after that push are not inspected, even if they
are malformed. A truncated first push, a first push shorter than 20 bytes, or a non-push first instruction produces
no OP_RETURN deposit. Bytes from later instructions cannot complete a short first push.

Payloads beginning with the active withdrawal, protocol-upgrade, or wallet-update prefixes remain excluded.
Additional OP_RETURN outputs do not change selection, and an invalid first output does not cause a search for a
later deposit. Inscription deposits and withdrawals are decoded independently and retain their message order.

The [OP_RETURN deposit example](examples/deposit_opreturn.rs) produces `6a14<receiver20>`: OP_RETURN, a minimal
20-byte push, and the receiver. Payloads longer than 20 bytes are eligible under the rules above, whatever their
length or push encoding. Bytes after the receiver do not supply call data or any other deposit field.

[ADR 0003](../../../docs/adr/0003-decode-op-return-deposit-pushes.md) records this decision, why the previous rule
was wrong, and the compatibility gate.

An invalid deposit encoding produces no message for that encoding. The parser preserves other successfully
decoded messages and continues processing later transactions. Rejection does not reverse the Bitcoin payment.

Decoding alone does not establish acceptance for L2 processing. The indexer requires the decoded amount to equal
the total paid to the bridge. [`ViaL1Deposit::l1_tx`](../types/src/l1/via_l1.rs) rejects reserved receiver addresses
and amounts below its fee requirement before producing an `L1Tx`.

The `ViaL1Deposit` conversion uses the receiver as the `L1Tx` destination and sets its call data to empty.
The inscription's `l2_contract_address` and `call_data` are not used in the resulting transaction.

See [Bitcoin deposits and payout signing](../../../docs/via_guides/bridging.md) for the execution flow and
[L2 contract calls through Bitcoin](../../../docs/via_guides/bridging-proposals.md) for the earlier proposals and
unresolved design decisions.

## Taproot Script witness data for via inscription standard

```
Witness Structure for each message type
in our case da_identifier is b"celestia"

(1)
System Bootstrapping Message (txid should be part of genesis state in verifier network)
Sender : Could be anyone
Votable : No
|-------------------------------------------------------------|
|      Schnorr Signature                                      |
|      Encoded Verifier Public Key                            |
|      OP_CHECKSIG                                            |
|      OP_FALSE                                               |
|      OP_IF                                                  |
|      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')"    |
|      OP_PUSHBYTES_32  b"Str('SystemBootstrappingMessage')"  |
|      OP_PUSHBYTES_32  b"start_block_height"                 |
|      OP_PUSHBYTES_32  b"protocol_version"                   |
|      OP_PUSHBYTES_32  b"Str('bootloader_hash')"             |
|      OP_PUSHBYTES_32  b"Str('abstract_account_hash')"       |
|      OP_PUSHBYTES_32  b"Str('snark_wrapper_vk_hash')"       |
|      OP_PUSHBYTES_32  b"Str('evm_emulator_hash')"           |
|      OP_PUSHBYTES_32  b"Str('governance_address')"          |
|      OP_PUSHBYTES_32  b"Str('sequencer_address')"           |
|      OP_PUSHBYTES_32  b"bridge_musig2_address"              |
|      OP_PUSHBYTES_32  b"verifier_1_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_2_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_3_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_4_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_5_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_6_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_7_p2wpkh_address"          |
|      OP_ENDIF                                               |
|-------------------------------------------------------------|


(2)
OP_1 means ok or valid
OP_0 means not ok ok or invalid
reference_txid could be the proof_reveal_txid or other administrative inscription txid

ValidatorAttestationMessage
Votable: No
|-------------------------------------------------------------|
|      Schnorr Signature                                      |
|      Encoded Verifier Public Key                            |
|      OP_CHECKSIG                                            |
|      OP_FALSE                                               |
|      OP_IF                                                  |
|      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')"    |
|      OP_PUSHBYTES_32  b"Str('ValidatorAttestationMessage')" |
|      OP_PUSHBYTES_32  b"reference_txid"                     |
|      OP_PUSHBYTES_1   b"OP_1" /  b"OP_0"                    |
|      OP_ENDIF                                               |
|-------------------------------------------------------------|


(3)
L1BatchDAReference
Votable: No
Sender Validation: only valid sequencer
|----------------------------------------------------------|
|      Schnorr Signature                                   |
|      Encoded Sequencer Public Key                        |
|      OP_CHECKSIG                                         |
|      OP_FALSE                                            |
|      OP_IF                                               |
|      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')" |
|      OP_PUSHBYTES_32  b"Str('L1BatchDAReferenceMessage')"|
|      OP_PUSHBYTES_32  b"l1_batch_hash"                   |
|      OP_PUSHBYTES_32  b"l1_batch_index"                  |
|      OP_PUSHBYTES_32  b"celestia"                        |
|      OP_PUSHBYTES_2   b"da_reference"                    |
|      OP_ENDIF                                            |
|----------------------------------------------------------|

(4)
ProofDAReferenceMessage
Votable: Yes
Sender Validation: only valid sequencer
|----------------------------------------------------------|
|      Schnorr Signature                                   |
|      Encoded Sequencer Public Key                        |
|      OP_CHECKSIG                                         |
|      OP_FALSE                                            |
|      OP_IF                                               |
|      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')" |
|      OP_PUSHBYTES_32  b"Str('ProofDAReferenceMessage')"  |
|      OP_PUSHBYTES_32  b"l1_batch_reveal_txid"            |
|      OP_PUSHBYTES_32  b"celestia"                        |
|      OP_PUSHBYTES_2   b"da_reference"                    |
|      OP_ENDIF                                            |
|----------------------------------------------------------|


(5)
L1ToL2Message
Votable: No
Sender Validation: anyone
|-------------------------------------------------------------|
|      Schnorr Signature                                      |
|      Encoded USER/Admin Public Key                          |
|      OP_CHECKSIG                                            |
|      OP_FALSE                                               |
|      OP_IF                                                  |
|      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')"    |
|      OP_PUSHBYTES_32  b"Str('L1ToL2Message')"               |
|      OP_PUSHBYTES_20  b"receiver_l2_address"                |
|      OP_PUSHBYTES_20  b"l2_contract_address"                |
|      Data push       b"call_data"                           |
|      OP_ENDIF                                               |
|-------------------------------------------------------------|

(6)
SystemContractUpgrade
Votable: No
Sender Validation: governance
|-------------------------------------------------------------|
|      Schnorr Signature                                      |
|      Encoded USER/Admin Public Key                          |
|      OP_CHECKSIG                                            |
|      OP_FALSE                                               |
|      OP_IF                                                  |
|      OP_PUSHBYTES_32  b"version"                            |
|      OP_PUSHBYTES_32  b"bootloader_code_hash"               |
|      OP_PUSHBYTES_32  b"default_account_code_hash"          |
|      OP_PUSHBYTES_32  b"recursion_scheduler_level_vk_hash"  |
|      OP_PUSHBYTES_32  b"system_contract"                    |
|      OP_PUSHBYTES_32  b"system_contract"                    |
|      OP_PUSHBYTES_32  b"system_contract..."                 |
|      OP_ENDIF                                               |
|-------------------------------------------------------------|

```
