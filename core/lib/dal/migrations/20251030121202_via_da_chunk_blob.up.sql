-- 1. Add the new index column with default 0
ALTER TABLE via_data_availability
ADD COLUMN index INTEGER NOT NULL DEFAULT 0;

-- 2. Drop the old primary key constraint
ALTER TABLE via_data_availability
DROP CONSTRAINT via_data_availability_pkey;

-- 3. Add the new primary key including the index column
ALTER TABLE via_data_availability
ADD PRIMARY KEY ("l1_batch_number", "is_proof", "index");
