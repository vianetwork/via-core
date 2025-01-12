use std::time::Duration;

use zksync_db_connection::connection_pool::ConnectionPool;
use zksync_types::{
    block::{L1BatchHeader, L2BlockHasher, L2BlockHeader},
    fee::Fee,
    fee_model::BatchFeeInput,
    helpers::unix_timestamp_ms,
    l1::{L1Tx, OpProcessingType, PriorityQueueType},
    l2::L2Tx,
    l2_to_l1_log::{L2ToL1Log, UserL2ToL1Log},
    protocol_upgrade::{ProtocolUpgradeTx, ProtocolUpgradeTxCommonData},
    snapshots::SnapshotRecoveryStatus,
    Address, Execute, K256PrivateKey, L1BatchNumber, L1BlockNumber, L1TxCommonData, L2BlockNumber,
    L2ChainId, PriorityOpId, ProtocolVersion, ProtocolVersionId, H160, H256, U256,
};

use crate::Verifier;

// TODO: Add tests here
