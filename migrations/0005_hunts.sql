-- Vulnerability hunts are runs with trigger kind "hunt". Their suspected findings, and once
-- probed the harness's verdict on each, are kept here as JSON.
ALTER TABLE runs ADD COLUMN findings TEXT;
