# Data Flow in the Via Protocol

This document outlines the data flow between Via Protocol components, detailing their interactions and communication
channels.

## Prerequisites

Before discussing the data flow, note the following codebase conventions and design principles:

- Services:
  - Components located under `core/node` and `via_verifier/node` are services.
- Libraries:
  - Components under `core/lib` and `via_verifier/lib` are libraries used by services for various functionalities.
- Indirect Communication:
  - Services do not communicate directly. Instead, each service fetches required data from the database using
    `core/lib/dal` or `via_verifier/lib/via_dal`, processes the data, and writes the output back to the database as
    necessary. This pattern creates an indirect communication channel between different services.
- Service Loop:
  - Every service runs an infinite loop with a sleep interval. In each iteration, the service:
    - Checks the database for relevant data or tasks.
    - Processes any available tasks.
    - Performs necessary operations.
    - Sleeps until the next iteration.
  - The loop terminates only upon receiving a stop signal.
- Node Framework:
  - The special service `core/node/node_framework` is used to build the final binaries (`core/bin/via_server` and
    `via_verifier/bin/verifier_server`). In the orchestration layer
    ([`via_verifier/bin/verifier_server/src/node_builder.rs`](../../via_verifier/bin/verifier_server/src/node_builder.rs)
    and [`core/bin/via_server/src/node_builder.rs`](../../core/bin/via_server/src/node_builder.rs)), this service
    registers the necessary services, initial configurations, and required libraries. It then uses Tokio to spawn a
    thread for each service.

---

## Data Flow

in this section, we will discuss about the data flow in the Via Protocol from beginning of the transaction to the
finalization L1Batch.

**L1Batch State**:

- Seal by sequencer
- Committed by sequencer/proposer on Bitcoin
- Proved by prover
- Proofs Committed on Bitcoin by sequencer/proposer
- Verified by verifier network
- Finalized
- Executed by verifier network

---

### Seal by sequencer

via_state_keeper

via_fee_model

waiting for sealing condition - enough tx - timeout - link to doc for reading more about sealing condition

communication with mempool - normal L2 transaction from mempool - via_btc_watch => Priority transaction => mempool

creating task for via_da_dispatcher

commitment generator

> difference of via_state_keeper Vs state_keeper creating task for eth_sender creating task for via_da_dispatcher

---

### Committed by sequencer/proposer on Bitcoin

da_dispatcher result

creating task for via_btc_sender

inscribing commitment data on Bitcoin with via_btc_sender

---

### Proved by prover

prover_gateway

prover

storage bucket

---

### Proofs Committed on Bitcoin by sequencer/proposer

via_da_dispatcher => checking storage bucket availability for specific L1Batch => if available, publish it to da layer
and create task for via_btc_sender (indirect communication through db)

> i just use the term of task for simplicity, in reality the services just write the data in related tables and then we
> have different query to figure out the task (step) for each service

via_btc_sender => inscribe proof commitment data on Bitcoin

---

### Verified by verifier network

via_btc_watch inscription indexing

notify by proof commitment inscription

parse inscription data

fetch L1 Batch commitment tx based on parsed information

fetch L1Batch from da layer fetch proof from da layer

validating L1Batch general info (piorirty id and etc)

verifying proof

via_btc_sender send attestation commitment to Bitcoin

---

### Finalized

this attestion is type of voting mechanism for L1Batch

---

### Executed by verifier network

BaseToken.sol

via_fee_model ...

withdrawal proccssing after L1Batch finalized

musig2 coordination

withdrawal builder

broadcast execution
