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
  - Database tables and specialized queries serve as the primary mechanism for task coordination. For example,
    components check for "ready" entries using SQL queries and create new entries to signal completion.
- Service Loop:
  - Every service runs an infinite loop with a sleep interval. In each iteration, the service:
    - Checks the database for relevant data or tasks.
    - Processes any available tasks.
    - Performs necessary operations.
    - Sleeps until the next iteration.
  - The loop terminates only upon receiving a stop signal.
  - Most services implement error handling and retries to recover from transient failures.
- Node Framework:
  - The special service `core/node/node_framework` is used to build the final binaries (`core/bin/via_server` and
    `via_verifier/bin/verifier_server`). In the orchestration layer
    ([`via_verifier/bin/verifier_server/src/node_builder.rs`](../../via_verifier/bin/verifier_server/src/node_builder.rs)
    and [`core/bin/via_server/src/node_builder.rs`](../../core/bin/via_server/src/node_builder.rs)), this service
    registers the necessary services, initial configurations, and required libraries. It then uses Tokio to spawn a
    thread for each service.
- Metrics and Monitoring:
  - Components collect and expose metrics for monitoring system health, performance, and state.
  - Critical operations are tracked with metrics such as latency, size, and success/failure counts.

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

The sealing process is the first stage in the lifecycle of an L1 batch. A sequencer is responsible for collecting
transactions, executing them, and forming them into L2 blocks and L1 batches. This process is primarily handled by the
[`via_state_keeper`](../../core/node/via_state_keeper) component.

#### via_state_keeper

[`via_state_keeper`](../../core/node/via_state_keeper) is the main component of the sequencer implementation. It acts as
the central orchestrator for transaction processing and batch creation. As described in its
[README](../../core/node/via_state_keeper/README.md), its primary responsibilities are:

- Extracting transactions from the mempool
- Forming transactions into L2 blocks and L1 batches
- Passing the formed batches for persistence and further processing

The state keeper maintains batch execution state in the `UpdatesManager` until a batch is sealed, at which point these
changes are persisted by the `StateKeeperIO` implementation.

The main loop of the state keeper, implemented in the [`keeper.rs`](../../core/node/via_state_keeper/src/keeper.rs)
file, performs the following steps:

1. Waits for new batch parameters
2. Initializes a new batch environment
3. Processes transactions until a sealing condition is met
4. Seals the batch and passes it to the output handler

**Implementation Details**:

The state keeper implementation is more complex than described above. Its main execution loop:

- Begins by checking for any pending (partially processed) batch from previous runs
- Re-executes pending batches when found, restoring the exact execution state
- Handles protocol upgrade transactions by loading them explicitly at the beginning of a new protocol version
- Maintains metrics for various operations (e.g., `L1_BATCH_METRICS.seal_delta`)
- When a batch is sealed, it immediately sets up for the next L2 block with a new timestamp
- Carefully manages the transition between protocol versions by detecting version changes

After sealing an L2 block within a batch, the state keeper immediately sets up for the next fictive L2 block,
maintaining continuous state. This is visible in code with calls like:

```rust
let new_l2_block_params = self.wait_for_new_l2_block_params(&updates_manager).await?;
Self::start_next_l2_block(new_l2_block_params, &mut updates_manager, &mut batch_executor).await?;
```

#### via_fee_model

[`via_fee_model`](../../core/node/via_fee_model) works in conjunction with the state keeper to calculate fees for
transactions. It provides a fee model implementation that determines:

- Base transaction costs
- Fee adjustments based on current chain conditions
- Batch fee calculations

The fee model is used during transaction selection and execution to ensure economically viable batches.

#### Sealing Conditions

Batch sealing occurs when certain criteria are met. The sealing logic is implemented in
[`seal_criteria`](../../core/node/via_state_keeper/src/seal_criteria) and can be triggered by:

- Reaching a specified number of transactions in a batch
- Reaching a gas limit for the batch
- Approaching a timeout since the last batch was sealed
- Reaching a limit of pubdata bytes
- Filling available slots in the batch

