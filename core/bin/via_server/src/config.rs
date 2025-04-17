use anyhow::Context as _;
use zksync_config::configs::consensus::ConsensusSecrets;
use zksync_core_leftovers::temp_config_store::decode_yaml_repr;
use zksync_protobuf_config::proto;

pub(crate) fn read_consensus_secrets() -> anyhow::Result<Option<ConsensusSecrets>> {
    // Read public config.
    let Ok(path) = std::env::var("CONSENSUS_SECRETS_PATH") else {
        return Ok(None);
    };
    let secrets = std::fs::read_to_string(&path).context(path)?;
    Ok(Some(
        decode_yaml_repr::<proto::secrets::ConsensusSecrets>(&secrets)
            .context("failed decoding YAML")?,
    ))
}
