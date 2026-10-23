use anyhow::{Context, Result};
use via_test_utils::bitcoin_core::BitcoinCore;
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
        let core = BitcoinCore::new()?;
        core.mine(1)?;
        let unique_private_key = "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc";

        DepositTestUtils::setup_funded_inscriber_with_key(&core, unique_private_key).await?;

        DepositTestUtils::perform_deposit_test_with_key(&core, None, unique_private_key).await?;

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_inscriber_deposit_with_container_user() -> Result<()> {
        let core = BitcoinCore::new()?;
        core.mine(1)?;
        let unique_private_key = "cQYPr9i7N9WA1js1SqGHUwcDNkFVxWikZQJAALasoDpzN5QuJrJE";

        let client = DepositTestUtils::create_bitcoin_client(&core)?;
        let block_count = client.fetch_block_height().await?;
        assert!(block_count > 0, "Block height should be greater than 0");

        let (_, inscriber) =
            DepositTestUtils::setup_funded_inscriber_with_key(&core, unique_private_key).await?;

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
        let core = BitcoinCore::new()?;
        core.mine(1)?;
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

        let mut successful_deposits = Vec::new();
        for (user_name, private_key, amount_sats) in test_users {
            let (_, inscriber) = DepositTestUtils::setup_funded_inscriber_with_key(&core, private_key).await?;
            let balance = inscriber.get_balance().await?;
            assert!(balance >= amount_sats as u128, "Insufficient balance for {user_name}: {balance} < {amount_sats}");
            DepositTestUtils::perform_deposit_test_with_key(&core, Some(amount_sats), private_key).await?;
            successful_deposits.push(user_name);
        }
        assert_eq!(successful_deposits.len(), 4);

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_opreturn_deposit_with_real_bitcoin_node() -> Result<()> {
        let core = BitcoinCore::new()?;
        core.mine(1)?;
        let unique_private_key = "cPuM8wv8rGA5EPD8DKJF2sirHdkdrk9JSVXEiyv8F6JCUGziXx19";

        let address = DepositTestUtils::get_address_from_private_key(unique_private_key).await?;
        DepositTestUtils::fund_test_address(&core, &address, 0.1).await?;

        let txid = DepositTestUtils::perform_opreturn_deposit_test(&core, None, unique_private_key)
            .await?;

        assert!(!txid.is_empty(), "Transaction ID should not be empty");

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_opreturn_deposit_multiple_amounts() -> Result<()> {
        let core = BitcoinCore::new()?;
        core.mine(1)?;
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
            DepositTestUtils::fund_test_address(&core, &address, 0.1).await?;

            let txid = DepositTestUtils::perform_opreturn_deposit_test(&core,
                Some(*amount_sats),
                unique_private_key,
            )
            .await?;

            assert!(
                !txid.is_empty(),
                "Transaction ID should not be empty for amount {}",
                amount_sats
            );

        }

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_insufficient_funds_deposit_failure() -> Result<()> {
        let core = BitcoinCore::new()?;
        core.mine(1)?;
        let insufficient_private_key = "cNxiyS3cffhwK6x5sp72LiyhvP7QkM8o4VDJVLya3yaRXc3QPJYc";

        let client = DepositTestUtils::create_bitcoin_client(&core)?;
        let excessive_amount =
            unfunded_balance(&client, insufficient_private_key).await? + 1_000_000;

        let result = DepositTestUtils::perform_deposit_test_with_key(&core,
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
        let core = BitcoinCore::new()?;
        core.mine(1)?;
        let insufficient_private_key = "cQYPr9i7N9WA1js1SqGHUwcDNkFVxWikZQJAALasoDpzN5QuJrJE";

        let client = DepositTestUtils::create_bitcoin_client(&core)?;
        let excessive_amount =
            unfunded_balance(&client, insufficient_private_key).await? + 1_000_000;

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
    async fn unfunded_balance(
        client: &std::sync::Arc<via_btc_client::client::BitcoinClient>,
        key: &str,
    ) -> Result<u128> {
        let inscriber = Inscriber::new(client.clone(), key, None)
            .await
            .context("Failed to create test Inscriber")?;
        inscriber.get_balance().await
    }
}
