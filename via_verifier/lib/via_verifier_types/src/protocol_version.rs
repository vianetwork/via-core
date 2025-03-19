use zksync_types::{
    protocol_version::{ProtocolSemanticVersion, VersionPatch},
    ProtocolVersionId,
};

const SEQUENCER_MINOR: ProtocolVersionId = ProtocolVersionId::Version26;
const SEQUENCER_PATCH: u32 = 0;

/// Bootloader code hash
const BOOTLOADER_CODE_HASH: &[u8] =
    &"010008e742608b21bf7eb23c1a9d0602047e3618b464c9b59c0fba3b3d7ab66e".as_bytes();

/// default aa code hash
const DEFAULT_AA_CODE_HASH: &[u8] =
    &"01000563374c277a2c1e34659a2a1e87371bb6d852ce142022d497bfb50b9e32".as_bytes();

/// Get the supported sequencer version by the verifier.
pub fn get_sequencer_version() -> ProtocolSemanticVersion {
    ProtocolSemanticVersion {
        minor: SEQUENCER_MINOR,
        patch: VersionPatch(SEQUENCER_PATCH),
    }
}

/// Get the supported sequencer base system contracts.
pub fn get_sequencer_base_system_contracts() -> (&'static [u8], &'static [u8]) {
    (BOOTLOADER_CODE_HASH, DEFAULT_AA_CODE_HASH)
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