The conditional sealer component, defined in
[`conditional_sealer.rs`](../../core/node/via_state_keeper/src/seal_criteria/conditional_sealer.rs), evaluates these
criteria and determines when a batch should be sealed.

#### Transaction Flow

Transactions follow two primary paths into the state keeper:

1. **Normal L2 Transactions**:

   - Users submit transactions to the mempool
   - The state keeper retrieves these transactions using `MempoolIO`
   - Transactions are executed by the `BatchExecutor`

2. **Priority Transactions**:
   - Detected by [`via_btc_watch`](../../core/node/via_btc_watch) component from Bitcoin transactions
   - Added to the mempool with priority for inclusion in the next batch

#### Commitment Generator

After a batch is sealed, a task is created for the [`commitment_generator`](../../core/node/commitment_generator). This
component is responsible for:

- Generating cryptographic commitments for the L1 batch data
- Processing storage writes from the state keeper
- Creating metadata required for batch verification
- Preparing auxiliary data needed for proofs

The commitment generator, as implemented in [`lib.rs`](../../core/node/commitment_generator/src/lib.rs), processes
sealed batches asynchronously and prepares them for the next stage in the data flow.

**Database Interaction**: The commitment generator doesn't receive direct tasks from state keeper. Instead, it polls the
database for sealed batches that need commitments using queries that look for specific conditions. This indirect
communication pattern is central to the system's architecture, allowing components to operate independently while
maintaining a coherent workflow.

#### Creating Tasks for Downstream Components

Once a batch is sealed and commitments are generated, the state keeper indirectly creates tasks for:

1. [`via_da_dispatcher`](../../core/node/via_da_dispatcher): The data availability dispatcher component that:
   - Prepares batch data for publication to the data availability layer
   - Manages the flow of data to the data availability layer
   - Handles batch metadata for proof generation

#### Difference between via_state_keeper and state_keeper

The [`via_state_keeper`](../../core/node/via_state_keeper) is an adaptation of the original `state_keeper` from zkSync
Era, modified to be compatible with Bitcoin as the data availability layer. Key differences include:

- Integration with Bitcoin-specific components like `via_btc_watch` instead of Ethereum components
- Creation of tasks for `via_da_dispatcher` instead of `eth_sender`

---

### Committed by sequencer/proposer on Bitcoin

After a batch is sealed and its commitments are generated, the next step is to commit this data to the Bitcoin
blockchain for data availability. This process is handled through the coordination of multiple components, primarily
`via_da_dispatcher` and `via_btc_sender`.

#### Data Availability Dispatcher

The [`via_da_dispatcher`](../../core/node/via_da_dispatcher) component is responsible for preparing batch data for
publication to the data availability layer (Celestia). As explained in its
[README](../../core/node/via_da_dispatcher/README.md), it extends the functionality of the original zkSync da_dispatcher
but with specific adaptations for Via Protocol:

- It retrieves sealed L1 batches that are ready for dispatch from the database
- It processes batch commitments and prepares them for publication
- It handles both L1 batch commitments and L1 batch proofs for the Celestia data availability layer

The main execution loop in [`da_dispatcher.rs`](../../core/node/via_da_dispatcher/src/da_dispatcher.rs) performs three
concurrent tasks:

1. `dispatch()` - Prepares and dispatches batch data to the data availability layer
2. `poll_for_inclusion()` - Checks if dispatched data has been successfully included
3. `dispatch_proofs()` - Handles proof dispatching for proven batches

Once the batch data is prepared, the dispatcher creates entries in the database that serve as indirect tasks for the
`via_btc_sender` component.

**Implementation Details**:

The DA dispatcher includes several important implementation features not covered in the high-level description:

- **Robust Retry Mechanism**: The dispatcher implements a retry system for handling transient failures when dispatching
  blobs:

  ```rust
  let dispatch_response = retry(self.config.max_retries(), batch.l1_batch_number, || {
      self.client.dispatch_blob(batch.l1_batch_number.0, batch.pubdata.clone())
  }).await
  ```

