CREATE TABLE "via_btc_inscriptions_request" (
  "id" BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  "request_type" varchar NOT NULL,
  "inscription_message" BYTEA,
  "predicted_fee" bigint,
  "confirmed_inscriptions_request_history_id" bigint UNIQUE,
  "created_at" timestamp NOT NULL DEFAULT 'now()',
  "updated_at" timestamp NOT NULL
);

CREATE TABLE "via_btc_inscriptions_request_history" (
  "id" BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  "commit_tx_id" varchar UNIQUE NOT NULL,
  "reveal_tx_id" varchar UNIQUE NOT NULL,
  "inscription_request_id" bigint NOT NULL,
  "signed_commit_tx" BYTEA NOT NULL,
  "signed_reveal_tx" BYTEA NOT NULL,
  "actual_fees" bigint NOT NULL,
  "confirmed_at" timestamp DEFAULT null,
  "sent_at_block" bigint NOT NULL,
  "created_at" timestamp DEFAULT 'now()',
  "updated_at" timestamp NOT NULL
);

CREATE TABLE "via_l1_batch_inscription_request" (
  "l1_batch_number" bigint UNIQUE NOT NULL,
  "commit_l1_batch_inscription_id" bigint UNIQUE NOT NULL,
  "commit_proof_inscription_id" bigint UNIQUE,
  "created_at" timestamp NOT NULL DEFAULT 'now()',
  "updated_at" timestamp NOT NULL
);

ALTER TABLE "via_btc_inscriptions_request_history" ADD FOREIGN KEY ("inscription_request_id") REFERENCES "via_btc_inscriptions_request" ("id") ON DELETE CASCADE ON UPDATE NO ACTION;
ALTER TABLE "via_btc_inscriptions_request" ADD FOREIGN KEY ("confirmed_inscriptions_request_history_id") REFERENCES "via_btc_inscriptions_request_history" ("id");
ALTER TABLE "via_l1_batch_inscription_request" ADD FOREIGN KEY ("l1_batch_number") REFERENCES "l1_batches" ("number") ON DELETE CASCADE ON UPDATE NO ACTION;
ALTER TABLE "via_l1_batch_inscription_request" ADD FOREIGN KEY ("commit_l1_batch_inscription_id") REFERENCES "via_btc_inscriptions_request" ("id");
ALTER TABLE "via_l1_batch_inscription_request" ADD FOREIGN KEY ("commit_proof_inscription_id") REFERENCES "via_btc_inscriptions_request" ("id");