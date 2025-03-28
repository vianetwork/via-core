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
|      OP_PUSHBYTES_32  b"verifier_1_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_2_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_3_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_4_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_5_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_6_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"verifier_7_p2wpkh_address"          |
|      OP_PUSHBYTES_32  b"bridge_musig2_address"          |
|      OP_PUSHBYTES_32  b"Str('bootloader_hash')"             |
|      OP_PUSHBYTES_32  b"Str('abstract_account_hash')"       |
|      OP_ENDIF                                               |
|-------------------------------------------------------------|


(2)
Propose Sequencer
verifier should sent attestation to network to validate this message
Sender Validation: one of the verifiers
Votable: Yes
|-------------------------------------------------------------|
|      Schnorr Signature                                      |
|      Encoded Verifier Public Key                            |
|      OP_CHECKSIG                                            |
|      OP_FALSE                                               |
|      OP_IF                                                  |
|      OP_PUSHBYTES_32  b"Str('via_inscription_protocol')"    |
|      OP_PUSHBYTES_32  b"Str('ProposeSequencerMessage')"     |
|      OP_PUSHBYTES_32  b"proposer_p2wpkh_address"            |
|      OP_ENDIF                                               |
|-------------------------------------------------------------|


(3)
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


(4)
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

(5)
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


(6)
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
|      OP_PUSHBYTES_32  b"receiver_l2_address"                |
|      OP_PUSHBYTES_32  b"l2_contract_address"                |
|      OP_PUSHBYTES_32  b"call_data"                          |
|      OP_ENDIF                                               |
|-------------------------------------------------------------|
 !!! for bridging the l2_contract_address and call_data is empty (0x00) !!!
 !!! and the amount is equal to the amount of btc user sends to bridge address in the same reveal tx !!!
 !!! if the contract address and call_data was provided the amount get used as fee and remaining amount get sent to l2 receiver address !!!
 !!! in future we can implement kinda enforcement withdrawal with using l1->l2 message (reference in notion) !!!
 !!! also we should support op_return only for bridging in future of the inscription indexer !!!

(7)
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
