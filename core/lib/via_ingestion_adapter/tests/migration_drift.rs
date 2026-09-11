struct MigrationCopies {
    file: &'static str,
    core: &'static str,
    verifier: &'static str,
    indexer: &'static str,
}

const MIGRATIONS: [MigrationCopies; 4] = [
    MigrationCopies {
        file: "20260711100000_via_ingestion_kernel.up.sql",
        core: include_str!("../../dal/migrations/20260711100000_via_ingestion_kernel.up.sql"),
        verifier: include_str!(
            "../../../../via_verifier/lib/verifier_dal/migrations/20260711100000_via_ingestion_kernel.up.sql"
        ),
        indexer: include_str!(
            "../../../../via_indexer/lib/via_indexer_dal/migrations/20260711100000_via_ingestion_kernel.up.sql"
        ),
    },
    MigrationCopies {
        file: "20260711100000_via_ingestion_kernel.down.sql",
        core: include_str!("../../dal/migrations/20260711100000_via_ingestion_kernel.down.sql"),
        verifier: include_str!(
            "../../../../via_verifier/lib/verifier_dal/migrations/20260711100000_via_ingestion_kernel.down.sql"
        ),
        indexer: include_str!(
            "../../../../via_indexer/lib/via_indexer_dal/migrations/20260711100000_via_ingestion_kernel.down.sql"
        ),
    },
    MigrationCopies {
        file: "20260711120000_via_ingestion_lock.up.sql",
        core: include_str!("../../dal/migrations/20260711120000_via_ingestion_lock.up.sql"),
        verifier: include_str!(
            "../../../../via_verifier/lib/verifier_dal/migrations/20260711120000_via_ingestion_lock.up.sql"
        ),
        indexer: include_str!(
            "../../../../via_indexer/lib/via_indexer_dal/migrations/20260711120000_via_ingestion_lock.up.sql"
        ),
    },
    MigrationCopies {
        file: "20260711120000_via_ingestion_lock.down.sql",
        core: include_str!("../../dal/migrations/20260711120000_via_ingestion_lock.down.sql"),
        verifier: include_str!(
            "../../../../via_verifier/lib/verifier_dal/migrations/20260711120000_via_ingestion_lock.down.sql"
        ),
        indexer: include_str!(
            "../../../../via_indexer/lib/via_indexer_dal/migrations/20260711120000_via_ingestion_lock.down.sql"
        ),
    },
];

#[test]
fn via_ingestion_migrations_are_identical_across_families() {
    for migration in MIGRATIONS {
        assert_eq!(
            migration.core, migration.verifier,
            "migration drift between core/lib/dal/migrations/{} and via_verifier/lib/verifier_dal/migrations/{}",
            migration.file, migration.file
        );
        assert_eq!(
            migration.core, migration.indexer,
            "migration drift between core/lib/dal/migrations/{} and via_indexer/lib/via_indexer_dal/migrations/{}",
            migration.file, migration.file
        );
    }
}
