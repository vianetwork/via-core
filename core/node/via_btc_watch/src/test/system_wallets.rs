#[cfg(test)]
mod tests {
    use std::{i64, str::FromStr, sync::Arc};

    use via_btc_client::{
        indexer::BitcoinInscriptionIndexer,
        types::{BitcoinAddress, FullInscriptionMessage},
    };
    use via_test_utils::bitcoin_core::BitcoinCore;
    use via_test_utils::utils::{
        create_update_bridge_inscription, create_update_governance_inscription,
        create_update_sequencer_inscription, random_bitcoin_wallet, test_create_indexer,
        test_wallets,
    };
    use zksync_dal::{ConnectionPool, Core, CoreDal};
    use zksync_types::via_wallet::{SystemWallets, SystemWalletsDetails};

    use crate::{message_processors::SystemWalletProcessor, MessageProcessor};

    #[tokio::test]
    async fn test_update_sequencer_wallet() -> anyhow::Result<()> {
        let (pool, mut indexer, mut processor, _core) = setup().await?;
        let new_sequencer_address = random_bitcoin_wallet().1;
        let msg = create_update_sequencer_inscription(new_sequencer_address.clone());

        let old_wallets = process_role_update(&pool, &mut indexer, &mut processor, msg).await?;
        assert_eq!(
            stored_wallets(&pool).await?,
            SystemWallets { sequencer: new_sequencer_address, ..(*old_wallets).clone() }
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_update_governance_wallet() -> anyhow::Result<()> {
        let (pool, mut indexer, mut processor, _core) = setup().await?;
        let new_governance_address = random_bitcoin_wallet().1;
        let msg = create_update_governance_inscription(new_governance_address.clone());

        let old_wallets = process_role_update(&pool, &mut indexer, &mut processor, msg).await?;
        let expected_wallets =
            SystemWallets { governance: new_governance_address, ..(*old_wallets).clone() };

        assert_eq!(stored_wallets(&pool).await?, expected_wallets);

        Ok(())
    }

    #[tokio::test]
    async fn test_update_bridge_wallet_with_4_new_verifiers_when_old_3() -> anyhow::Result<()> {
        check_bridge_update((0..4).map(|_| random_bitcoin_wallet().1).collect()).await
    }

    #[tokio::test]
    async fn test_update_bridge_wallet_with_2_new_verifiers_when_old_3() -> anyhow::Result<()> {
        check_bridge_update((0..2).map(|_| random_bitcoin_wallet().1).collect()).await
    }

    async fn check_bridge_update(new_verifiers: Vec<BitcoinAddress>) -> anyhow::Result<()> {
        let (pool, mut indexer, mut processor, _core) = setup().await?;

        let (msg, new_bridge_address) = bridge_update(&_core, new_verifiers.clone()).await?;

        let old_wallets = indexer.get_state();

        processor.process_messages(&mut pool.connection().await?, vec![msg], &mut indexer).await?;

        assert_eq!(indexer.get_state(), old_wallets);
        let expected = SystemWallets {
            bridge: new_bridge_address,
            verifiers: new_verifiers,
            ..(*old_wallets).clone()
        };
        assert_eq!(stored_wallets(&pool).await?, expected);

        Ok(())
    }

    #[tokio::test]
    async fn test_update_governance_bridge_wallet_and_sequencer() -> anyhow::Result<()> {
        let (pool, mut indexer, mut processor, _core) = setup().await?;

        let new_governance_address = random_bitcoin_wallet().1;
        let gov_msg = create_update_governance_inscription(new_governance_address.clone());

        let new_sequencer_address = random_bitcoin_wallet().1;
        let sequencer_msg = create_update_sequencer_inscription(new_sequencer_address.clone());


        let new_verifier_1 = random_bitcoin_wallet().1;
        let new_verifier_2 = random_bitcoin_wallet().1;
        let new_verifier_3 = random_bitcoin_wallet().1;
        let new_verifier_4 = random_bitcoin_wallet().1;

        let new_verifiers = vec![new_verifier_1, new_verifier_2, new_verifier_3, new_verifier_4];

        let (bridge_msg, new_bridge_address) = bridge_update(&_core, new_verifiers.clone()).await?;

        processor
            .process_messages(&mut pool.connection().await?, vec![bridge_msg, sequencer_msg, gov_msg], &mut indexer)
            .await?;

        assert_eq!(indexer.get_state(), old_wallets);
        let expected = SystemWallets {
            bridge: new_bridge_address,
            sequencer: new_sequencer_address,
            governance: new_governance_address,
            verifiers: new_verifiers,
        };
        assert_eq!(stored_wallets(&pool).await?, expected);

        Ok(())
    }

    async fn bridge_update(
        core: &BitcoinCore,
        verifiers: Vec<BitcoinAddress>,
    ) -> anyhow::Result<(FullInscriptionMessage, BitcoinAddress)> {
        let address = BitcoinAddress::from_str(
            "bcrt1pcx974cg2w66cqhx67zadf85t8k4sd2wp68l8x8agd3aj4tuegsgsz97amg",
        )?
        .assume_checked();
        Ok((create_update_bridge_inscription(core, address.clone(), verifiers).await?, address))
    }

    async fn setup() -> anyhow::Result<(
        ConnectionPool<Core>,
        BitcoinInscriptionIndexer,
        SystemWalletProcessor,
        BitcoinCore,
    )> {
        let core = BitcoinCore::new()?;
        let pool = ConnectionPool::<Core>::test_pool().await;
        let wallets = SystemWalletsDetails::try_from(test_wallets())?;
        pool.connection().await?.via_wallet_dal().insert_wallets(&wallets, 0).await?;
        Ok((
            pool,
            test_create_indexer(&core)?,
            SystemWalletProcessor::new(core.client()?),
            core,
        ))
    }

    async fn stored_wallets(pool: &ConnectionPool<Core>) -> anyhow::Result<SystemWallets> {
        let stored = pool
            .connection()
            .await?
            .via_wallet_dal()
            .get_system_wallets_raw(i64::MAX)
            .await?
            .unwrap();
        SystemWallets::try_from(stored)
    }

    async fn process_role_update(
        pool: &ConnectionPool<Core>,
        indexer: &mut BitcoinInscriptionIndexer,
        processor: &mut SystemWalletProcessor,
        message: FullInscriptionMessage,
    ) -> anyhow::Result<Arc<SystemWallets>> {
        let old = indexer.get_state();
        let changed_height = processor
            .process_messages(&mut pool.connection().await?, vec![message], indexer)
            .await?;
        assert_eq!(changed_height, Some(0));
        assert_eq!(indexer.get_state(), old);
        Ok(old)
    }
}
