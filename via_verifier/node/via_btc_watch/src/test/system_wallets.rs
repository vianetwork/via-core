#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};

    use via_btc_client::types::BitcoinAddress;
    use via_test_utils::utils::{
        create_update_bridge_inscription, create_update_governance_inscription,
        create_update_sequencer_inscription, random_bitcoin_wallet, test_bitcoin_client,
        test_create_indexer, test_wallets,
    };
    use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
    use zksync_types::via_wallet::{SystemWallets, SystemWalletsDetails};

    use crate::{message_processors::SystemWalletProcessor, MessageProcessor};

    #[tokio::test]
    async fn test_update_sequencer_wallet() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer();

        let system_wallet_map = SystemWalletsDetails::try_from(test_wallets())?;

        pool.connection()
            .await?
            .via_wallet_dal()
            .insert_wallets(&system_wallet_map)
            .await?;

        let mut processor = SystemWalletProcessor::new(Arc::new(test_bitcoin_client()));
        let new_sequencer_address = random_bitcoin_wallet().1;
        let msg = create_update_sequencer_inscription(new_sequencer_address.clone());

        let old_wallets = indexer.get_state();

        processor
            .process_messages(&mut pool.connection().await?, vec![msg], &mut indexer)
            .await?;

        let new_wallets = indexer.get_state();

        assert_ne!(new_wallets, old_wallets);
        assert_eq!(new_wallets.sequencer, new_sequencer_address);

        let system_wallets_db_map = pool
            .connection()
            .await?
            .via_wallet_dal()
            .get_system_wallets_raw()
            .await?
            .unwrap();

        let system_wallets_db = Arc::new(SystemWallets::try_from(system_wallets_db_map.clone())?);

        assert_eq!(system_wallets_db, new_wallets);

        Ok(())
    }

    #[tokio::test]
    async fn test_update_governance_wallet() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer();

        let system_wallet_map = SystemWalletsDetails::try_from(test_wallets())?;

        pool.connection()
            .await?
            .via_wallet_dal()
            .insert_wallets(&system_wallet_map)
            .await?;

        let mut processor = SystemWalletProcessor::new(Arc::new(test_bitcoin_client()));
        let new_governance_address = random_bitcoin_wallet().1;
        let msg = create_update_governance_inscription(new_governance_address.clone());

        let old_wallets = indexer.get_state();

        processor
            .process_messages(&mut pool.connection().await?, vec![msg], &mut indexer)
            .await?;

        let new_wallets = indexer.get_state();

        assert_ne!(new_wallets, old_wallets);
        assert_eq!(new_wallets.governance, new_governance_address);

        let system_wallets_db_map = pool
            .connection()
            .await?
            .via_wallet_dal()
            .get_system_wallets_raw()
            .await?
            .unwrap();

        let system_wallets_db = Arc::new(SystemWallets::try_from(system_wallets_db_map.clone())?);

        assert_eq!(system_wallets_db, new_wallets);

        Ok(())
    }

    #[tokio::test]
    async fn test_update_bridge_wallet_with_4_new_verifiers_when_old_3() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer();

        let system_wallet_map = SystemWalletsDetails::try_from(test_wallets())?;
        pool.connection()
            .await?
            .via_wallet_dal()
            .insert_wallets(&system_wallet_map)
            .await?;

        let mut processor = SystemWalletProcessor::new(Arc::new(test_bitcoin_client()));
        let new_bridge_address = BitcoinAddress::from_str(
            &"bcrt1pcx974cg2w66cqhx67zadf85t8k4sd2wp68l8x8agd3aj4tuegsgsz97amg",
        )?
        .assume_checked();

        let new_verifier_1 = random_bitcoin_wallet().1;
        let new_verifier_2 = random_bitcoin_wallet().1;
        let new_verifier_3 = random_bitcoin_wallet().1;
        let new_verifier_4 = random_bitcoin_wallet().1;

        let new_verifiers = vec![
            new_verifier_1,
            new_verifier_2,
            new_verifier_3,
            new_verifier_4,
        ];

        let msg =
            create_update_bridge_inscription(new_bridge_address.clone(), new_verifiers.clone())
                .await?;

        let old_wallets = indexer.get_state();

        processor
            .process_messages(&mut pool.connection().await?, vec![msg], &mut indexer)
            .await?;

        let new_wallets = indexer.get_state();

        assert_ne!(new_wallets, old_wallets);
        assert_eq!(new_wallets.bridge, new_bridge_address);

        let system_wallets_db_map = pool
            .connection()
            .await?
            .via_wallet_dal()
            .get_system_wallets_raw()
            .await?
            .unwrap();

        let system_wallets_db = Arc::new(SystemWallets::try_from(system_wallets_db_map.clone())?);

        assert_eq!(system_wallets_db, new_wallets);

        Ok(())
    }

    #[tokio::test]
    async fn test_update_bridge_wallet_with_2_new_verifiers_when_old_3() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer();

        let system_wallet_map = SystemWalletsDetails::try_from(test_wallets())?;
        pool.connection()
            .await?
            .via_wallet_dal()
            .insert_wallets(&system_wallet_map)
            .await?;

        let mut processor = SystemWalletProcessor::new(Arc::new(test_bitcoin_client()));
        let new_bridge_address = BitcoinAddress::from_str(
            &"bcrt1pcx974cg2w66cqhx67zadf85t8k4sd2wp68l8x8agd3aj4tuegsgsz97amg",
        )?
        .assume_checked();

        let new_verifier_1 = random_bitcoin_wallet().1;
        let new_verifier_2 = random_bitcoin_wallet().1;

        let new_verifiers = vec![new_verifier_1, new_verifier_2];

        let msg =
            create_update_bridge_inscription(new_bridge_address.clone(), new_verifiers.clone())
                .await?;

        let old_wallets = indexer.get_state();

        processor
            .process_messages(&mut pool.connection().await?, vec![msg], &mut indexer)
            .await?;

        let new_wallets = indexer.get_state();

        assert_ne!(new_wallets, old_wallets);
        assert_eq!(new_wallets.bridge, new_bridge_address);

        let system_wallets_db_map = pool
            .connection()
            .await?
            .via_wallet_dal()
            .get_system_wallets_raw()
            .await?
            .unwrap();

        let system_wallets_db = Arc::new(SystemWallets::try_from(system_wallets_db_map.clone())?);

        assert_eq!(system_wallets_db, new_wallets);

        Ok(())
    }

    #[tokio::test]
    async fn test_update_governance_bridge_wallet_and_sequencer() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer();

        let system_wallet_map = SystemWalletsDetails::try_from(test_wallets())?;
        pool.connection()
            .await?
            .via_wallet_dal()
            .insert_wallets(&system_wallet_map)
            .await?;

        let mut processor = SystemWalletProcessor::new(Arc::new(test_bitcoin_client()));

        let new_governance_address = random_bitcoin_wallet().1;
        let gov_msg = create_update_governance_inscription(new_governance_address.clone());

        let new_sequencer_address = random_bitcoin_wallet().1;
        let sequencer_msg = create_update_sequencer_inscription(new_sequencer_address.clone());

        let new_bridge_address = BitcoinAddress::from_str(
            &"bcrt1pcx974cg2w66cqhx67zadf85t8k4sd2wp68l8x8agd3aj4tuegsgsz97amg",
        )?
        .assume_checked();

        let new_verifier_1 = random_bitcoin_wallet().1;
        let new_verifier_2 = random_bitcoin_wallet().1;
        let new_verifier_3 = random_bitcoin_wallet().1;
        let new_verifier_4 = random_bitcoin_wallet().1;

        let new_verifiers = vec![
            new_verifier_1,
            new_verifier_2,
            new_verifier_3,
            new_verifier_4,
        ];

        let bridge_msg =
            create_update_bridge_inscription(new_bridge_address.clone(), new_verifiers.clone())
                .await?;

        let old_wallets = indexer.get_state();

        processor
            .process_messages(
                &mut pool.connection().await?,
                vec![bridge_msg, sequencer_msg, gov_msg],
                &mut indexer,
            )
            .await?;

        let new_wallets = indexer.get_state();

        assert_ne!(new_wallets, old_wallets);
        assert_eq!(new_wallets.bridge, new_bridge_address);
        assert_eq!(new_wallets.sequencer, new_sequencer_address);

        let system_wallets_db_map = pool
            .connection()
            .await?
            .via_wallet_dal()
            .get_system_wallets_raw()
            .await?
            .unwrap();

        let system_wallets_db = Arc::new(SystemWallets::try_from(system_wallets_db_map.clone())?);

        assert_eq!(system_wallets_db, new_wallets);

        Ok(())
    }
}
