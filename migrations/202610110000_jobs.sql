-- A durable job queue. Jobs are inserted in the same transaction as the
-- change that causes them (an order placed, a payment confirmed), so work is
-- never lost to a crash and never done for a change that rolled back.

CREATE TABLE jobs (
    id           UUID PRIMARY KEY,
    kind         TEXT NOT NULL,
    payload      JSONB NOT NULL,
    -- Stops the same job being queued twice, e.g. 'order:<id>:paid'.
    dedupe_key   TEXT UNIQUE,
    run_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts     INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 8,
    -- While set and in the future, a worker owns the job. A worker that dies
    -- mid-job just lets this lapse, and the job is picked up again.
    locked_until TIMESTAMPTZ,
    last_error   TEXT,
    completed_at TIMESTAMPTZ,
    -- Set after the last attempt fails; the job is kept for inspection.
    failed_at    TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX jobs_due_idx ON jobs (run_at) WHERE completed_at IS NULL AND failed_at IS NULL;
CREATE INDEX jobs_finished_idx ON jobs (completed_at) WHERE completed_at IS NOT NULL;