- **Detailed Metrics Collection**: The dispatcher tracks extensive metrics about its operations:

  ```
  METRICS.last_dispatched_l1_batch.set(batch.l1_batch_number.0 as usize);
  METRICS.blob_size.observe(batch.pubdata.len());
  ```

- **Dual Proof Handling Paths**: The dispatcher has separate code paths for real and dummy proofs:
  ```rust
  async fn dispatch_proofs(&self) -> anyhow::Result<()> {
      match self.dispatch_real_proof {
          true => self.dispatch_real_proofs().await?,
          false => self.dispatch_dummy_proofs().await?,
      }
      Ok(())
  }
  ```

#### Bitcoin Sender

The [`via_btc_sender`](../../core/node/via_btc_sender) component is responsible for inscribing the L1 batch commitment
onto the Bitcoin blockchain. This component:

- Retrieves pending inscription requests from the database
- Constructs Bitcoin inscription messages containing the batch commitment data
- Creates and signs Bitcoin transactions for the inscriptions
- Broadcasts the transactions to the Bitcoin network
- Updates the database with the transaction information

check [DEV.md](../../core/lib/via_btc_client/DEV.md) for more detail about the inscription data structure.


The inscription process is handled by the
[`btc_inscription_manager.rs`](../../core/node/via_btc_sender/src/btc_inscription_manager.rs), which implements the core
logic for creating and managing inscriptions. It works in a loop that:

1. Checks for pending inscription requests in the database
2. Prepares inscription data from these requests
3. Creates and signs Bitcoin transactions for the inscriptions
4. Broadcasts the transactions to the Bitcoin network
5. Updates the database with the transaction information

**Implementation Details**:

The Bitcoin sender implementation includes several sophisticated components not mentioned in the high-level overview:

- **Inscription Components**: The system actually uses multiple components for inscription handling:

  - `ViaBtcInscriptionAggregator` - Aggregates related batches for inscription
  - `ViaBtcInscriptionManager` - Manages the actual inscription process

- **Fee Estimation and Tracking**: The system carefully calculates and tracks fees:

  ```rust
  let actual_fees = inscribe_info.reveal_tx_output_info._reveal_fee + inscribe_info.commit_tx_output_info.commit_tx_fee;
  ```

- **Blockchain Synchronization**: Before creating inscriptions, the system synchronizes its state with the Bitcoin
  blockchain:
  ```rust
  self.sync_context_with_blockchain().await?;
  ```

#### Inscription Process

The actual inscription of commitment data on Bitcoin involves a two-transaction approach:

1. **Commit Transaction**: Sets up the necessary UTXO structure for the inscription
2. **Reveal Transaction**: Contains the actual inscription data embedded in the transaction

This process, implemented in [`via_btc_client/src/inscriber`](../../core/lib/via_btc_client/src/inscriber), utilizes
Bitcoin's Taproot features to efficiently embed data. 

Once the inscription transactions are broadcast and confirmed on the Bitcoin network, the batch commitment is considered
published, providing a secure, decentralized record of the L1 batch that can be verified by any observer.

---

### Proved by prover

After L1Batch is committed to Bitcoin, the next stage in the data flow is generating cryptographic proofs that verify
the correctness of state transitions in the batch. This process is handled by the prover subsystem.

#### Prover Gateway

The [`prover_gateway`](../../prover/crates/bin/prover_fri_gateway) serves as the interface between the core system and
the prover subsystem. As described in its [README](../../prover/crates/bin/prover_fri_gateway/README.md), it has two
primary responsibilities:

- **Fetching Proof Generation Data**: The gateway polls the core API to retrieve data for batches that need proofs. This
  includes transaction execution data, state diffs, and other necessary inputs.
- **Submitting Completed Proofs**: Once proofs are generated, the gateway submits them back to the core system for
  verification and further processing. (through the data bucket)

#### Prover Subsystem Components

The proof generation is a multi-stage process that involves several specialized components working together:

1. **Witness Generator**: Takes batch information (transaction execution results, state diffs) and constructs a witness
   for proof generation.
