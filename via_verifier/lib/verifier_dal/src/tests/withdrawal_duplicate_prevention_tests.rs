use std::str::FromStr;

use anyhow::Result;
use bitcoin::{Address as BitcoinAddress, Amount};
use via_verifier_types::withdrawal::WithdrawalRequest;
use zksync_types::Address;

use crate::{Connection, ConnectionPool, Verifier, VerifierDal};

#[cfg(test)]
mod withdrawal_duplicate_prevention_tests {
    use super::*;

    async fn create_test_connection() -> Connection<'static, Verifier> {
        let connection_pool = ConnectionPool::<Verifier>::verifier_test_pool().await;
        connection_pool.connection().await.unwrap()
    }

    fn create_withdrawal_request(
        id: &str,
        receiver: &str,
        amount_sats: u64,
        l2_tx_hash: &str,
        l2_tx_log_index: u16,
    ) -> WithdrawalRequest {
        WithdrawalRequest {
            id: id.to_string(),
            receiver: BitcoinAddress::from_str(receiver).unwrap().assume_checked(),
            amount: Amount::from_sat(amount_sats),
            l2_sender: Address::zero(),
            l2_tx_hash: l2_tx_hash.to_string(),
            l2_tx_log_index,
        }
    }

    #[tokio::test]
    async fn test_bridge_withdrawal_exists_dal_method() -> Result<()> {
        let mut storage = create_test_connection().await;

        let tx_id_1 = vec![0x01, 0x02, 0x03, 0x04];
        let tx_id_2 = vec![0x05, 0x06, 0x07, 0x08];

        let exists = storage
            .via_withdrawal_dal()
            .bridge_withdrawal_exists(&tx_id_1)
            .await?;
        assert!(!exists);

        storage
            .via_withdrawal_dal()
            .insert_bridge_withdrawal_tx(&tx_id_1)
            .await?;

        let exists = storage
            .via_withdrawal_dal()
            .bridge_withdrawal_exists(&tx_id_1)
            .await?;
        assert!(exists);

        let exists = storage
            .via_withdrawal_dal()
            .bridge_withdrawal_exists(&tx_id_2)
            .await?;
        assert!(!exists);

        Ok(())
    }

    #[tokio::test]
    async fn test_check_withdrawal_exists_unprocessed_dal_method() -> Result<()> {
        let mut storage = create_test_connection().await;

        let withdrawal = create_withdrawal_request(
            "test_duplicate_check",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0x1234567890abcdef1234567890abcdef12345678",
            0,
        );

        let exists = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal)
            .await?;
        assert!(!exists);

        storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;

        let exists = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal)
            .await?;
        assert!(exists);

        let bridge_id = storage
            .via_withdrawal_dal()
            .insert_bridge_withdrawal_tx(&vec![0x01, 0x02])
            .await?;

        storage
            .via_withdrawal_dal()
            .mark_withdrawal_as_processed(bridge_id, &withdrawal)
            .await?;

        let exists = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal)
            .await?;
        assert!(!exists);

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_withdrawals_conflict_handling() -> Result<()> {
        let mut storage = create_test_connection().await;

        let withdrawal = create_withdrawal_request(
            "test_conflict_handling",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0x1111111111111111111111111111111111111111",
            0,
        );

        let results = storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;
        assert_eq!(results.len(), 1);
        assert!(results[0]);

        let results = storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;
        assert_eq!(results.len(), 1);
        assert!(results[0]);

        let withdrawal2 = create_withdrawal_request(
            "test_conflict_handling_2",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            30000,
            "0x2222222222222222222222222222222222222222",
            1,
        );

        let results = storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone(), withdrawal2.clone()])
            .await?;
        assert_eq!(results.len(), 2);
        assert!(results[0]);
        assert!(results[1]);
        Ok(())
    }

    #[tokio::test]
    async fn test_list_no_processed_withdrawals() -> Result<()> {
        let mut storage = create_test_connection().await;

        let withdrawal1 = create_withdrawal_request(
            "test_list_1",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0x3333333333333333333333333333333333333333",
            0,
        );

        let withdrawal2 = create_withdrawal_request(
            "test_list_2",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            30000,
            "0x4444444444444444444444444444444444444444",
            1,
        );

        let withdrawal3 = create_withdrawal_request(
            "test_list_3",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            1000,
            "0x5555555555555555555555555555555555555555",
            2,
        );

        storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[
                withdrawal1.clone(),
                withdrawal2.clone(),
                withdrawal3.clone(),
            ])
            .await?;

        let unprocessed = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;
        assert_eq!(unprocessed.len(), 3);

        let unprocessed = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(20000, 10)
            .await?;
        assert_eq!(unprocessed.len(), 2);

        let unprocessed = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 2)
            .await?;
        assert_eq!(unprocessed.len(), 2);

        let bridge_id = storage
            .via_withdrawal_dal()
            .insert_bridge_withdrawal_tx(&vec![0x01, 0x02])
            .await?;

        storage
            .via_withdrawal_dal()
            .mark_withdrawal_as_processed(bridge_id, &withdrawal1)
            .await?;

        let unprocessed = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;
        assert_eq!(unprocessed.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn test_insert_l1_batch_bridge_withdrawals() -> Result<()> {
        let mut storage = create_test_connection().await;

        let proof_tx_id_1 = vec![0x01, 0x02, 0x03, 0x04];
        let proof_tx_id_2 = vec![0x05, 0x06, 0x07, 0x08];

        storage
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_1)
            .await?;

        storage
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_1)
            .await?;

        storage
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_2)
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_mark_withdrawals_as_processed() -> Result<()> {
        let mut storage = create_test_connection().await;

        let withdrawal1 = create_withdrawal_request(
            "test_mark_1",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0x6666666666666666666666666666666666666666",
            0,
        );

        let withdrawal2 = create_withdrawal_request(
            "test_mark_2",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            30000,
            "0x7777777777777777777777777777777777777777",
            1,
        );

        storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal1.clone(), withdrawal2.clone()])
            .await?;

        let bridge_id = storage
            .via_withdrawal_dal()
            .insert_bridge_withdrawal_tx(&vec![0x01, 0x02, 0x03])
            .await?;

        storage
            .via_withdrawal_dal()
            .mark_withdrawals_as_processed(bridge_id, &[withdrawal1.clone(), withdrawal2.clone()])
            .await?;

        let unprocessed = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;

        let unprocessed_ids: Vec<String> = unprocessed.iter().map(|w| w.id.clone()).collect();
        assert!(!unprocessed_ids.contains(&withdrawal1.id));
        assert!(!unprocessed_ids.contains(&withdrawal2.id));
        Ok(())
    }

    #[tokio::test]
    async fn test_get_bridge_withdrawal_id() -> Result<()> {
        let mut storage = create_test_connection().await;

        let tx_id = vec![0x01, 0x02, 0x03, 0x04];

        let id = storage
            .via_withdrawal_dal()
            .get_bridge_withdrawal_id(&tx_id)
            .await?;
        assert!(id.is_none());

        let inserted_id = storage
            .via_withdrawal_dal()
            .insert_bridge_withdrawal_tx(&tx_id)
            .await?;

        let id = storage
            .via_withdrawal_dal()
            .get_bridge_withdrawal_id(&tx_id)
            .await?;
        assert_eq!(id, Some(inserted_id));
        Ok(())
    }

    #[tokio::test]
    async fn test_broadcast_withdrawal_flow_idempotent() -> Result<()> {
        let mut storage = create_test_connection().await;

        let withdrawal1 = create_withdrawal_request(
            "broadcast_flow_1",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0xc0ffee0000000000000000000000000000000001",
            0,
        );
        let withdrawal2 = create_withdrawal_request(
            "broadcast_flow_2",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            30000,
            "0xc0ffee0000000000000000000000000000000002",
            1,
        );

        storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal1.clone(), withdrawal2.clone()])
            .await?;

        let tx_id = vec![0xde, 0xad, 0xbe, 0xef];

        let mut transaction1 = storage.start_transaction().await?;
        let bridge_id1 = transaction1
            .via_withdrawal_dal()
            .insert_bridge_withdrawal_tx(&tx_id)
            .await?;

        transaction1
            .via_withdrawal_dal()
            .mark_withdrawals_as_processed(bridge_id1, &[withdrawal1.clone(), withdrawal2.clone()])
            .await?;
        transaction1.commit().await?;

        let mut transaction2 = storage.start_transaction().await?;
        let bridge_id2 = match transaction2
            .via_withdrawal_dal()
            .get_bridge_withdrawal_id(&tx_id)
            .await?
        {
            Some(id) => id,
            None => {
                transaction2
                    .via_withdrawal_dal()
                    .insert_bridge_withdrawal_tx(&tx_id)
                    .await?
            }
        };
        assert_eq!(bridge_id1, bridge_id2);

        transaction2
            .via_withdrawal_dal()
            .mark_withdrawals_as_processed(bridge_id2, &[withdrawal1.clone(), withdrawal2.clone()])
            .await?;
        transaction2.commit().await?;

        let exists_unprocessed_1 = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal1)
            .await?;
        let exists_unprocessed_2 = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal2)
            .await?;
        assert!(!exists_unprocessed_1);
        assert!(!exists_unprocessed_2);

        let exists_bridge = storage
            .via_withdrawal_dal()
            .bridge_withdrawal_exists(&tx_id)
            .await?;
        assert!(exists_bridge);

        Ok(())
    }

    #[tokio::test]
    async fn test_broadcast_withdrawal_two_different_txids() -> Result<()> {
        let mut storage = create_test_connection().await;

        let withdrawal = create_withdrawal_request(
            "broadcast_two_txids",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            42000,
            "0xabc0000000000000000000000000000000000000",
            0,
        );
        storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;

        let tx_id_a = vec![0x01, 0x02, 0x03, 0x04];
        let tx_id_b = vec![0x05, 0x06, 0x07, 0x08];

        let mut t1 = storage.start_transaction().await?;
        let bridge_a = t1
            .via_withdrawal_dal()
            .insert_bridge_withdrawal_tx(&tx_id_a)
            .await?;
        t1.via_withdrawal_dal()
            .mark_withdrawal_as_processed(bridge_a, &withdrawal)
            .await?;
        t1.commit().await?;

        let mut t2 = storage.start_transaction().await?;
        let bridge_b = t2
            .via_withdrawal_dal()
            .insert_bridge_withdrawal_tx(&tx_id_b)
            .await?;
        t2.via_withdrawal_dal()
            .mark_withdrawal_as_processed(bridge_b, &withdrawal)
            .await?;
        t2.commit().await?;

        let exists_a = storage
            .via_withdrawal_dal()
            .bridge_withdrawal_exists(&tx_id_a)
            .await?;
        let exists_b = storage
            .via_withdrawal_dal()
            .bridge_withdrawal_exists(&tx_id_b)
            .await?;
        assert!(exists_a && exists_b);
        Ok(())
    }

    #[tokio::test]
    async fn test_complete_duplicate_prevention_flow() -> Result<()> {
        let mut storage = create_test_connection().await;

        let proof_tx_id = vec![0x01, 0x02, 0x03, 0x04];
        let withdrawal = create_withdrawal_request(
            "test_complete_flow",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0x8888888888888888888888888888888888888888",
            0,
        );

        storage
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id)
            .await?;

        storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;

        let unprocessed = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;

        assert_eq!(unprocessed.len(), 1);
        assert_eq!(unprocessed[0].id, withdrawal.id);

        let exists_unprocessed = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal)
            .await?;

        assert!(exists_unprocessed);

        let tx_id = vec![0x05, 0x06, 0x07, 0x08];

        let bridge_id = storage
            .via_withdrawal_dal()
            .insert_bridge_withdrawal_tx(&tx_id)
            .await?;

        storage
            .via_withdrawal_dal()
            .mark_withdrawals_as_processed(bridge_id, &[withdrawal.clone()])
            .await?;

        let existing_id = storage
            .via_withdrawal_dal()
            .get_bridge_withdrawal_id(&tx_id)
            .await?;

        assert_eq!(existing_id, Some(bridge_id));

        let unprocessed = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;

        let unprocessed_ids: Vec<String> = unprocessed.iter().map(|w| w.id.clone()).collect();
        assert!(!unprocessed_ids.contains(&withdrawal.id));

        let exists_unprocessed = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal)
            .await?;

        assert!(!exists_unprocessed);
        Ok(())
    }

    #[tokio::test]
    async fn test_concurrent_processing_scenarios() -> Result<()> {
        let mut storage = create_test_connection().await;

        let withdrawal = create_withdrawal_request(
            "test_concurrent",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0x9999999999999999999999999999999999999999",
            0,
        );

        let results1 = storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;

        let results2 = storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;

        assert!(results1[0]);
        assert!(results2[0]);
        let tx_id = vec![0x01, 0x02, 0x03, 0x04];

        let id_opt1 = storage
            .via_withdrawal_dal()
            .get_bridge_withdrawal_id(&tx_id)
            .await?;

        let id1 = match id_opt1 {
            Some(id) => id,
            None => {
                storage
                    .via_withdrawal_dal()
                    .insert_bridge_withdrawal_tx(&tx_id)
                    .await?
            }
        };

        let id_opt2 = storage
            .via_withdrawal_dal()
            .get_bridge_withdrawal_id(&tx_id)
            .await?;

        let id2 = match id_opt2 {
            Some(id) => id,
            None => {
                storage
                    .via_withdrawal_dal()
                    .insert_bridge_withdrawal_tx(&tx_id)
                    .await?
            }
        };

        assert_eq!(id1, id2);

        let proof_tx_id = vec![0x05, 0x06, 0x07, 0x08];

        storage
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id)
            .await?;

        storage
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id)
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_critical_duplicate_prevention_mechanisms() -> Result<()> {
        let mut storage = create_test_connection().await;

        let proof_tx_id = vec![0x01, 0x02, 0x03, 0x04];
        let withdrawal = create_withdrawal_request(
            "test_critical_1",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            0,
        );

        storage
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id)
            .await?;
        storage
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id)
            .await?;

        storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;
        storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;

        let tx_id = vec![0x05, 0x06, 0x07, 0x08];

        let id_opt = storage
            .via_withdrawal_dal()
            .get_bridge_withdrawal_id(&tx_id)
            .await?;

        let id = match id_opt {
            Some(id) => id,
            None => {
                storage
                    .via_withdrawal_dal()
                    .insert_bridge_withdrawal_tx(&tx_id)
                    .await?
            }
        };

        let id_opt2 = storage
            .via_withdrawal_dal()
            .get_bridge_withdrawal_id(&tx_id)
            .await?;

        let id2 = match id_opt2 {
            Some(id) => id,
            None => {
                storage
                    .via_withdrawal_dal()
                    .insert_bridge_withdrawal_tx(&tx_id)
                    .await?
            }
        };

        assert_eq!(id, id2);

        let bridge_id_opt = storage
            .via_withdrawal_dal()
            .get_bridge_withdrawal_id(&vec![0x09, 0x0a])
            .await?;

        let bridge_id = match bridge_id_opt {
            Some(id) => id,
            None => {
                storage
                    .via_withdrawal_dal()
                    .insert_bridge_withdrawal_tx(&vec![0x09, 0x0a])
                    .await?
            }
        };

        storage
            .via_withdrawal_dal()
            .mark_withdrawal_as_processed(bridge_id, &withdrawal)
            .await?;

        let exists_unprocessed = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal)
            .await?;
        assert!(!exists_unprocessed);

        let exists = storage
            .via_withdrawal_dal()
            .bridge_withdrawal_exists(&tx_id)
            .await?;
        assert!(exists);

        let withdrawal2 = create_withdrawal_request(
            "test_critical_2",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            30000,
            "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            1,
        );

        storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal2.clone()])
            .await?;
        storage
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal2.clone()])
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_prepare_withdrawal_session_method_simulation() -> Result<()> {
        let mut storage = create_test_connection().await;

        let _l1_batches = storage
            .via_withdrawal_dal()
            .list_finalized_blocks_with_no_bridge_withdrawal()
            .await?;

        let proof_tx_id_1 = vec![0x01, 0x02, 0x03, 0x04];
        let proof_tx_id_2 = vec![0x05, 0x06, 0x07, 0x08];

        let withdrawal1 = create_withdrawal_request(
            "test_prepare_method_1",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0x1111111111111111111111111111111111111111",
            0,
        );

        let withdrawal2 = create_withdrawal_request(
            "test_prepare_method_2",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            30000,
            "0x2222222222222222222222222222222222222222",
            1,
        );

        let mut transaction = storage.start_transaction().await?;

        transaction
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_1)
            .await?;
        transaction
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal1.clone()])
            .await?;

        transaction
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_2)
            .await?;
        transaction
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal2.clone()])
            .await?;

        transaction.commit().await?;

        let exists1 = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal1)
            .await?;
        assert!(exists1);

        let exists2 = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal2)
            .await?;
        assert!(exists2);

        let unprocessed = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;

        let unprocessed_ids: Vec<String> = unprocessed.iter().map(|w| w.id.clone()).collect();
        assert!(unprocessed_ids.contains(&withdrawal1.id));
        assert!(unprocessed_ids.contains(&withdrawal2.id));
        Ok(())
    }

    #[tokio::test]
    async fn test_prepare_withdrawal_session_duplicate_calls_simulation() -> Result<()> {
        let mut storage = create_test_connection().await;

        let proof_tx_id = vec![0x01, 0x02, 0x03, 0x04];
        let withdrawal = create_withdrawal_request(
            "test_duplicate_calls",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0x3333333333333333333333333333333333333333",
            0,
        );

        let mut transaction1 = storage.start_transaction().await?;
        transaction1
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id)
            .await?;
        transaction1
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;
        transaction1.commit().await?;

        let exists_after_first = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal)
            .await?;
        assert!(exists_after_first);

        let unprocessed_after_first = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;
        let count_after_first = unprocessed_after_first
            .iter()
            .filter(|w| w.id == withdrawal.id)
            .count();
        assert_eq!(count_after_first, 1);

        let mut transaction2 = storage.start_transaction().await?;
        transaction2
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id)
            .await?;
        transaction2
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;
        transaction2.commit().await?;

        let exists_after_second = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal)
            .await?;
        assert!(exists_after_second);

        let unprocessed_after_second = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;
        let count_after_second = unprocessed_after_second
            .iter()
            .filter(|w| w.id == withdrawal.id)
            .count();
        assert_eq!(count_after_second, 1);

        let mut transaction3 = storage.start_transaction().await?;
        transaction3
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id)
            .await?;
        transaction3
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;
        transaction3.commit().await?;

        let exists_after_third = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal)
            .await?;
        assert!(exists_after_third);

        let unprocessed_after_third = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;
        let count_after_third = unprocessed_after_third
            .iter()
            .filter(|w| w.id == withdrawal.id)
            .count();
        assert_eq!(count_after_third, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_prepare_withdrawal_session_empty_batches_simulation() -> Result<()> {
        let mut storage = create_test_connection().await;

        let l1_batches = storage
            .via_withdrawal_dal()
            .list_finalized_blocks_with_no_bridge_withdrawal()
            .await?;

        assert!(l1_batches.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_prepare_withdrawal_session_transaction_rollback_simulation() -> Result<()> {
        let mut storage = create_test_connection().await;

        let proof_tx_id = vec![0x01, 0x02, 0x03, 0x04];
        let withdrawal = create_withdrawal_request(
            "test_rollback",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0x4444444444444444444444444444444444444444",
            0,
        );

        let mut transaction = storage.start_transaction().await?;

        transaction
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id)
            .await?;

        transaction
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal.clone()])
            .await?;

        drop(transaction);

        let exists = storage
            .via_withdrawal_dal()
            .check_if_withdrawal_exists_unprocessed(&withdrawal)
            .await?;
        assert!(!exists);

        let unprocessed = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;
        let unprocessed_ids: Vec<String> = unprocessed.iter().map(|w| w.id.clone()).collect();
        assert!(!unprocessed_ids.contains(&withdrawal.id));
        Ok(())
    }

    #[tokio::test]
    async fn test_prepare_withdrawal_session_complete_workflow_simulation() -> Result<()> {
        let mut storage = create_test_connection().await;

        let proof_tx_id_1 = vec![0x01, 0x02, 0x03, 0x04];
        let proof_tx_id_2 = vec![0x05, 0x06, 0x07, 0x08];
        let proof_tx_id_3 = vec![0x09, 0x0a, 0x0b, 0x0c];

        let withdrawal1 = create_withdrawal_request(
            "test_workflow_1",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            50000,
            "0x5555555555555555555555555555555555555555",
            0,
        );

        let withdrawal2 = create_withdrawal_request(
            "test_workflow_2",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            30000,
            "0x6666666666666666666666666666666666666666",
            1,
        );

        let withdrawal3 = create_withdrawal_request(
            "test_workflow_3",
            "bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56",
            10000,
            "0x7777777777777777777777777777777777777777",
            2,
        );

        let mut transaction1 = storage.start_transaction().await?;

        transaction1
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_1)
            .await?;
        transaction1
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal1.clone()])
            .await?;

        transaction1
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_2)
            .await?;
        transaction1
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal2.clone()])
            .await?;

        transaction1.commit().await?;

        let unprocessed_after_first = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;
        assert_eq!(unprocessed_after_first.len(), 2);

        let mut transaction2 = storage.start_transaction().await?;

        transaction2
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_2)
            .await?;
        transaction2
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal2.clone()])
            .await?;

        transaction2
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_3)
            .await?;
        transaction2
            .via_withdrawal_dal()
            .insert_withdrawals(&[withdrawal3.clone()])
            .await?;

        transaction2.commit().await?;

        let unprocessed_after_second = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;
        assert_eq!(unprocessed_after_second.len(), 3);

        let unprocessed_ids: Vec<String> = unprocessed_after_second
            .iter()
            .map(|w| w.id.clone())
            .collect();
        assert!(unprocessed_ids.contains(&withdrawal1.id));
        assert!(unprocessed_ids.contains(&withdrawal2.id));
        assert!(unprocessed_ids.contains(&withdrawal3.id));

        assert_eq!(
            unprocessed_ids
                .iter()
                .filter(|&id| id == &withdrawal1.id)
                .count(),
            1
        );
        assert_eq!(
            unprocessed_ids
                .iter()
                .filter(|&id| id == &withdrawal2.id)
                .count(),
            1
        );
        assert_eq!(
            unprocessed_ids
                .iter()
                .filter(|&id| id == &withdrawal3.id)
                .count(),
            1
        );

        let mut transaction3 = storage.start_transaction().await?;

        transaction3
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_1)
            .await?;
        transaction3
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_2)
            .await?;
        transaction3
            .via_withdrawal_dal()
            .insert_l1_batch_bridge_withdrawals(&proof_tx_id_3)
            .await?;
        transaction3
            .via_withdrawal_dal()
            .insert_withdrawals(&[
                withdrawal1.clone(),
                withdrawal2.clone(),
                withdrawal3.clone(),
            ])
            .await?;

        transaction3.commit().await?;

        let unprocessed_after_third = storage
            .via_withdrawal_dal()
            .list_no_processed_withdrawals(0, 10)
            .await?;
        assert_eq!(unprocessed_after_third.len(), 3);

        let unprocessed_ids_final: Vec<String> = unprocessed_after_third
            .iter()
            .map(|w| w.id.clone())
            .collect();
        assert_eq!(
            unprocessed_ids_final
                .iter()
                .filter(|&id| id == &withdrawal1.id)
                .count(),
            1
        );
        assert_eq!(
            unprocessed_ids_final
                .iter()
                .filter(|&id| id == &withdrawal2.id)
                .count(),
            1
        );
        assert_eq!(
            unprocessed_ids_final
                .iter()
                .filter(|&id| id == &withdrawal3.id)
                .count(),
            1
        );

        Ok(())
    }
}
