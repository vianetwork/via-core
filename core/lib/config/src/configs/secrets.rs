use anyhow::Context;
use zksync_basic_types::url::SensitiveUrl;

use crate::configs::consensus::ConsensusSecrets;

#[derive(Debug, Clone, PartialEq)]
pub struct DatabaseSecrets {
    pub server_url: Option<SensitiveUrl>,
    pub prover_url: Option<SensitiveUrl>,
    pub server_replica_url: Option<SensitiveUrl>,
    pub verifier_url: Option<SensitiveUrl>,
    pub indexer_url: Option<SensitiveUrl>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct L1Secrets {
    pub l1_rpc_url: SensitiveUrl,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Secrets {
    pub consensus: Option<ConsensusSecrets>,
    pub database: Option<DatabaseSecrets>,
    pub l1: Option<L1Secrets>,
}

impl DatabaseSecrets {
    /// Returns a copy of the master database URL as a `Result` to simplify error propagation.
    pub fn master_url(&self) -> anyhow::Result<SensitiveUrl> {
        self.server_url.clone().context("Master DB URL is absent")
    }

    /// Returns a copy of the replica database URL as a `Result` to simplify error propagation.
    pub fn replica_url(&self) -> anyhow::Result<SensitiveUrl> {
        if let Some(replica_url) = &self.server_replica_url {
            Ok(replica_url.clone())
        } else {
            self.master_url()
        }
    }

    /// Returns a copy of the prover database URL as a `Result` to simplify error propagation.
    pub fn prover_url(&self) -> anyhow::Result<SensitiveUrl> {
        self.prover_url.clone().context("Prover DB URL is absent")
    }

    /// Returns a copy of the verifier database URL as a `Result` to simplify error propagation.
    pub fn verifier_url(&self) -> anyhow::Result<SensitiveUrl> {
        self.verifier_url
            .clone()
            .context("Verifier DB URL is absent")
    }

    /// Returns a copy of the indexer database URL as a `Result` to simplify error propagation.
    pub fn indexer_url(&self) -> anyhow::Result<SensitiveUrl> {
        self.indexer_url.clone().context("Indexer DB URL is absent")
    }
}