2. **Witness Vector Generator**: Uses the witness to compute a witness vector that serves as input for circuit provers.
3. **Circuit Prover**: The core component that generates cryptographic proofs, typically accelerated by GPU hardware.
4. **Proof Compressor**: "Wraps" the generated proof into snark proof.

Each component runs as an independent service and processes jobs from a shared queue, with the House Keeper component
managing job scheduling and monitoring.

#### Storage Bucket

The prover subsystem uses object storage buckets to store and retrieve large data objects during the proving process.
These buckets are divided into several categories:

- **Proofs Bucket**: Stores the final generated proofs for L1 batches, accessed via `Bucket::ProofsFri`
- **Prover Jobs Bucket**: Contains job-specific data needed during the proving process
- **Public Bucket**: Houses data that might be needed by external components

The object storage implementation supports both local file-backed storage and cloud-based storage (primarily Google
Cloud Storage), with the configuration defined in the system's settings. The storage bucket mechanism allows for
efficient handling of large proving data without overwhelming the database.

#### Proof Generation Flow

The overall proof generation flow consists of the following steps:

1. The prover gateway retrieves data for batches that need proofs from the core API
2. The witness generator produces a witness based on the batch data
3. The witness vector generator prepares data for the circuit provers
4. Circuit provers generate the actual cryptographic proofs
5. The proof compressor formats the proof for efficient verification
6. The prover gateway submits the completed proof back to the core (it's uploading final proof to the storage bucket
   then `via_da_dispatcher` will pick it up and send it to the data availability layer)

---

### Proofs Committed on Bitcoin by sequencer/proposer

After a proof is generated by the prover subsystem, it must be committed to the Bitcoin blockchain to make it publicly
available and verifiable.

#### Proof Availability Detection

The [`via_da_dispatcher`](../../core/node/via_da_dispatcher) component plays a central role in this process. It extends
its responsibilities beyond batch commitment data to include proof commitments:

- It regularly polls storage buckets to check for newly available proofs for specific L1 batches
- When a proof becomes available in the `Bucket::ProofsFri` storage bucket, the dispatcher detects it and initiates the
  publication process
- The dispatcher extracts the proof data and prepares it for publication to the data availability layer

This monitoring approach creates a seamless connection between the prover subsystem and the sequencer's commitment
process, despite their indirect communication model.

#### Proof Publication Pipeline

Once a proof is detected, the dispatcher follows the same steps as the batch commitment process.

**Database Query Example**:

The DA dispatcher uses specialized queries to identify proofs ready for dispatch:

```sql
SELECT
    proof_generation_details.l1_batch_number,
    proof_generation_details.proof_blob_url
FROM
    proof_generation_details
WHERE
    proof_generation_details.status = 'generated'
    AND proof_generation_details.proof_blob_url IS NOT NULL
    AND EXISTS (
        SELECT 1 FROM via_data_availability
        WHERE l1_batch_number = proof_generation_details.l1_batch_number
        AND is_proof = FALSE AND blob_id IS NOT NULL
    )
    AND NOT EXISTS (
        SELECT 1 FROM via_data_availability
        WHERE l1_batch_number = proof_generation_details.l1_batch_number
        AND is_proof = TRUE AND blob_id IS NOT NULL
    )
ORDER BY proof_generation_details.l1_batch_number
LIMIT $1
```

This query demonstrates how complex decision-making is encoded in database queries rather than direct component
communication.

#### Bitcoin Inscription of Proofs

The actual inscription of proof data on Bitcoin is handled by the [`via_btc_sender`](../../core/node/via_btc_sender)
component, which:

- Identifies pending proof commitment tasks by querying relevant database tables
- Constructs Bitcoin inscription messages containing the proof DA reference
- Creates and signs Bitcoin transactions to inscribe the proof data
- Broadcasts these transactions to the Bitcoin network
- Updates the database with transaction confirmation information

The proof inscription process follows the same two-transaction approach (commit and reveal) as used for batch
commitments, with specialized formatting for proof data.

