# Data Flow in the Via Protocol

This document outlines the data flow between Via Protocol components, detailing their interactions and communication channels.

## Prerequisites

Before discussing the data flow, note the following codebase conventions and design principles:

- Services:
  - Components located under `core/node` and `via_verifier/node` are services.
- Libraries:
  - Components under `core/lib` and `via_verifier/lib` are libraries used by services for various functionalities.
- Indirect Communication:
  - Services do not communicate directly. Instead, each service fetches required data from the database using `core/lib/dal` or `via_verifier/lib/via_dal`, processes the data, and writes the output back to the database as necessary. This pattern creates an indirect communication channel between different services.
- Service Loop:
  - Every service runs an infinite loop with a sleep interval. In each iteration, the service:
    - Checks the database for relevant data or tasks.
    - Processes any available tasks.
    - Performs necessary operations.
    - Sleeps until the next iteration.
  - The loop terminates only upon receiving a stop signal.
- Node Framework:
  - The special service `core/node/node_framework` is used to build the final binaries (`core/bin/via_server` and `via_verifier/bin/verifier_server`). In the orchestration layer ([`via_verifier/bin/verifier_server/src/node_builder.rs`](../../via_verifier/bin/verifier_server/src/node_builder.rs) and [`core/bin/via_server/src/node_builder.rs`](../../core/bin/via_server/src/node_builder.rs)), this service registers the necessary services, initial configurations, and required libraries. It then uses Tokio to spawn a thread for each service.

## Entry Point

- L2 Transaction
- L1 -> L2 Transaction (via_btc_watch)

mempool

> db as communication tools between services

L1 Batch Commitment:

- via_state_keeper
- via_da_dispatcher
- via_btc_sender

L1 Batch Proving:

- prover gateway
- prover
- data bucket
- via_da_dispatcher
- via_btc_sender

L1 Batch Verification (finalization):

- via_btc_watch
- via_da_dispatcher
- via_zk_verification
- via_btc_sender

L1 Batch Execution (L2 -> L1 Transaction):
- via_btc_watch
- coordinator <=> verifier
- withdrawal_builder
- musig2 coordination
- broadcast execution
