# Via Project Architecture

This document will help you answer the question: _where can I find the logic for x?_ by giving a directory-tree style
structure of the physical architecture of the Via project.

## High-Level Overview

This repository is the fork of the [zksync-era](https://github.com/matter-labs/zksync-era) repository. In contrast to
ZKsync Era, Via is the Bitcoin rollup that uses Celestia as a data availability layer and dedicated Verifier Network for
settlement. Considerring this it inherits only certain parts of the ZKsync protocol. Our coding convention is not to
modify a source file/folder, but to clone it with the `via_` prefix. Here are the sections relevant to Via Protocol:

<ins>**Smart Contracts:**</ins> All the smart contracts in charge of the protocols on the L2, including L2 and system
contracts.

**<ins>Core App:**</ins> The execution layer. A node running the Via network in charge of the following components:

- Monitoring the Bitcoin network for deposits or priority operations.
- Maintaining a mempool that receives transactions.
- Picking up transactions from the mempool, executing them in a VM, and changing the state accordingly.
- Generating Via chain blocks and batches.
- Preparing circuits for executed batches to be proved.
- Submitting pubdata and proofs to Celestia network.
- Inscibing batch and proof metadata to the Bitcoin network.
- Exposing the Ethereum-compatible web3 API.

**<ins>Prover App:**</ins> The prover app takes batches and metadata generated by the server and constructs a validity
ZK proof for them.

**<ins>Storage Layer:**</ins> The different components and subcomponents don't communicate with each other directly via
APIs, rather via the single source of truth -- the db storage layer.

**<ins>Verifier App:**</ins> The ZK proof verification layer. A node running the Via verification software in charge of
the following components:

- Monitoring Bitcoin network for new proof inscriptions.
- Obtaining batch and proof data from the DA layer.
- Verifying ZK proofs for batches.
- Sending the attestation inscriptions after the verification to the Bitcoin network.
- Processing user withdrawals.

## Low-Level Overview

This section provides a physical map of folders & files in this repository.

- `/contracts`

  - `/system-contracts`: Privileged special-purpose contracts that instantiate some recurring actions on the protocol
    level.

- `/core`

  - `/bin`: Executables for the microservices components comprising Via Core Node.

    - `/via_server`: Main Via Node binary.
    - `/via_block_reverter`: Via Block Reverter CLI implementation.

  - `/lib`: All the library crates used as dependencies of the binary crates above.

    - `/basic_types`: Crate with essential Via primitive types.
    - `/config`: All the configured values used by the different Via apps.
    - `/crypto`: Cryptographical primitives used by the different Via crates.
    - `/dal`: Data access layer
      - `/migrations`: All the db migrations applied to create the storage layer.
      - `/src`: Functionality to interact with the different db tables.
    - `/mempool`: Implementation of the ZKsync transaction pool.
    - `/merkle_tree`: Implementation of a sparse Merkle tree.
    - `/mini_merkle_tree`: In-memory implementation of a sparse Merkle tree.
    - `/multivm`: A wrapper over several versions of VM that have been used by the main node.
    - `/object_store`: Abstraction for storing blobs outside the main data store.
    - `/prometheus_exporter`: Prometheus data exporter.
    - `/queued_job_processor`: An abstraction for async job processing
    - `/state`: A state keeper responsible for handling transaction execution and creating miniblocks and L1 batches.
    - `/storage`: An encapsulated database interface.
    - `/test_account`: A representation of Via account.
    - `/types`: Via network operations, transactions, and common types.
    - `/utils`: Miscellaneous helpers for Via crates.
    - `/vlog`: Via logging utility.
    - `/vm`: ULightweight out-of-circuit VM interface.
    - `/web3_decl`: Declaration of the Web3 API.
    - `via_btc_client`: Module providing an interface to interact with a Bitcoin node.
    - `via_da_clients`: Implementation of the Via DA (Celestia) client.

  - `/node`: Via Node Framework implementation.

- `/prover`: ZKsync Prover orchestrator application.

- `/docker`: Project docker files.

- `/bin` & `/infrastructure`: Infrastructure scripts that help to work with Via applications.

  - `/infrastructure/via`: Via CLI application.

- `/etc`: Configuration files.

  - `/env`:`.env` files that contain environment variables for different configurations of Via Server, Prover or
    Verifier.

- `/keys`: Verification keys for `circuit` module.

- `/via_verifier`: Via Verifier implementation.

  - `bin`: Executables for the components comprising Via Verifier Node.
  - `lib`: All the library crates used as dependencies of the binary crates above.
  - `/node`: Via Verifier Node Framework implementation.
  - `via_btc_sender`: Bitcoin transaction sender implementation for the Verifier Node.
  - `via_btc_watch`: Bitcoin watcher implementation for the Verifier Node.
  - `via_verifier_coordinator`: Main Via Verifier/Coordinator implementation.
  - `via_zk_verifier`: ZK proof verifier implementation for the Verifier Node.

- `via-playground`: Project that demonstrates how to interact with Via network.
