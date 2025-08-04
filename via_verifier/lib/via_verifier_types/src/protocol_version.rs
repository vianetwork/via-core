use zksync_types::{
    protocol_version::{ProtocolSemanticVersion, VersionPatch},
    ProtocolVersionId,
};

const SEQUENCER_MINOR: ProtocolVersionId = ProtocolVersionId::Version27;
const SEQUENCER_PATCH: u32 = 0;

/// Get the supported sequencer version by the verifier.
pub fn get_sequencer_version() -> ProtocolSemanticVersion {
    ProtocolSemanticVersion {
        minor: SEQUENCER_MINOR,
        patch: VersionPatch(SEQUENCER_PATCH),
    }
}

/// TODO: Once the protocol stabilizes, update the logic to check only for changes in the minor version,
/// preventing node upgrades for patch-level changes.
pub fn check_if_supported_sequencer_version(
    last_protocol_version: ProtocolSemanticVersion,
) -> anyhow::Result<()> {
    let supported_sequencer_version = get_sequencer_version();

    if supported_sequencer_version < last_protocol_version {
        anyhow::bail!(
            "Verifier node version must be upgraded from {} to {}",
            supported_sequencer_version.to_string(),
            last_protocol_version.to_string()
        );
    }
    Ok(())
}
