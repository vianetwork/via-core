CREATE TABLE via_wallets (
    id BIGSERIAL PRIMARY KEY,
    role VARCHAR NOT NULL,
    address VARCHAR NOT NULL,
    tx_hash VARCHAR NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT unique_tx_hash_address_role UNIQUE (tx_hash, address, role)
);

CREATE INDEX idx_via_wallets_role_created_at ON via_wallets(role, created_at DESC);
