//! Own Via position/payload SQL. Both marked stores share this schema and wallet projection.

use std::collections::HashMap;

use anyhow::{ensure, Context};
use sqlx::{FromRow, Row};
use via_btc_client::indexer::{
    configuration::ConfigurationInput,
    replay::{compare_fields, Included},
};
use zksync_db_connection::{connection::DbMarker, instrument::InstrumentExt};
use zksync_types::via_wallet::{SystemWallets, SystemWalletsDetails};

use crate::{ReaderDal, StoreError};

pub struct StoredConfiguration {
    pub id: i64,
    pub change: Included<ConfigurationInput>,
    pub accepted: Option<bool>,
}

impl<DB: DbMarker> ReaderDal<'_, '_, DB> {
    /// decide_configurations accepts or rejects each undecided change in block h in carrier order, rejecting every role with two distinct occurrences.
    /// Bridge change waits on rotation evidence and protocol change needs higher version, so decision never depends on arrival order.
    /// Role applies returned protocol changes in this same outer transaction.
    /// Idea only from CometBFT processChanges, which rejects duplicate validator updates in one block: https://github.com/cometbft/cometbft/blob/99dfb19d9fb406389269b079d34aba555ee4f3d5/types/validator_set.go
    pub async fn decide_configurations(
        &mut self,
        height: u32,
        proofs: &[crate::CanonicalProof],
        votes: &[crate::CanonicalVote],
        retained_complete: bool,
        network: bitcoin::Network,
        threshold: f64,
    ) -> anyhow::Result<Vec<StoredConfiguration>> {
        use via_reorg::configuration::{conflicting_roles, RotationDecision};
        if self
            .consumer(crate::PROTOCOL_CONSUMER)
            .await?
            .map_or(true, |cursor| cursor.header.height < height)
            || self.wallets_before(height).await?.is_none()
        {
            return Ok(Vec::new());
        }
        let changes = self.configurations_at(height).await?;
        let duplicates = conflicting_roles(changes.iter().map(|change| {
            (
                change.change.payload.input.role(),
                (change.change.origin.transaction_index, change.change.origin.message_index),
            )
        }));
        let mut protocol = Vec::new();
        for change in changes.iter().filter(|change| change.accepted.is_none()) {
            let input = &change.change.payload.input;
            let accepted = if duplicates.contains(input.role()) || !input.valid_proposal() {
                false
            } else if matches!(input, ConfigurationInput::Bridge(..)) {
                match self
                    .rotation_decision(proofs, retained_complete, votes, height, threshold)
                    .await?
                {
                    RotationDecision::NotReady => continue,
                    RotationDecision::Accepted => true,
                    RotationDecision::Rejected => false,
                }
            } else if let ConfigurationInput::Protocol(_, proposals) = input {
                let Some(previous) = self.protocol_before(height).await? else {
                    continue;
                };
                proposals[0].payload.input.version > previous
            } else {
                true
            };
            let wallets = if accepted {
                match via_btc_client::indexer::configuration::project_wallets(
                    input,
                    change.change.origin.txid,
                    network,
                ) {
                    Ok(wallets) => wallets,
                    Err(error) => {
                        tracing::warn!(%error,"Rejecting configuration with an invalid wallet");
                        self.decide_configuration(
                            change.id,
                            false,
                            &SystemWalletsDetails::default(),
                        )
                        .await?;
                        continue;
                    }
                }
            } else {
                SystemWalletsDetails::default()
            };
            self.decide_configuration(change.id, accepted, &wallets).await?;
            if accepted && matches!(input, ConfigurationInput::Protocol(..)) {
                protocol.push(change.id);
            }
        }
        Ok(changes
            .into_iter()
            .filter(|change| protocol.contains(&change.id))
            .map(|mut change| {
                change.accepted = Some(true);
                change
            })
            .collect())
    }

