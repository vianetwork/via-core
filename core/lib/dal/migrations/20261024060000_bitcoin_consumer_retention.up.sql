ALTER TABLE via_btc_consumers ADD COLUMN oldest_needed_id BIGINT REFERENCES via_btc_headers(id);
UPDATE via_btc_consumers SET oldest_needed_id=(SELECT id FROM via_btc_headers ORDER BY number LIMIT 1);
ALTER TABLE via_btc_consumers ALTER COLUMN oldest_needed_id SET NOT NULL;
