-- Control database (Cloudflare D1 in production, SQLite locally). Same SQL in both.
-- Timestamps are epoch milliseconds. JSON columns hold TEXT.

CREATE TABLE runs (
    id               TEXT    PRIMARY KEY,
    fingerprint      TEXT,                       -- alert symptom; merges duplicate alerts
    scenario_id      TEXT    NOT NULL,
    scenario_hash    TEXT    NOT NULL,
    goal             TEXT    NOT NULL,
    trigger          TEXT    NOT NULL,           -- JSON: { "kind": "manual" | "alert", ... }
    state            TEXT    NOT NULL,
    state_version    INTEGER NOT NULL DEFAULT 0,
    provider         TEXT    NOT NULL,           -- "meta:<model>" or "replay"
    budget           TEXT    NOT NULL,           -- JSON, fixed at creation
    usage            TEXT    NOT NULL DEFAULT '{}', -- JSON: model_turns, tool_calls, candidates, tokens
    cancel_requested INTEGER NOT NULL DEFAULT 0,
    aa_noise         REAL,
    baseline_artifact_id TEXT,
    observation      TEXT,                       -- JSON from the observer role
    summary          TEXT,                       -- final human-readable result
    error            TEXT,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    finished_at      INTEGER
);
CREATE UNIQUE INDEX runs_active_fingerprint ON runs (fingerprint)
    WHERE fingerprint IS NOT NULL AND state NOT IN ('COMPLETED', 'EXHAUSTED', 'FAILED', 'CANCELLED');
CREATE INDEX runs_state ON runs (state);
CREATE INDEX runs_created ON runs (created_at);

CREATE TABLE trials (
    id                 TEXT    PRIMARY KEY,
    run_id             TEXT    NOT NULL REFERENCES runs (id),
    seq                INTEGER NOT NULL,
    hypothesis         TEXT    NOT NULL,
    plan               TEXT    NOT NULL,         -- JSON experiment plan (without the patch body)
    patch              TEXT    NOT NULL,         -- JSON PatchProposal
    patch_hash         TEXT    NOT NULL,
    status             TEXT    NOT NULL,         -- PENDING | RUNNING | DONE
    outcome            TEXT,                     -- VERIFIED | REJECTED | INCONCLUSIVE | EXECUTION_ERROR | POLICY_REJECTED
    evaluation         TEXT,                     -- JSON Evaluation
    report_artifact_id TEXT,
    interpretation     TEXT,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL
);
-- The same change is never measured twice in one run, whatever its summary says.
CREATE UNIQUE INDEX trials_run_patch ON trials (run_id, patch_hash);
CREATE UNIQUE INDEX trials_run_seq ON trials (run_id, seq);

CREATE TABLE jobs (
    id             TEXT    PRIMARY KEY,
    run_id         TEXT    NOT NULL REFERENCES runs (id),
    trial_id       TEXT,
    kind           TEXT    NOT NULL,             -- baseline | trial
    status         TEXT    NOT NULL,             -- pending | submitted | done | lost | cancelled
    attempt        INTEGER NOT NULL DEFAULT 1,
    runner_job_id  TEXT,
    runner_boot_id TEXT,
    runner_phase   TEXT,
    last_seen_at   INTEGER,
    error          TEXT,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
CREATE INDEX jobs_run ON jobs (run_id, status);

CREATE TABLE run_events (
    run_id  TEXT    NOT NULL,
    seq     INTEGER NOT NULL,
    ts      INTEGER NOT NULL,
    kind    TEXT    NOT NULL,
    payload TEXT    NOT NULL,                    -- JSON
    PRIMARY KEY (run_id, seq)
);

CREATE TABLE artifacts (
    id           TEXT    PRIMARY KEY,
    run_id       TEXT,
    kind         TEXT    NOT NULL,
    content_type TEXT    NOT NULL,
    sha256       TEXT    NOT NULL,
    size         INTEGER NOT NULL,
    r2_key       TEXT,                           -- set when the body lives in R2
    inline_body  TEXT,                           -- set when the body is small enough for D1
    created_at   INTEGER NOT NULL
);

-- Experiment memory: what was tried, under which conditions, and how it ended. Failures included.
CREATE TABLE experiment_cases (
    id                 TEXT    PRIMARY KEY,
    scenario_id        TEXT    NOT NULL,
    run_id             TEXT    NOT NULL,
    trial_id           TEXT    NOT NULL,
    hypothesis         TEXT    NOT NULL,
    patch_hash         TEXT    NOT NULL,
    outcome            TEXT    NOT NULL,
    median_improvement REAL,
    conditions         TEXT    NOT NULL,         -- JSON: scenario hash, runner env, load plan
    validation         TEXT    NOT NULL,         -- experiment_validated | human_reported
    created_at         INTEGER NOT NULL
);
CREATE INDEX experiment_cases_scenario ON experiment_cases (scenario_id, created_at);
