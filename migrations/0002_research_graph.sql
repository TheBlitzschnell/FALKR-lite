-- Research object graph.
--
-- This is what makes cost-per-run possible: a cost_events row carries a run_id
-- in its dimensional spine, and these tables are what that id points at.

CREATE TABLE experiments (
    id                  UUID PRIMARY KEY,
    tenant_id           UUID NOT NULL,
    project_id          UUID NOT NULL,
    name                TEXT NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE runs (
    id                      UUID PRIMARY KEY,
    tenant_id               UUID NOT NULL,
    experiment_id           UUID NOT NULL REFERENCES experiments (id),

    -- Ingestion identity. `(source, external_id)` is what the tracker calls
    -- this run; see the unique index below.
    external_source         TEXT NOT NULL,
    external_id             TEXT NOT NULL,

    status                  TEXT NOT NULL,
    started_at              TIMESTAMPTZ NOT NULL,
    ended_at                TIMESTAMPTZ,

    -- A projection recomputed from cost_events, never authored. Stored so
    -- "what did this run cost" is one read rather than an aggregate over the
    -- whole cost table.
    attributed_cost         NUMERIC(38, 12) NOT NULL DEFAULT 0,
    attributed_cost_currency TEXT NOT NULL,

    function_code           TEXT NOT NULL,
    capitalization_status   TEXT NOT NULL,

    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT runs_function_code_known
        CHECK (function_code IN ('COGS', 'RND', 'OPEX')),
    CONSTRAINT runs_capitalization_status_known
        CHECK (capitalization_status IN ('EXPENSED', 'CAPITALIZED', 'PENDING_REVIEW')),
    CONSTRAINT runs_currency_is_iso4217
        CHECK (attributed_cost_currency ~ '^[A-Z]{3}$'),
    CONSTRAINT runs_end_after_start
        CHECK (ended_at IS NULL OR ended_at >= started_at)
);

-- Ingestion idempotency. A W&B webhook and a poll can
-- deliver the same run within the same second; two rows for one training job
-- would double-count every cost attributed to it.
CREATE UNIQUE INDEX runs_external_identity
    ON runs (tenant_id, external_source, external_id);

CREATE INDEX runs_tenant_experiment ON runs (tenant_id, experiment_id);
CREATE INDEX runs_tenant_status ON runs (tenant_id, status);

-- Deliberately NO foreign key from cost_events.run_id to runs.id.
--
-- Provider billing and tracker ingestion are independent pipelines with
-- independent latency: a GPU node's cost export routinely lands before the W&B
-- run record does. A foreign key would force an ingestion order the real world
-- does not respect, and the alternative — holding the cost event until the run
-- appears — would violate the rule that dimensions are populated at write time
-- and never backfilled. Referential integrity here is
-- reconciled, not enforced.

CREATE TABLE checkpoints (
    id                  UUID PRIMARY KEY,
    tenant_id           UUID NOT NULL,
    run_id              UUID NOT NULL REFERENCES runs (id),
    storage_cost        NUMERIC(38, 12) NOT NULL,
    storage_cost_currency TEXT NOT NULL,
    storage_uri         TEXT NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT checkpoints_currency_is_iso4217
        CHECK (storage_cost_currency ~ '^[A-Z]{3}$')
);

CREATE INDEX checkpoints_tenant_run ON checkpoints (tenant_id, run_id);

-- RLS on every table, from the moment it exists.
ALTER TABLE experiments ENABLE ROW LEVEL SECURITY;
ALTER TABLE experiments FORCE ROW LEVEL SECURITY;
CREATE POLICY experiments_tenant_isolation ON experiments
    USING (tenant_id = NULLIF(current_setting('falkr.tenant_id', TRUE), '')::UUID)
    WITH CHECK (tenant_id = NULLIF(current_setting('falkr.tenant_id', TRUE), '')::UUID);

ALTER TABLE runs ENABLE ROW LEVEL SECURITY;
ALTER TABLE runs FORCE ROW LEVEL SECURITY;
CREATE POLICY runs_tenant_isolation ON runs
    USING (tenant_id = NULLIF(current_setting('falkr.tenant_id', TRUE), '')::UUID)
    WITH CHECK (tenant_id = NULLIF(current_setting('falkr.tenant_id', TRUE), '')::UUID);

ALTER TABLE checkpoints ENABLE ROW LEVEL SECURITY;
ALTER TABLE checkpoints FORCE ROW LEVEL SECURITY;
CREATE POLICY checkpoints_tenant_isolation ON checkpoints
    USING (tenant_id = NULLIF(current_setting('falkr.tenant_id', TRUE), '')::UUID)
    WITH CHECK (tenant_id = NULLIF(current_setting('falkr.tenant_id', TRUE), '')::UUID);
