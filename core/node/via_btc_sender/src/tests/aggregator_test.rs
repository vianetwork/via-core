#[cfg(test)]
mod tests {
    use bitcoin::{hashes::Hash, Txid};
    use via_btc_client::{traits::Serializable, types::InscriptionMessage};
    use zksync_contracts::BaseSystemContractsHashes;
    use zksync_dal::{ConnectionPool, Core, CoreDal};
    use zksync_node_test_utils::{create_l1_batch, l1_batch_metadata_to_commitment_artifacts};
    use zksync_types::{
        btc_block::ViaBtcL1BlockDetails, btc_inscription_operations::ViaBtcInscriptionRequestType,
        ProtocolVersionId,
    };

    use crate::tests::utils::{
        default_l1_batch_metadata, generate_random_bytes, get_btc_sender_config, ViaAggregatorTest,
    };

    // Get the current operation (commitBatch or commitProof) to execute when there is no batches. Should return 'None'
    #[tokio::test]
    async fn test_get_next_ready_operation_when_no_l1_batch() {
        let pool = ConnectionPool::<Core>::test_pool().await;
        let header = create_l1_batch(1);
        let mut aggregator_test = ViaAggregatorTest::new(
            header.protocol_version.unwrap(),
            header.base_system_contracts_hashes,
            pool,
            None,
        )
        .await;
        let op = aggregator_test.get_next_ready_operation().await;
        assert!(op.is_none());
    }