    pub async fn protocol_before(
        &mut self,
        height: u32,
    ) -> anyhow::Result<Option<zksync_types::protocol_version::ProtocolSemanticVersion>> {
        let row = sqlx::query("SELECT p.minor,p.patch FROM protocol_patches p JOIN via_btc_headers h
            ON h.number=p.bitcoin_height AND h.hash=p.bitcoin_hash WHERE h.number<$1 ORDER BY p.minor DESC,p.patch DESC LIMIT 1")
            .bind(i64::from(height)).instrument("bitcoin_historical_protocol").fetch_optional(self.storage).await?;
        row.map(|row| {
            Ok(zksync_types::protocol_version::ProtocolSemanticVersion {
                minor: u16::try_from(row.try_get::<i32, _>("minor")?)?.try_into()?,
                patch: zksync_types::protocol_version::VersionPatch(
                    row.try_get::<i32, _>("patch")?.try_into()?,
                ),
            })
        })
        .transpose()
    }

    /// Native minor fields and upgrade bytes are validated by the role before this common source save.
    pub async fn bind_protocol(&mut self, change: &StoredConfiguration) -> anyhow::Result<()> {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("protocol binding outside transaction")
        );
        let ConfigurationInput::Protocol(_, proposals) = &change.change.payload.input else {
            anyhow::bail!(StoreError::Integrity("non-protocol binding"));
        };
        let [proposal] = proposals.as_slice() else {
            anyhow::bail!(StoreError::Integrity("ambiguous protocol binding"));
        };
        let version = proposal.payload.input.version;
        let saved = sqlx::query_scalar::<_, Option<serde_json::Value>>(
            "SELECT bitcoin_payload FROM protocol_patches WHERE minor=$1 AND patch=$2",
        )
        .bind(version.minor as i32)
        .bind(i32::try_from(version.patch.0)?)
        .instrument("bitcoin_protocol_replay")
        .fetch_one(self.storage)
        .await?;
        if let Some(saved) = saved {
            change.change.payload.compare(&serde_json::from_value(saved)?)?;
        }
        let patch = sqlx::query("UPDATE protocol_patches p SET bitcoin_height=c.l1_block_number,bitcoin_hash=c.block_hash,bitcoin_payload=c.payload
            FROM via_btc_configuration c JOIN via_btc_headers h ON h.number=c.l1_block_number AND h.hash=c.block_hash
            WHERE c.id=$1 AND c.accepted AND p.minor=$2 AND p.patch=$3")
            .bind(change.id).bind(version.minor as i32).bind(i32::try_from(version.patch.0)?)
            .instrument("bitcoin_bind_protocol_patch").execute(self.storage).await?;
        ensure!(patch.rows_affected() == 1, StoreError::HeadChanged);
        Ok(())
    }

    pub async fn configurations_at(
        &mut self,
        height: u32,
    ) -> anyhow::Result<Vec<StoredConfiguration>> {
        type Candidate = (i64, i64, String, i32, i32, String, serde_json::Value, Option<bool>);
        let rows = sqlx::query(
            "SELECT c.id,c.l1_block_number,c.block_hash,c.transaction_index,c.message_index,c.txid,c.payload,c.accepted
             FROM via_btc_configuration c JOIN via_btc_headers h ON h.number=c.l1_block_number AND h.hash=c.block_hash
             WHERE c.l1_block_number=$1 ORDER BY c.transaction_index,c.message_index",
        ).bind(i64::from(height)).try_map(|row: sqlx::postgres::PgRow| Candidate::from_row(&row)).instrument("bitcoin_configuration_at").fetch_all(self.storage).await?;
        rows.into_iter()
            .map(|(id, number, hash, transaction, message, txid, payload, accepted)| {
                let block = crate::block_ref(number, &hash)?;
                Ok(StoredConfiguration {
                    id,
                    accepted,
                    change: Included {
                        origin: via_btc_client::indexer::replay::MessageOrigin {
                            height: block.height,
                            hash: block.block_hash,
                            transaction_index: transaction.try_into().context(
                                StoreError::Integrity("configuration transaction index"),
                            )?,
                            message_index: message
                                .try_into()
                                .context(StoreError::Integrity("configuration message index"))?,
                            txid: txid
                                .parse()
                                .context(StoreError::Integrity("configuration txid"))?,
                        },
                        payload: serde_json::from_value(payload)
                            .context(StoreError::Integrity("configuration payload"))?,
                    },
                })
            })
            .collect()
    }
    /// wallets_before returns wallet set in force at height from latest accepted change below it per role, or None while any earlier change is undecided.
    /// Change in block h therefore applies from h+1, and legacy rows count only at bootstrap height.
    /// Idea only from cw4-group SnapshotMap, which loads membership as of past height: https://github.com/CosmWasm/cw-plus/blob/2be5732a555e98d4f7ba759d9f1cd5b8bf126972/contracts/cw4-group/src/state.rs
    pub async fn wallets_before(&mut self, height: u32) -> anyhow::Result<Option<SystemWallets>> {
        let pending = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM via_btc_configuration WHERE l1_block_number<$1 AND accepted IS NULL)",
        ).bind(i64::from(height)).instrument("bitcoin_configuration_prefix")
            .fetch_one(self.storage).await?;
        if pending {
            return Ok(None);
        }
        let rows = sqlx::query(
            "SELECT DISTINCT ON(w.role) w.role,w.address FROM via_wallets w CROSS JOIN via_btc_reader r
             WHERE w.l1_block_number<$1 AND ((w.block_hash IS NULL AND w.l1_block_number=r.bootstrap_number)
                OR EXISTS(SELECT 1 FROM via_btc_headers h WHERE h.number=w.l1_block_number AND h.hash=w.block_hash))
             ORDER BY w.role,w.l1_block_number DESC,w.transaction_index DESC NULLS LAST,w.message_index DESC NULLS LAST",
        ).bind(i64::from(height)).try_map(|row: sqlx::postgres::PgRow| <(String, String)>::from_row(&row)).instrument("bitcoin_historical_wallets").fetch_all(self.storage).await?;
        if rows.is_empty() {
            return Ok(None);
        }
        let wallets: HashMap<_, _> = rows.into_iter().collect();
        Ok(Some(SystemWallets::try_from(wallets)?))
    }

    /// save_configuration inserts occurrence once and on replay compares height, txid and payload against stored row.
    /// Changed content under same position is conflict, never overwrite.
    /// Idea only from TigerBeetle create_transfer_exists, which names first differing field on duplicate id: https://github.com/tigerbeetle/tigerbeetle/blob/0450c32b2c3c3ab6114f3402cc601341cc8192aa/src/state_machine.zig
    pub async fn save_configuration(
        &mut self,
        change: &Included<ConfigurationInput>,
    ) -> anyhow::Result<i64> {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("configuration outside transaction")
        );
        let origin = &change.origin;
        let id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO via_btc_configuration(l1_block_number,block_hash,transaction_index,message_index,txid,role,payload)
             SELECT $1,$2,$3,$4,$5,$6,$7 WHERE EXISTS(SELECT 1 FROM via_btc_headers WHERE number=$1 AND hash=$2)
             ON CONFLICT DO NOTHING RETURNING id",
        ).bind(i64::from(origin.height)).bind(origin.hash.to_string()).bind(i32::try_from(origin.transaction_index)?)
            .bind(i32::try_from(origin.message_index)?).bind(origin.txid.to_string()).bind(change.payload.input.role())
            .bind(serde_json::to_value(&change.payload)?).instrument("bitcoin_configuration_save").fetch_optional(self.storage).await?;
        if let Some(id) = id {
            return Ok(id);
        }
        let saved = sqlx::query(
            "SELECT id,l1_block_number,txid,payload FROM via_btc_configuration WHERE block_hash=$1 AND transaction_index=$2 AND message_index=$3 AND role=$4",
        ).bind(origin.hash.to_string()).bind(i32::try_from(origin.transaction_index)?)
            .bind(i32::try_from(origin.message_index)?).bind(change.payload.input.role())
            .instrument("bitcoin_configuration_replay").fetch_optional(self.storage).await?.context(StoreError::HeadChanged)?;
        compare_fields([
            ("height", saved.try_get::<i64, _>("l1_block_number")? == i64::from(origin.height)),
            ("txid", saved.try_get::<String, _>("txid")? == origin.txid.to_string()),
        ])?;
        change.payload.compare(&serde_json::from_value(saved.try_get("payload")?)?)?;
        Ok(saved.try_get("id")?)
    }

    pub async fn decide_configuration(
        &mut self,
        id: i64,
        accepted: bool,
        wallets: &SystemWalletsDetails,
    ) -> anyhow::Result<()> {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("configuration decision outside transaction")
        );
        let changed = sqlx::query(
            "UPDATE via_btc_configuration c SET accepted=$2 WHERE c.id=$1 AND (accepted IS NULL OR accepted=$2)
             AND EXISTS(SELECT 1 FROM via_btc_headers h WHERE h.number=c.l1_block_number AND h.hash=c.block_hash)",
        ).bind(id).bind(accepted).instrument("bitcoin_configuration_decision").execute(self.storage).await?;
        ensure!(changed.rows_affected() == 1, StoreError::HeadChanged);
        if accepted {
            self.insert_wallets(wallets, WalletSource::Configuration(id)).await?;
        }
        Ok(())
    }

    pub async fn insert_wallets(
        &mut self,
        wallets: &SystemWalletsDetails,
        source: WalletSource,
    ) -> anyhow::Result<()> {
        ensure!(
            self.storage.in_transaction(),
            StoreError::Integrity("wallet write outside transaction")
        );
        let (height, hash, transaction_index, message_index) = match source {
            WalletSource::Unbound(height) => (height, None, None, None),
            WalletSource::Configuration(id) => {
                let row = sqlx::query(
                    "SELECT c.l1_block_number,c.block_hash,c.transaction_index,c.message_index FROM via_btc_configuration c
                     JOIN via_btc_headers h ON h.number=c.l1_block_number AND h.hash=c.block_hash WHERE c.id=$1 AND c.accepted",
                ).bind(id).instrument("bitcoin_wallet_source").fetch_optional(self.storage).await?.context(StoreError::HeadChanged)?;
                (
                    row.try_get("l1_block_number")?,
                    Some(row.try_get::<String, _>("block_hash")?),
                    Some(row.try_get::<i32, _>("transaction_index")?),
                    Some(row.try_get::<i32, _>("message_index")?),
                )
            }
        };
        for (role, info) in &wallets.0 {
            let addresses =
                info.addresses.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
            sqlx::query(
                "INSERT INTO via_wallets(role,address,tx_hash,l1_block_number,block_hash,transaction_index,message_index)
                 VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING",
            ).bind(role.to_string()).bind(addresses).bind(info.txid.to_string()).bind(height).bind(hash.as_deref())
                .bind(transaction_index).bind(message_index).instrument("bitcoin_wallets_save").execute(self.storage).await?;
        }
        Ok(())
    }
}

pub enum WalletSource {
    Unbound(i64),
    Configuration(i64),
}
