#[cfg(test)]
mod tests {
    use std::{i64, str::FromStr, sync::Arc};

    use via_btc_client::types::BitcoinAddress;
    use via_test_utils::utils::{
        create_update_bridge_inscription, create_update_governance_inscription,
        create_update_sequencer_inscription, random_bitcoin_wallet, test_bitcoin_client,
        test_bitcoin_ops_serving_transaction, test_create_indexer,
        test_system_contract_upgrade_activation, test_system_contract_upgrade_proposal,
        test_system_contract_upgrade_proposal_input,
        test_system_contract_upgrade_proposal_transaction, test_wallets,
    };
    use via_verifier_dal::{Connection, ConnectionPool, Verifier, VerifierDal};
    use zksync_types::{
        protocol_version::ProtocolSemanticVersion,
        via_protocol_upgrade::ViaProtocolUpgrade,
        via_wallet::{SystemWallets, SystemWalletsDetails},
    };

    use crate::{
        message_processors::{GovernanceUpgradesEventProcessor, SystemWalletProcessor},
        MessageProcessor,
    };

    #[tokio::test]
    async fn test_update_sequencer_wallet() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer();

        let system_wallet_map = SystemWalletsDetails::try_from(test_wallets())?;

        pool.connection()
            .await?
            .via_wallet_dal()
            .insert_wallets(&system_wallet_map, 0)
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
            .get_system_wallets_raw(i64::MAX)
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
            .insert_wallets(&system_wallet_map, 0)
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
            .get_system_wallets_raw(i64::MAX)
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
            .insert_wallets(&system_wallet_map, 0)
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
            .get_system_wallets_raw(i64::MAX)
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
            .insert_wallets(&system_wallet_map, 0)
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
            .get_system_wallets_raw(i64::MAX)
            .await?
            .unwrap();

        let system_wallets_db = Arc::new(SystemWallets::try_from(system_wallets_db_map.clone())?);

        assert_eq!(system_wallets_db, new_wallets);

        Ok(())
    }

    #[tokio::test]
    async fn test_system_contract_upgrade_hashes_all_pairs() -> anyhow::Result<()> {
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let mut indexer = test_create_indexer();

        let proposal_input = test_system_contract_upgrade_proposal_input();
        let expected_upgrade_hash = ViaProtocolUpgrade::default().get_canonical_tx_hash(
            proposal_input.version,
            proposal_input.system_contracts.clone(),
        )?;
        let proposal_tx =
            test_system_contract_upgrade_proposal_transaction(&proposal_input).await?;
        let proposal_tx_id = proposal_tx.compute_txid();
        let btc_client = test_bitcoin_ops_serving_transaction(proposal_tx);
        let mut upgrade_processor = GovernanceUpgradesEventProcessor::new(btc_client);
        let mut storage = pool.connection().await?;

        upgrade_processor
            .process_messages(
                &mut storage,
                vec![test_system_contract_upgrade_proposal(
                    proposal_input.clone(),
                )],
                &mut indexer,
            )
            .await?;
        assert_eq!(
            protocol_patch_count(&mut storage, proposal_input.version).await?,
            0
        );

        let activation = test_system_contract_upgrade_activation(proposal_tx_id);
        upgrade_processor
            .process_messages(
                &mut storage,
                vec![activation.clone(), activation],
                &mut indexer,
            )
            .await?;
        assert_eq!(
            protocol_patch_count(&mut storage, proposal_input.version).await?,
            1
        );
        let stored_upgrade_hash = storage
            .via_protocol_versions_dal()
            .get_protocol_upgrade_tx(proposal_input.version.minor)
            .await?
            .expect("activated upgrade hash must be stored");
        assert_eq!(stored_upgrade_hash, expected_upgrade_hash);
        Ok(())
    }

    async fn protocol_patch_count(
        storage: &mut Connection<'_, Verifier>,
        version: ProtocolSemanticVersion,
    ) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM protocol_patches WHERE minor = $1 AND patch = $2",
        )
        .bind(version.minor as i32)
        .bind(version.patch.0 as i32)
        .fetch_one(storage.conn())
        .await?)
    }
}
