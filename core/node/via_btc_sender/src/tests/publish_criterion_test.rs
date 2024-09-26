#[cfg(test)]
mod tests {
    use chrono::Utc;
    use zksync_types::L1BatchNumber;

    use crate::{
        publish_criterion::{
            TimestampDeadlineCriterion, ViaBtcL1BatchCommitCriterion, ViaNumberCriterion,
        },
        tests::utils::create_btc_l1_batch_details,
    };

    #[tokio::test]
    async fn test_number_criterion() {
        // When one batch and limit 1.
        let expected_l1_batch_number = L1BatchNumber::from(1);
        let mut number_criterion = ViaNumberCriterion { limit: 1 };

        let consecutive_l1_batches = vec![create_btc_l1_batch_details(expected_l1_batch_number, 0)];
        let res = number_criterion
            .last_l1_batch_to_publish(&consecutive_l1_batches)
            .await;
        assert_eq!(res, Some(expected_l1_batch_number));

        // When many batches and limit 1.
        let consecutive_l1_batches = vec![
            create_btc_l1_batch_details(expected_l1_batch_number, 0),
            create_btc_l1_batch_details(L1BatchNumber::from(2), 0),
        ];

        let res = number_criterion
            .last_l1_batch_to_publish(&consecutive_l1_batches)
            .await;
        assert_eq!(res, Some(expected_l1_batch_number));

        // When one batch and limit 10.
        let mut number_criterion = ViaNumberCriterion { limit: 10 };
        let consecutive_l1_batches = vec![create_btc_l1_batch_details(expected_l1_batch_number, 0)];

        let res = number_criterion
            .last_l1_batch_to_publish(&consecutive_l1_batches)
            .await;
        assert_eq!(res, None);

        // When multiple batches and match the limit
        let total_blocks = 10;
        let expected_l1_batch_number = L1BatchNumber::from(9);
        let mut number_criterion = ViaNumberCriterion {
            limit: total_blocks,
        };
        let mut consecutive_l1_batches = Vec::with_capacity(total_blocks as usize);
        for index in 0..total_blocks {
            consecutive_l1_batches.push(create_btc_l1_batch_details(L1BatchNumber::from(index), 0));
        }

        let res = number_criterion
            .last_l1_batch_to_publish(&consecutive_l1_batches)
            .await;
        assert_eq!(
            res,
            Some(consecutive_l1_batches.clone()[expected_l1_batch_number.0 as usize].number)
        );
    }

    #[tokio::test]
    async fn test_timestamp_criterion() {
        // When one batch and when batch timestamp + deadline_seconds <= now() and deadline_seconds=0
        let expected_l1_batch_number = L1BatchNumber::from(1);
        let timestamp = Utc::now().timestamp();

        let mut timestamp_criterion = TimestampDeadlineCriterion {
            deadline_seconds: 0,
        };

        let consecutive_l1_batches = vec![create_btc_l1_batch_details(
            expected_l1_batch_number,
            timestamp,
        )];
        let res = timestamp_criterion
            .last_l1_batch_to_publish(&consecutive_l1_batches)
            .await;
        assert_eq!(res, Some(expected_l1_batch_number));

        // When one batch and when batch timestamp + deadline_seconds > now() and deadline_seconds=60*60*24 (24hour)
        let mut timestamp_criterion = TimestampDeadlineCriterion {
            deadline_seconds: 60 * 60 * 24,
        };

        let consecutive_l1_batches = vec![create_btc_l1_batch_details(
            expected_l1_batch_number,
            timestamp,
        )];

        let res = timestamp_criterion
            .last_l1_batch_to_publish(&consecutive_l1_batches)
            .await;
        assert_eq!(res, None);

        // When many batches and when batches' timestamp + deadline_seconds <= now() and deadline_seconds=0
        let total_blocks = 10;
        let expected_l1_batch_number = L1BatchNumber::from(9);
        let mut timestamp_criterion = TimestampDeadlineCriterion {
            deadline_seconds: 0,
        };
        let mut consecutive_l1_batches = Vec::with_capacity(total_blocks as usize);
        for index in 0..total_blocks {
            consecutive_l1_batches.push(create_btc_l1_batch_details(L1BatchNumber::from(index), 0));
        }
        let res = timestamp_criterion
            .last_l1_batch_to_publish(&consecutive_l1_batches)
            .await;
        assert_eq!(res, Some(expected_l1_batch_number));

        // When many batches and when some batches' timestamp + deadline_seconds <= now() and deadline_seconds=0
        let total_blocks = 10;
        let expected_l1_batch_number = L1BatchNumber::from(5);
        let mut timestamp_criterion = TimestampDeadlineCriterion {
            deadline_seconds: 0,
        };
        let mut consecutive_l1_batches = Vec::with_capacity(total_blocks as usize);
        for index in 0..total_blocks {
            let mut ts = timestamp;
            if index > expected_l1_batch_number.0 {
                ts += 60 * 60 * 24;
            }
            consecutive_l1_batches
                .push(create_btc_l1_batch_details(L1BatchNumber::from(index), ts));
        }
        let res = timestamp_criterion
            .last_l1_batch_to_publish(&consecutive_l1_batches)
            .await;
        assert_eq!(res, Some(expected_l1_batch_number));
    }
}
