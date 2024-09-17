CREATE TABLE IF NOT EXISTS via_data_availability
(
    l1_batch_number BIGINT NOT NULL REFERENCES l1_batches (number) ON DELETE CASCADE,
    is_proof        BOOLEAN NOT NULL,

    blob_id         TEXT      NOT NULL, -- blob here is an abstract term, unrelated to any DA implementation
    inclusion_data  BYTEA,
    sent_at         TIMESTAMP NOT NULL,

    created_at      TIMESTAMP NOT NULL,
    updated_at      TIMESTAMP NOT NULL,

    PRIMARY KEY (l1_batch_number, is_proof) -- for ensuring uniqueness of the combination of l1_batch_number and is_proof
);