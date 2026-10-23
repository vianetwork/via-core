#[cfg(test)]
mod tests {
    // jscpd:ignore-start Separate wallet processor and DAL schema per node.
    use std::str::FromStr;

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
    use via_verifier_dal::{ConnectionPool, Verifier, VerifierDal};
    use zksync_types::via_wallet::{SystemWallets, SystemWalletsDetails};

    use crate::{message_processors::SystemWalletProcessor, MessageProcessor};

    #[tokio::test]
    async fn identical_duplicate_role_updates_leave_unrelated_role_active() -> anyhow::Result<()> {
        let (pool, mut indexer, mut processor, _core) = setup().await?;
        let original = indexer.get_state();
        let sequencer = random_bitcoin_wallet().1;
        let governance = random_bitcoin_wallet().1;
        let message = create_update_sequencer_inscription(sequencer);
        assert_eq!(
            apply_prepared(
                &pool,
                &mut processor,
                &mut indexer,
                vec![
                    message.clone(),
                    message,
                    create_update_governance_inscription(governance.clone())
                ]
            )
            .await?,
            Some(1)
        );
        assert_eq!(
            stored_wallets(&pool).await?,
            SystemWallets { governance, ..(*original).clone() }
        );
        let mut storage = pool.connection().await?;
        let decisions:Vec<String>=sqlx::query_scalar("SELECT i.decision FROM via_wallet_inclusions i JOIN via_wallets w ON w.id=i.wallet_id WHERE w.role='Sequencer' ORDER BY i.tx_index")
            .fetch_all(storage.conn()).await?;
        assert_eq!(decisions, vec!["duplicate_role", "duplicate_role"]);
        assert_eq!(
            SystemWallets::try_from(
                storage.via_wallet_dal().get_system_wallets_raw(0).await?.unwrap()
            )?,
            *original
        );
        Ok(())
    }

