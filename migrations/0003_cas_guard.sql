-- Makes a state transition all-or-nothing. The transition batch runs its compare-and-set UPDATE
-- on runs, then writes changes() into this table. When the UPDATE matched no row (another writer
-- moved the run first), the CHECK fails and the whole batch rolls back, including the follow-up
-- statements (jobs, trials, events) that belong to the transition.
CREATE TABLE cas_guard (
    id  INTEGER PRIMARY KEY CHECK (id = 1),
    won INTEGER NOT NULL CONSTRAINT cas_lost CHECK (won = 1)
);
