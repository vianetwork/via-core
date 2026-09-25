use zksync_object_store::{ObjectStore, ObjectStoreError, StoredObject};
use zksync_types::{protocol_version::ProtocolSemanticVersion, L1BatchNumber};

/// Finds a wrapped proof stored under any allowed protocol version.
/// `Ok(None)` means no allowed key holds the proof yet.
/// Other store errors are returned so each caller can classify them.
pub async fn find_wrapped_proof<P>(
    blob_store: &dyn ObjectStore,
    l1_batch_number: L1BatchNumber,
    allowed_versions: &[ProtocolSemanticVersion],
) -> Result<Option<P>, ObjectStoreError>
where
    P: for<'a> StoredObject<Key<'a> = (L1BatchNumber, ProtocolSemanticVersion)>,
{
    for version in allowed_versions {
        match blob_store.get::<P>((l1_batch_number, *version)).await {
            Ok(proof) => return Ok(Some(proof)),
            Err(ObjectStoreError::KeyNotFound(_)) => continue,
            Err(err) => return Err(err),
        }
    }

    // Proofs written before versioned keys existed use this name for patch 0.
    if allowed_versions.iter().any(|version| version.patch.0 == 0) {
        let deprecated_key = format!("l1_batch_proof_{l1_batch_number}.bin");
        match blob_store.get_by_encoded_key::<P>(deprecated_key).await {
            Ok(proof) => return Ok(Some(proof)),
            Err(ObjectStoreError::KeyNotFound(_)) => {}
            Err(err) => return Err(err),
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use zksync_object_store::{serialize_using_bincode, Bucket, MockObjectStore};
    use zksync_types::{protocol_version::VersionPatch, ProtocolVersionId};

    use super::*;

    /// A stored proof that names the key it was written under.
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Proof(String);

    impl StoredObject for Proof {
        const BUCKET: Bucket = Bucket::ProofsFri;
        type Key<'a> = (L1BatchNumber, ProtocolSemanticVersion);

        fn encode_key((l1_batch_number, version): Self::Key<'_>) -> String {
            let semver_suffix = version.to_string().replace('.', "_");
            format!("l1_batch_proof_{l1_batch_number}_{semver_suffix}.bin")
        }

        serialize_using_bincode!();
    }

    const BATCH: L1BatchNumber = L1BatchNumber(7);

    fn patch(patch: u32) -> ProtocolSemanticVersion {
        ProtocolSemanticVersion::new(ProtocolVersionId::Version28, VersionPatch(patch))
    }

    #[tokio::test]
    async fn the_first_allowed_version_holding_a_proof_wins() {
        let store = MockObjectStore::arc();
        for p in [0, 1] {
            store
                .put((BATCH, patch(p)), &Proof(format!("patch {p}")))
                .await
                .unwrap();
        }
        let found =
            find_wrapped_proof::<Proof>(&*store, BATCH, &[patch(2), patch(1), patch(0)]).await;
        assert_eq!(found.unwrap(), Some(Proof("patch 1".into())));
    }

    #[tokio::test]
    async fn the_legacy_key_is_read_only_when_patch_zero_is_allowed() {
        let store = MockObjectStore::arc();
        let legacy = bincode::serialize(&Proof("legacy".into())).unwrap();
        let key = format!("l1_batch_proof_{BATCH}.bin");
        store
            .put_raw(Bucket::ProofsFri, &key, legacy)
            .await
            .unwrap();

        let found = find_wrapped_proof::<Proof>(&*store, BATCH, &[patch(1)]).await;
        assert_eq!(found.unwrap(), None);
        let found = find_wrapped_proof::<Proof>(&*store, BATCH, &[patch(1), patch(0)]).await;
        assert_eq!(found.unwrap(), Some(Proof("legacy".into())));
    }
}