#### Proof Commitment Data Structure

The proof commitment inscription contains several critical pieces of information:

- DA reference to the cryptographic proof data verifying the correctness of state transitions
- Metadata linking the proof to its corresponding L1 batch
- References to the original batch commitment for verification (bitcoin tx hash)
- Version information for compatibility with protocol upgrades

#### Verification Readiness

Once the proof commitment is inscribed and confirmed on Bitcoin, the L1 batch enters a state where it can be verified by
the verifier network. This represents a critical transition in the batch lifecycle:

- The batch has been executed (by the sequencer)
- The execution has been proved (by the prover)
- Both the execution data and its proof have been committed to Bitcoin
- The batch is now ready for independent verification

This completes the sequencer/proposer side of the process, with the next stage involving verification by independent
verifiers.

---

### Verified by verifier network

After proofs are committed to Bitcoin, the next step in the data flow is verification by the independent verifier
network. This process ensures that the state transitions contained in the L1 batch are valid before the batch can be
finalized.

#### Bitcoin Inscription Indexing

The verification process begins with the [`via_btc_watch`](../../via_verifier/node/via_btc_watch) component of the
verifier network. This component:

- Continuously monitors the Bitcoin blockchain for new inscriptions
- Utilizes the `BitcoinInscriptionIndexer` to track and process relevant inscriptions
- Filters for inscriptions that match the protocol's requirements for proof commitments
- Maintains state about the last processed Bitcoin block to ensure no inscriptions are missed

The `via_btc_watch` component implements a polling loop that regularly checks for new Bitcoin blocks and processes any
relevant inscriptions found within them.

**Implementation Details**:

The `via_btc_watch` component contains specialized message processors for different types of inscriptions:

- `VerifierMessageProcessor` - Handles proof verifications and votes
- `L1ToL2MessageProcessor` - Processes messages from L1 to L2

The message processing system is designed to handle various inscription types:

```rust
match msg {
    ref f @ FullInscriptionMessage::ProofDAReference(ref proof_msg) => {
        // Handle proof reference inscriptions
    },
    ref f @ FullInscriptionMessage::ValidatorAttestation(ref attestation_msg) => {
        // Handle validator attestations
    },
    // Handle other message types
}
```

#### Notification by Proof Commitment Inscription

When a new proof commitment inscription is detected on the Bitcoin blockchain, the verifier network is implicitly
notified through its monitoring infrastructure:

- The `VerifierMessageProcessor` component processes incoming inscriptions from Bitcoin
- It specifically identifies `ProofDAReference` inscriptions, which contain references to proof data
- These inscriptions serve as notifications that a new batch and its proof are available for verification
- The processor extracts essential metadata from the inscription, including batch numbers and data references

#### Parsing Inscription Data

Once a proof commitment inscription is detected, the verifier network parses the inscription data to extract critical
information:

- The batch number associated with the proof
- References to the original L1 batch commitment transaction
- Data availability identifiers indicating where to fetch the complete proof data
- Blob IDs and other metadata needed to locate and verify the proof

The parsing process, primarily handled in the `message_processors/verifier.rs` module, converts the raw inscription data
into structured information that can be used to access and verify the proof.

#### Fetching Batch and Proof Data

With the parsed information, the verifier proceeds to fetch the necessary data for verification:

- The verifier fetches the L1 batch commitment transaction based on the references in the proof inscription
- It retrieves the complete L1 batch data from the data availability layer
- It fetches the proof data using the blob identifiers provided in the inscription
- All this data is gathered via the data availability layer (Celestia or other supported DA solutions)

This fetching process ensures that the verifier has all the information needed to perform a complete verification of the
batch.

#### Validating L1 Batch Information

Before proceeding with cryptographic verification, the verifier validates general information about the L1 batch:

- Checks that the batch number is in sequence with previously verified batches
- Validates priority operations are included correctly
- Ensures the batch metadata matches what was committed in the original batch commitment
- Verifies that the batch hasn't already been finalized (to prevent duplicate verifications)