    #[tokio::test]
    async fn malformed_update_retains_raw_evidence_without_erasing_another_role(
    ) -> anyhow::Result<()> {
        for empty_bridge_proposal in [false, true] {
            let (pool, mut indexer, mut processor, core) = setup().await?;
            let original = indexer.get_state();
            let (invalid, roles) = if empty_bridge_proposal {
                (bridge_update(&core, vec![]).await?.0, vec!["Bridge", "Verifier"])
            } else {
                let foreign = BitcoinAddress::from_script(
                    &random_bitcoin_wallet().1.script_pubkey(),
                    bitcoin::Network::Bitcoin,
                )?;
                (create_update_governance_inscription(foreign), vec!["Gov"])
            };
            let payload = via_btc_client::indexer::replay::semantic_payload(&invalid)?;
            let sequencer = random_bitcoin_wallet().1;
            assert_eq!(
                apply_prepared(
                    &pool,
                    &mut processor,
                    &mut indexer,
                    vec![invalid, create_update_sequencer_inscription(sequencer.clone())]
                )
                .await?,
                Some(1)
            );
            assert_eq!(
                stored_wallets(&pool).await?,
                SystemWallets { sequencer, ..(*original).clone() }
            );
            let mut storage = pool.connection().await?;
            let rejected:Vec<(String,serde_json::Value)>=sqlx::query_as("SELECT i.decision,w.immutable_payload FROM via_wallet_inclusions i JOIN via_wallets w ON w.id=i.wallet_id WHERE w.role=ANY($1) ORDER BY w.role")
                .bind(&roles).fetch_all(storage.conn()).await?;
            assert_eq!(rejected.len(), roles.len());
            for (decision, retained) in rejected {
                assert_eq!(decision, "bad_update");
                for (field, value) in payload.as_object().unwrap() {
                    assert_eq!(&retained["message"][field], value);
                }
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn unavailable_proposal_is_not_ready_and_never_becomes_a_permanent_rejection(
    ) -> anyhow::Result<()> {
        use bitcoin::{hashes::Hash, Txid};
        let (pool, mut indexer, mut processor, core) = setup().await?;
        let original = indexer.get_state();
        let (mut update, _) = bridge_update(&core, original.verifiers.clone()).await?;
        let FullInscriptionMessage::UpdateBridge(message) = &mut update else { unreachable!() };
        message.input.proposal_tx_id = Txid::all_zeros();
        let error =
            apply_prepared(&pool, &mut processor, &mut indexer, vec![update]).await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::message_processors::MessageProcessorError>(),
            Some(crate::message_processors::MessageProcessorError::ConfigurationNotReady { .. })
        ));
        assert_eq!(stored_wallets(&pool).await?, *original);
        let mut storage = pool.connection().await?;
        let rejected: i64 = sqlx::query_scalar("SELECT count(*) FROM via_wallet_inclusions")
            .fetch_one(storage.conn())
            .await?;
        assert_eq!(rejected, 0);
        Ok(())
    }

    #[tokio::test]
    async fn all_roles_activate_together_with_larger_or_smaller_verifier_sets() -> anyhow::Result<()>
    {
        for verifier_count in [2, 4] {
            let (pool, mut indexer, mut processor, core) = setup().await?;
            let old = indexer.get_state();
            let governance = random_bitcoin_wallet().1;
            let sequencer = random_bitcoin_wallet().1;
            let verifiers =
                (0..verifier_count).map(|_| random_bitcoin_wallet().1).collect::<Vec<_>>();
            let (bridge_message, bridge) = bridge_update(&core, verifiers.clone()).await?;
            assert_eq!(
                apply_prepared(
                    &pool,
                    &mut processor,
                    &mut indexer,
                    vec![
                        bridge_message,
                        create_update_sequencer_inscription(sequencer.clone()),
                        create_update_governance_inscription(governance.clone())
                    ]
                )
                .await?,
                Some(1)
            );
            assert_eq!(indexer.get_state(), old);
            assert_eq!(
                stored_wallets(&pool).await?,
                SystemWallets { bridge, sequencer, governance, verifiers }
            );
            let mut storage = pool.connection().await?;
            assert_eq!(
                SystemWallets::try_from(
                    storage.via_wallet_dal().get_system_wallets_raw(0).await?.unwrap()
                )?,
                *old
            );
        }
        Ok(())
    }

    // Processor-unit inputs are already parsed; the shared fixture supplies their explicit source preconditions.
    async fn apply_prepared(
        pool: &ConnectionPool<Verifier>,
        processor: &mut SystemWalletProcessor,
        indexer: &mut BitcoinInscriptionIndexer,
        mut messages: Vec<FullInscriptionMessage>,
    ) -> anyhow::Result<Option<u32>> {
        use bitcoin::{hashes::Hash, BlockHash, Txid};
        let mut storage = pool.connection().await?;
        let mut tx = storage.start_transaction().await?;
        tx.via_indexer_dal().lock_source_writer().await?;
        let height: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(height),0)+1 FROM via_btc_headers")
                .fetch_one(tx.conn())
                .await?;
        let header = via_reorg::chain::Header {
            height: u32::try_from(height)?,
            block_hash: BlockHash::from_byte_array([height as u8; 32]),
            parent_hash: BlockHash::all_zeros(),
        };
        via_test_utils::reader_db::header(tx.conn(), header, true).await?;
        for (index, message) in messages.iter_mut().enumerate() {
            let common = message.common_mut();
            common.block_height = header.height;
            common.block_hash = Some(header.block_hash);
            common.tx_index = Some(index);
            common.message_index = Some(0);
            common.tx_id = Txid::from_byte_array([height as u8 + index as u8; 32]);
            common.carrier_tx_id = Some(common.tx_id);
        }
        let changed = processor.process_messages(&mut tx, messages, indexer).await?;
        tx.commit().await?;
        Ok(changed)
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
        ConnectionPool<Verifier>,
        BitcoinInscriptionIndexer,
        SystemWalletProcessor,
        BitcoinCore,
    )> {
        let core = BitcoinCore::new()?;
        let pool = ConnectionPool::<Verifier>::test_pool().await;
        let wallets = SystemWalletsDetails::try_from(test_wallets())?;
        pool.connection().await?.via_wallet_dal().insert_bootstrap_wallets(&wallets, 0).await?;
        Ok((
            pool,
            test_create_indexer(&core)?,
            SystemWalletProcessor::new(core.client()?),
            core,
        ))
    }

    async fn stored_wallets(pool: &ConnectionPool<Verifier>) -> anyhow::Result<SystemWallets> {
        let stored = pool
            .connection()
            .await?
            .via_wallet_dal()
            .get_system_wallets_raw(i64::MAX)
            .await?
            .unwrap();
        SystemWallets::try_from(stored)
    }
    // jscpd:ignore-end
}
