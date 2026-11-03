-- 24/7 operation.
-- A runner job can wait before it is submitted: when the machine is too noisy to measure on,
-- the same measurement is scheduled again later instead of the run failing.
ALTER TABLE jobs ADD COLUMN not_before INTEGER;
-- How many times a run has waited for a quieter machine; bounded by AIOPS_NOISE_RETRIES.
ALTER TABLE runs ADD COLUMN env_retries INTEGER NOT NULL DEFAULT 0;