These checks help ensure the integrity and consistency of the batch before investing resources in proof verification.

#### Verifying the Proof

The core of the verification process is the cryptographic verification of the ZK-SNARK proof. This is handled by the
[`via_verification`](../../via_verifier/lib/via_verification) library:

- The library loads the appropriate verification key based on the batch number
- It processes the proof using specialized verification algorithms
- The verification mathematically confirms that the state transitions described in the batch were computed correctly
- The verification returns a boolean result indicating whether the proof is valid

This verification step is the most computationally intensive part of the process and provides cryptographic certainty
about the correctness of the batch execution.

#### Sending Attestation Commitment to Bitcoin

After verification is complete, the verifier node records its vote and sends an attestation to Bitcoin:

- The [`via_btc_sender`](../../via_verifier/node/via_btc_sender) component prepares a `ValidatorAttestation` inscription
- The attestation includes a reference to the original proof transaction and a vote (Ok or NotOk)
- The attestation is inscribed on Bitcoin using the same inscription mechanism as other protocol data
- The inscription serves as a permanent record of the verifier's decision on the batch

Each verifier in the network independently performs verification and submits attestations, creating a decentralized
consensus on the validity of batches.

**Voting Details**:

The attestation vote processing has specific logic for determining consensus:

```rust
// Vote = true if attestation_msg.input.attestation == Vote::Ok
let is_ok = matches!(
    attestation_msg.input.attestation,
    via_btc_client::types::Vote::Ok
);
```

The verifier also records the attestation and checks if enough votes have been received:

```rust
if votes_dal
    .finalize_transaction_if_needed(
        votable_transaction_id,
        self.zk_agreement_threshold,
        indexer.get_number_of_verifiers(),
    )
    .await
    .map_err(|e| MessageProcessorError::DatabaseError(e.to_string()))?
{
    tracing::info!(
        "Finalizing transaction with tx_id: {:?} and block number: {:?}",
        tx_id,
        l1_batch_number
    );
}
```

#### Finalizing the Batch

Once a sufficient number of attestations are received from the verifier network, the batch can be considered finalized:

- The system tracks attestation votes from all verifiers
- When the number of positive attestations crosses a predetermined threshold (controlled by the `zk_agreement_threshold`
  parameter)
- The batch is marked as finalized in the system
- This finalization status is recorded and becomes part of the protocol's state

The finalization process represents the final confirmation that a batch has been properly executed, proved, and
verified, allowing the protocol to build upon it for future operations.

**Threshold Calculation**:

The `zk_agreement_threshold` parameter is crucial to the finalization process. The system calculates the required number
of votes based on this threshold:

```rust
// In database code
let required_votes = (total_verifiers as f64 * threshold).ceil() as i64;
```

This ensures that a sufficient percentage of verifiers have confirmed the batch before it's finalized.

---

### Executed by verifier network

After a batch is verified and finalized, the final stage in the data flow is the execution of operations that bridge
assets from L2 to L1 (withdrawals). This process is handled by the verifier network in a coordinated, secure manner.

#### L2 Withdrawal Initiation

Withdrawals begin in the L2 environment through interactions with the
[`L2BaseToken`](../../contracts/system-contracts/contracts/L2BaseToken.sol) contract. This system contract:

- Provides methods for users to initiate withdrawals (`withdraw`)
- Burns the L2 tokens from the user's account
- Creates a withdrawal message that is sent to the L1 Messenger contract
- Records the withdrawal event with critical information for later processing

When a user calls the withdrawal function, the withdrawal request is included in an L1 batch and goes through the entire
protocol pipeline: sealing, commitment, proving, verification, and finalization.

#### Via Fee Model for Withdrawals

The [`via_fee_model`](../../core/node/via_fee_model) component is involved in the withdrawal process by:

- Calculating appropriate fees for withdrawal transactions
- Ensuring economic viability of the withdrawal operation
- Providing fee parameters that are used in the construction of the Bitcoin transaction

These fee calculations are particularly important for the verifier network as they need to create economically viable
Bitcoin transactions to process withdrawals.

