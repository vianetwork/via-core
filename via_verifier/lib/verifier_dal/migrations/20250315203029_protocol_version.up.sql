CREATE TABLE IF NOT EXISTS protocol_versions (
    id INT PRIMARY KEY,
    bootloader_code_hash BYTEA NOT NULL,
    default_account_code_hash BYTEA NOT NULL,
    upgrade_tx_hash BYTEA UNIQUE NOT NULL,
    recursion_scheduler_level_vk_hash BYTEA UNIQUE NOT NULL,
    executed BOOLEAN NOT NULL,
    created_at TIMESTAMP NOT NULL
);

CREATE TABLE protocol_patches (
    minor INTEGER NOT NULL REFERENCES protocol_versions(id),
    patch INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL,
    PRIMARY KEY (minor, patch)
);