    // Get the current operation (commitBatch or commitProof) to execute, when there is one batch ready to be 'commitBatch'.
    #[tokio::test]
    async fn test_get_next_ready_operation_when_one_commit_l1_batch() {
        let header = create_l1_batch(1);
        let mut aggregator_test = ViaAggregatorTest::new(
            header.protocol_version.unwrap(),
            header.base_system_contracts_hashes,
            ConnectionPool::<Core>::test_pool().await,
            None,
        )
        .await;

        aggregator_test
            .insert_l1_batch(
                header.clone(),
                l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
            )
            .await;

        let op = aggregator_test.get_next_ready_operation().await.unwrap();

        assert_eq!(op.get_l1_batches_detail().len(), 1);
        assert_eq!(
            op.get_l1_batches_detail().first().unwrap().number,
            header.number
        );
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain
        );
    }

    // Get the current operation (commitBatch or commitProof) to execute, when there are many batch ready to be 'commitBatch'.
    #[tokio::test]
    async fn test_get_next_ready_operation_when_many_batches() {
        let max_aggregated_blocks_to_commit: usize = 5;
        let config = get_btc_sender_config(max_aggregated_blocks_to_commit as i32, 5);

        let mut protocol_version: Option<ProtocolVersionId> = None;
        let mut base_system_contracts_hashes: Option<BaseSystemContractsHashes> = None;
        let mut l1_batches = Vec::with_capacity(max_aggregated_blocks_to_commit);

        for index in 1..max_aggregated_blocks_to_commit + 1 {
            let header = create_l1_batch(index as u32);
            l1_batches.push(header.clone());

            if base_system_contracts_hashes.is_none() {
                base_system_contracts_hashes = Some(header.base_system_contracts_hashes);
            }

            if protocol_version.is_none() {
                protocol_version = header.protocol_version;
            }
        }

        let mut aggregator_test = ViaAggregatorTest::new(
            protocol_version.unwrap(),
            base_system_contracts_hashes.unwrap(),
            ConnectionPool::<Core>::test_pool().await,
            Some(config),
        )
        .await;

        // Insert l1_batches
        for header in l1_batches {
            aggregator_test
                .insert_l1_batch(
                    header.clone(),
                    l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
                )
                .await;
        }

        // Get next operation to process
        let op = aggregator_test
            .get_next_ready_operation()
            .await
            .expect("Expected to receive one batch ready to commit");

        // Check if the operation is type ViaBtcInscriptionRequestType::CommitL1BatchOnchain
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitL1BatchOnchain
        );

        // Check if the number of blocks match the selected ones.
        assert_eq!(
            op.get_l1_batches_detail().len(),
            max_aggregated_blocks_to_commit
        );
    }

    // When the selected batches are not sequantial.
    #[tokio::test]
    #[should_panic(expected = "L1 batches prepared for commit are not sequential")]
    async fn test_get_next_ready_operation_when_many_batches_not_sequential() {
        // The number of batches we want to create for testing.
        let max_aggregated_blocks_to_commit: usize = 5;
        let config = get_btc_sender_config(max_aggregated_blocks_to_commit as i32, 5);
        let mut protocol_version: Option<ProtocolVersionId> = None;
        let mut base_system_contracts_hashes: Option<BaseSystemContractsHashes> = None;
        let mut l1_batches = Vec::with_capacity(max_aggregated_blocks_to_commit);

        for index in 1..max_aggregated_blocks_to_commit + 1 {
            // We ignore the batch with id 3 to simulate no sequantail list of batch.
            if index == 3 {
                continue;
            }
            let header = create_l1_batch(index as u32);
            l1_batches.push(header.clone());

            if base_system_contracts_hashes.is_none() {
                base_system_contracts_hashes = Some(header.base_system_contracts_hashes);
            }

            if protocol_version.is_none() {
                protocol_version = header.protocol_version;
            }
        }

        let mut aggregator_test = ViaAggregatorTest::new(
            protocol_version.unwrap(),
            base_system_contracts_hashes.unwrap(),
            ConnectionPool::<Core>::test_pool().await,
            Some(config),
        )
        .await;

        for header in l1_batches {
            aggregator_test
                .insert_l1_batch(
                    header.clone(),
                    l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
                )
                .await;
        }

        aggregator_test.get_next_ready_operation().await.unwrap();
    }

    // When there is one batch ready to commit its proof.
    #[tokio::test]
    async fn test_get_next_ready_operation_when_one_commit_proof_batch() {
        let header = create_l1_batch(1);
        let mut aggregator_test = ViaAggregatorTest::new(
            header.protocol_version.unwrap(),
            header.base_system_contracts_hashes,
            ConnectionPool::<Core>::test_pool().await,
            None,
        )
        .await;

        // Insert the batch
        aggregator_test
            .insert_l1_batch(
                header.clone(),
                l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
            )
            .await;

        let (inscription_request_id, inscription_request_history_id) = aggregator_test
            .update_l1_block_for_ready_to_commit_proof(header.number)
            .await;

        aggregator_test
            .storage
            .btc_sender_dal()
            .confirm_inscription(inscription_request_id, inscription_request_history_id)
            .await
            .unwrap();

        // Get next operation to process
        let op = aggregator_test.get_next_ready_operation().await.unwrap();

        // Check if the batch was selected and it's number match.
        assert_eq!(op.get_l1_batches_detail().len(), 1);
        assert_eq!(
            op.get_l1_batches_detail().first().unwrap().number,
            header.number
        );
        // Check if the inscription type match ViaBtcInscriptionRequestType::CommitProofOnchain
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitProofOnchain
        );
    }

    // When some blocks are ready to be commit the pub data and others ready to commit proof.
    // Expected: Return a list of baches ready to be proof.
    //
    #[tokio::test]
    async fn test_get_next_ready_operation_when_many_l1_batch_ready_to_proof() {
        let max_aggregated_blocks_to_commit: usize = 5;
        let expected_batches: usize = 3;
        let config = get_btc_sender_config(max_aggregated_blocks_to_commit as i32, 5);
        let mut protocol_version: Option<ProtocolVersionId> = None;
        let mut base_system_contracts_hashes: Option<BaseSystemContractsHashes> = None;
        let mut l1_batches = Vec::with_capacity(max_aggregated_blocks_to_commit);

        for index in 1..max_aggregated_blocks_to_commit + 1 {
            let header = create_l1_batch(index as u32);
            l1_batches.push(header.clone());

            if base_system_contracts_hashes.is_none() {
                base_system_contracts_hashes = Some(header.base_system_contracts_hashes);
            }

            if protocol_version.is_none() {
                protocol_version = header.protocol_version;
            }
        }

        let mut aggregator_test = ViaAggregatorTest::new(
            protocol_version.unwrap(),
            base_system_contracts_hashes.unwrap(),
            ConnectionPool::<Core>::test_pool().await,
            Some(config),
        )
        .await;

        for (index, header) in l1_batches.iter().enumerate() {
            aggregator_test
                .insert_l1_batch(
                    header.clone(),
                    l1_batch_metadata_to_commitment_artifacts(&default_l1_batch_metadata()),
                )
                .await;

            // Switch the status of the batches <= expected_batches to ready to proof. At the end we will have 3 batches ready to commit proof
            // and 2 ready to commit pubdata.
            if index + 1 > expected_batches {
                continue;
            }

            let (inscription_request_id, inscription_request_history_id) = aggregator_test
                .update_l1_block_for_ready_to_commit_proof(header.number)
                .await;

            aggregator_test
                .storage
                .btc_sender_dal()
                .confirm_inscription(inscription_request_id, inscription_request_history_id)
                .await
                .unwrap();
        }

        let op = aggregator_test.get_next_ready_operation().await.unwrap();

        // Check if the inscription request is ViaBtcInscriptionRequestType::CommitProofOnchain
        assert_eq!(
            op.get_inscription_request_type(),
            ViaBtcInscriptionRequestType::CommitProofOnchain
        );
        // Check if the number of batches returned match the expected len.
        assert_eq!(op.get_l1_batches_detail().len(), expected_batches);
        let ready_proof_batches = aggregator_test
            .storage
            .via_blocks_dal()
            .get_ready_for_commit_proof_l1_batches(5)
            .await
            .unwrap();
        assert_eq!(ready_proof_batches.len(), expected_batches);

        // Check if the ready commit match the expect len.
        let ready_commit_batches = aggregator_test
            .storage
            .via_blocks_dal()
            .get_ready_for_commit_l1_batches(
                5,
                aggregator_test
                    .protocol_version
                    .base_system_contracts_hashes
                    .bootloader,
                aggregator_test
                    .protocol_version
                    .base_system_contracts_hashes
                    .default_aa,
                protocol_version.unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            ready_commit_batches.len(),
            max_aggregated_blocks_to_commit - expected_batches
        );
    }

    #[tokio::test]
    async fn test_construct_inscription_message() {
        let config = get_btc_sender_config(1, 1);
        let header = create_l1_batch(1);
        let base_system_contracts_hashes = header.base_system_contracts_hashes;
        let protocol_version = header.protocol_version;

        let aggregator_test = ViaAggregatorTest::new(
            protocol_version.unwrap(),
            base_system_contracts_hashes,
            ConnectionPool::<Core>::test_pool().await,
            Some(config),
        )
        .await;

        let batch = ViaBtcL1BlockDetails {
            number: header.number,
            hash: Some(generate_random_bytes(32)),
            blob_id: "".to_string(),
            commit_tx_id: Txid::from_byte_array(generate_random_bytes(32).try_into().unwrap()),
            reveal_tx_id: Txid::from_byte_array(generate_random_bytes(32).try_into().unwrap()),
            timestamp: header.timestamp as i64,
        };

        let message: via_btc_client::types::InscriptionMessage = aggregator_test
            .aggregator
            .construct_inscription_message(
                &ViaBtcInscriptionRequestType::CommitL1BatchOnchain,
                &batch,
            )
            .unwrap();
        let message_bytes = InscriptionMessage::to_bytes(&message);
        assert_eq!(InscriptionMessage::from_bytes(&message_bytes), message);
    }
}
