use anyhow::{Context, Result};
use via_btc_client::{inscriber::Inscriber, traits::BitcoinOps};

mod deposit_utils;
use deposit_utils::{config, DepositTestUtils};

#[cfg(test)]
mod integration_tests {
    use tracing_test::traced_test;

    use super::*;

    #[tokio::test]
    #[traced_test]
    async fn test_inscriber_deposit_with_real_bitcoin_node() -> Result<()> {
        let unique_private_key = "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc";

        DepositTestUtils::setup_funded_inscriber_with_key(unique_private_key).await?;

        DepositTestUtils::perform_deposit_test_with_key(None, unique_private_key).await?;

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_inscriber_deposit_with_container_user() -> Result<()> {
        let unique_private_key = "cQYPr9i7N9WA1js1SqGHUwcDNkFVxWikZQJAALasoDpzN5QuJrJE";

        let client = DepositTestUtils::create_bitcoin_client()?;
        let block_count = client.fetch_block_height().await?;
        assert!(block_count > 0, "Block height should be greater than 0");

        let (_, inscriber) =
            DepositTestUtils::setup_funded_inscriber_with_key(unique_private_key).await?;

        let balance = inscriber.get_balance().await?;

        assert!(
            balance >= config::DEFAULT_DEPOSIT_AMOUNT_SATS as u128,
            "Insufficient balance: {} < {}",
            balance,
            config::DEFAULT_DEPOSIT_AMOUNT_SATS
        );

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_inscriber_multiple_users_different_amounts() -> Result<()> {
        let test_users = vec![
            (
                "User1",
                "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc",
                50000,
            ),
            (
                "User2",
                "cQYPr9i7N9WA1js1SqGHUwcDNkFVxWikZQJAALasoDpzN5QuJrJE",
                100000,
            ),
            (
                "User3",
                "cPuM8wv8rGA5EPD8DKJF2sirHdkdrk9JSVXEiyv8F6JCUGziXx19",
                250000,
            ),
            (
                "User4",
                "cNTBWvTjsqpFz2T7yUqftjcXuWneB6rhNviHbigYyDdXdUXiYtww",
                500000,
            ),
        ];

        let enable_l2_balance_check = false;
        let max_retry_attempts = 3;
        let retry_delay_ms = 1000;
        let continue_on_failure = true;

        let mut successful_deposits = Vec::new();
        let mut failed_deposits = Vec::new();

        for (user_name, private_key, amount_sats) in test_users {
            let (_, inscriber) = DepositTestUtils::setup_funded_inscriber_with_key(private_key)
                .await
                .expect(&format!("Failed to setup funding for {}", user_name));

            let balance = inscriber.get_balance().await?;

            assert!(
                balance >= amount_sats as u128,
                "Insufficient balance for {}: {} < {} sats",
                user_name,
                balance,
                amount_sats
            );

            let mut attempt = 1;
            let mut last_error = None;

            while attempt <= max_retry_attempts {
                match DepositTestUtils::perform_deposit_test_with_key_and_l2_check(
                    Some(amount_sats),
                    private_key,
                    enable_l2_balance_check,
                )
                .await
                {
                    Ok(_) => {
                        successful_deposits.push((user_name, amount_sats));
                        break;
                    }
                    Err(e) => {
                        last_error = Some(e);

                        if attempt < max_retry_attempts {
                            tokio::time::sleep(tokio::time::Duration::from_millis(retry_delay_ms))
                                .await;
                        }
                        attempt += 1;
                    }
                }
            }

            if attempt > max_retry_attempts {
                if let Some(error) = last_error {
                    failed_deposits.push((user_name, amount_sats, error.to_string()));

                    if !continue_on_failure {
                        panic!("Deposit failed for {} after {} attempts and continue_on_failure is false", 
                               user_name, max_retry_attempts);
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        assert!(
            !successful_deposits.is_empty(),
            "All deposits failed - no successful deposits"
        );

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_opreturn_deposit_with_real_bitcoin_node() -> Result<()> {
        let unique_private_key = "cPuM8wv8rGA5EPD8DKJF2sirHdkdrk9JSVXEiyv8F6JCUGziXx19";

        let address = DepositTestUtils::get_address_from_private_key(unique_private_key).await?;
        DepositTestUtils::fund_test_address(&address, 0.1).await?;

        let txid =
            DepositTestUtils::perform_opreturn_deposit_test(None, unique_private_key).await?;

        assert!(!txid.is_empty(), "Transaction ID should not be empty");

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_opreturn_deposit_multiple_amounts() -> Result<()> {
        let test_amounts = vec![50000, 100000, 250000, 500000];

        for (i, amount_sats) in test_amounts.iter().enumerate() {
            let unique_private_key = match i {
                0 => "cNTBWvTjsqpFz2T7yUqftjcXuWneB6rhNviHbigYyDdXdUXiYtww",
                1 => "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc",
                2 => "cQYPr9i7N9WA1js1SqGHUwcDNkFVxWikZQJAALasoDpzN5QuJrJE",
                3 => "cPuM8wv8rGA5EPD8DKJF2sirHdkdrk9JSVXEiyv8F6JCUGziXx19",
                _ => "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc",
            };

            let address =
                DepositTestUtils::get_address_from_private_key(unique_private_key).await?;
            DepositTestUtils::fund_test_address(&address, 0.1).await?;

            let txid = DepositTestUtils::perform_opreturn_deposit_test(
                Some(*amount_sats),
                unique_private_key,
            )
            .await?;

            assert!(
                !txid.is_empty(),
                "Transaction ID should not be empty for amount {}",
                amount_sats
            );

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_insufficient_funds_deposit_failure() -> Result<()> {
        let insufficient_private_key = "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc";

        let client = DepositTestUtils::create_bitcoin_client()?;
        let context = None;
        let inscriber = Inscriber::new(client.clone(), insufficient_private_key, context)
            .await
            .context("Failed to create test Inscriber")?;

        let current_balance = inscriber.get_balance().await?;
        println!("Current balance: {} sats", current_balance);

        let excessive_amount = current_balance + 1_000_000;
        println!(
            "Attempting to deposit: {} sats (exceeds balance by 1M sats)",
            excessive_amount
        );

        let result = DepositTestUtils::perform_deposit_test_with_key(
            Some(excessive_amount as u64),
            insufficient_private_key,
        )
        .await;

        assert!(
            result.is_err(),
            "Deposit should fail with insufficient funds"
        );

        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("Insufficient balance")
                || error_message.contains("Insufficient funds")
                || error_message.contains("bad-txns-inputs-missingorspent"),
            "Error message should indicate insufficient balance/funds or UTXO issues, got: {}",
            error_message
        );

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_opreturn_insufficient_funds_deposit_failure() -> Result<()> {
        let insufficient_private_key = "cQYPr9i7N9WA1js1SqGHUwcDNkFVxWikZQJAALasoDpzN5QuJrJE";

        let client = DepositTestUtils::create_bitcoin_client()?;
        let context = None;
        let inscriber = Inscriber::new(client.clone(), insufficient_private_key, context)
            .await
            .context("Failed to create test Inscriber")?;

        let current_balance = inscriber.get_balance().await?;
        println!("Current balance: {} sats", current_balance);

        let excessive_amount = current_balance + 1_000_000;
        println!(
            "Attempting to deposit: {} sats (exceeds balance by 1M sats)",
            excessive_amount
        );

        let bridge_address = DepositTestUtils::get_bridge_address()?;
        let receiver_l2_address = DepositTestUtils::get_test_receiver_l2_address()?;

        let result = super::deposit_utils::DepositTestUtils::create_opreturn_transaction(
            &client,
            insufficient_private_key,
            excessive_amount as u64,
            &bridge_address,
            &receiver_l2_address,
        )
        .await;

        assert!(
            result.is_err(),
            "OP_RETURN deposit should fail with insufficient funds"
        );

        let error_message = result.unwrap_err().to_string();
        assert!(
            error_message.contains("Insufficient funds")
                || error_message.contains("Insufficient balance"),
            "Error message should indicate insufficient balance/funds, got: {}",
            error_message
        );

        Ok(())
    }
}
