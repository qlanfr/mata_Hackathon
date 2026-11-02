-- Only one aiopsd instance may advance runs at a time. During a deploy rollout Cloudflare starts
-- the new aiopsd container while the old one is still shutting down; without a lease both tick
-- the same runs, and one can fail a run the other is in the middle of advancing.
CREATE TABLE leases (
    name       TEXT    PRIMARY KEY,
    holder     TEXT    NOT NULL,  -- instance id of the aiopsd process holding the lease
    expires_at INTEGER NOT NULL   -- epoch ms; anyone may take the lease after this
);
