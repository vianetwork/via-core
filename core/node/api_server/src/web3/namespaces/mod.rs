//! Actual implementation of Web3 API namespaces logic, not tied to the backend
//! used to create a JSON RPC server.

mod debug;
mod en;
pub(crate) mod eth;
mod net;
mod snapshots;
mod unstable;
mod via;
mod via_zks;
mod web3;
mod zks;

pub(super) use self::{
    debug::DebugNamespace, en::EnNamespace, eth::EthNamespace, net::NetNamespace,
    snapshots::SnapshotsNamespace, unstable::UnstableNamespace, via::ViaNamespace,
    via_zks::ZksNamespace, web3::Web3Namespace,
};