#### Withdrawal Processing After L1 Batch Finalization

Once a batch containing withdrawals is finalized (through the verification process described in the previous section),
the verifier network begins processing these withdrawals:

1. **Detection**: The [`via_verifier_coordinator`](../../via_verifier/node/via_verifier_coordinator) component detects
   finalized batches with unprocessed withdrawals
2. **Database Updates**: The system updates transaction status in the database to reflect that withdrawal processing has
   begun
3. **Priority Order**: Withdrawals are processed in order based on batch numbers, ensuring proper sequencing

This processing begins automatically after a batch is finalized, creating a seamless flow from verification to
execution.

**Detection Query**:

The system uses database queries to identify finalized batches with unprocessed withdrawals:

```rust
// From the implementation
let l1_batches = self
    .master_connection_pool
    .connection_tagged("withdrawal session")
    .await?
    .via_votes_dal()
    .list_finalized_blocks_and_non_processed_withdrawals()
    .await?;
```

#### Withdrawal Service

The withdrawal service component is responsible for orchestrating the entire withdrawal process:

- It manages the state of pending withdrawals
- It coordinates the signing process across the verifier network
- It monitors the status of withdrawal transactions
- It ensures that withdrawals are processed in the correct order

The service implements an asynchronous processing model that allows it to handle multiple withdrawals efficiently while
maintaining the protocol's security properties.

**Session Management Pattern**:

The withdrawal system uses a session-based approach with several lifecycle stages:

```rust
// Interface methods
async fn session(&self) -> anyhow::Result<Option<SessionOperation>>;
async fn is_session_in_progress(&self, session_op: &SessionOperation) -> anyhow::Result<bool>;
async fn verify_message(&self, session_op: &SessionOperation) -> anyhow::Result<bool>;
async fn before_process_session(&self, _: &SessionOperation) -> anyhow::Result<bool>;
async fn before_broadcast_final_transaction(&self, session_op: &SessionOperation) -> anyhow::Result<bool>;
async fn after_broadcast_final_transaction(&self, txid: Txid, session_op: &SessionOperation) -> anyhow::Result<bool>;
```

This structured approach ensures that sessions proceed through proper verification and coordination steps.

#### Extracting L2->L1 Requests from L1 Batch Information

A critical step in the withdrawal process is extracting the actual withdrawal requests:

1. The verifier network retrieves the L1 batch information from the data availability layer
2. It parses the batch data to extract L2->L1 messages (withdrawal requests)
3. It validates these requests against the finalized batch state
4. It groups withdrawal requests by recipient to optimize transaction efficiency

This extraction process, implemented in the [`withdrawal_client`](../../via_verifier/lib/via_withdrawal_client), ensures
that all legitimate withdrawal requests are accurately identified and processed.

**Withdrawal Grouping Optimization**:

The system optimizes Bitcoin transactions by grouping withdrawals by recipient:

```rust
// Group withdrawals by address and sum amounts
let mut grouped_withdrawals: HashMap<Address, Amount> = HashMap::new();
for w in withdrawals {
    *grouped_withdrawals.entry(w.address).or_insert(Amount::ZERO) = grouped_withdrawals
        .get(&w.address)
        .unwrap_or(&Amount::ZERO)
        .checked_add(w.amount)
        .ok_or_else(|| anyhow::anyhow!("Withdrawal amount overflow when grouping"))?;
}
```

This significantly reduces transaction costs when multiple withdrawals are going to the same address.

#### Coordinator-Driven Process

In the Via Protocol's verifier network, one node acts as the coordinator, responsible for orchestrating the withdrawal
verification and signing process:

1. The coordinator initiates a withdrawal session for a specific finalized batch
2. It constructs an unsigned Bitcoin transaction that represents all withdrawals in the batch
3. other verifiers will get this transaction from coordinator
4. Each verifier independently verifies the transaction, checking:
   - That the withdrawals match those in the finalized batch
   - That the transaction structure and outputs are correct
   - That the fee calculations are appropriate

