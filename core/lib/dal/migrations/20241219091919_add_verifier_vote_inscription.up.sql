CREATE TABLE "via_l1_batch_vote_inscription_request" (
  "l1_batch_number" bigint UNIQUE NOT NULL,
  "vote_l1_batch_inscription_id" bigint UNIQUE NOT NULL,
  "created_at" timestamp NOT NULL DEFAULT 'now()',
  "updated_at" timestamp NOT NULL
);

ALTER TABLE "via_l1_batch_vote_inscription_request" ADD FOREIGN KEY ("vote_l1_batch_inscription_id") REFERENCES "via_btc_inscriptions_request" ("id");