This independent verification process ensures that all verifiers agree on the withdrawal transaction before proceeding
to sign it.

#### MuSig2 Coordination

The Via Protocol uses MuSig2 (Multi-Signature Schnorr) for coordinating signatures across the verifier network:

1. **Round 1 - Nonce Generation**: Each verifier generates a nonce pair and shares the public nonce with the coordinator
2. **Aggregation**: The coordinator aggregates all public nonces
3. **Round 2 - Partial Signatures**: Each verifier creates a partial signature using their private key
4. **Final Aggregation**: The coordinator combines all partial signatures into a single aggregate signature

This MuSig2 implementation, found in [`via_musig2`](../../via_verifier/lib/via_musig2), allows the verifier network to
create a single signature from multiple parties without revealing individual private keys, providing both security and
efficiency.

**Implementation Details**:

The MuSig2 implementation includes sophisticated state tracking for the signing process:

```rust
pub struct Signer {
    secret_key: SecretKey,
    public_key: PublicKey,
    signer_index: usize,
    key_agg_ctx: KeyAggContext,
    first_round: Option<FirstRound>,
    second_round: Option<SecondRound<Vec<u8>>>,
    message: Vec<u8>,
    nonce_submitted: bool,
    partial_sig_submitted: bool,
}
```

The signing process includes distinct methods for each stage:

- `start_signing_session` - Creates the public nonce for the first round
- `receive_nonce` - Collects nonces from other verifiers
- `create_partial_signature` - Creates a signature contribution
- `receive_partial_signature` - Collects signatures from others
- `create_final_signature` - Combines all signatures into one

#### Withdrawal Transaction Builder

The withdrawal transaction builder is responsible for constructing the Bitcoin transaction that will execute the
withdrawals:

- It collects UTXOs from the bridge address to fund the transaction
- It creates outputs for each withdrawal recipient
- It adds an OP_RETURN output with metadata linking to the L1 batch
- It calculates appropriate fees based on current Bitcoin network conditions
- It structures the transaction to be compatible with Bitcoin's Taproot features

The [`transaction_builder`](../../via_verifier/lib/via_musig2/src/transaction_builder.rs) component handles these tasks,
creating a well-formed Bitcoin transaction that represents the withdrawals in the batch.

**UTXO Management**:

The system includes a specialized UTXO manager that handles Bitcoin transaction creation:

```rust
pub struct UtxoManager {
    /// Btc client
    btc_client: Arc<dyn BitcoinOps>,
    /// The wallet address
    address: Address,
    /// The transactions executed by the wallet
    context: Arc<RwLock<VecDeque<Transaction>>>,
    /// The minimum amount to merge utxos
    minimum_amount: Amount,
    /// The maximum number of utxos to merge in a single tx
    merge_limit: usize,
}
```

This component synchronizes with the blockchain before transactions are built:

``` rust
self.utxo_manager.sync_context_with_blockchain().await?;
```

It also handles UTXO selection, consolidation, and preventing double-spending of UTXOs in pending transactions.

#### Broadcast and Execution

The final step in the withdrawal process is broadcasting the signed transaction to the Bitcoin network:

1. The coordinator receives the aggregate signature
2. It applies this signature to the unsigned transaction
3. It broadcasts the signed transaction to the Bitcoin network
4. It monitors the transaction for confirmations
5. Once confirmed, it updates the database to mark the withdrawals as processed

The transaction, once confirmed on the Bitcoin network, completes the withdrawal process, transferring the requested
funds from the bridge address to the recipients specified in the withdrawal requests.

**Database Update**:

After successful broadcast, the system updates the database to mark the withdrawal as processed:

```rust
self.master_connection_pool
    .connection_tagged("verifier task")
    .await?
    .via_votes_dal()
    .mark_vote_transaction_as_processed(
        H256::from_slice(&txid.as_raw_hash().to_byte_array()),
        &session_op.get_proof_tx_id(),
        session_op.get_l1_batche_number(),
    )
    .await?;
```

---

This flow is repeating for each L1Batch.
