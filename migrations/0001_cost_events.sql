-- Cost & usage event spine.
--
-- Two things are deliberately baked in at schema v1 rather than added later:
--
--   1. The full dimensional spine as NOT NULL where the
--      dimension is mandatory. Retrofitting these means re-deriving them from
--      provider invoices after the fact, which is the exact failure mode this
--      system exists to prevent.
--   2. Row-level security. Multi-tenancy is a security boundary, not a filter
--     , and a table that ships without RLS is a table
--      someone forgets to add it to.

CREATE TABLE cost_events (
    id                      UUID PRIMARY KEY,

    -- Dimensional spine. Mandatory dimensions are NOT NULL; a NULL on an
    -- optional one means "not applicable", never "not yet known".
    tenant_id               UUID        NOT NULL,
    project_id              UUID        NOT NULL,
    run_id                  UUID,
    team_id                 UUID        NOT NULL,
    customer_id             UUID,
    model_id                UUID,
    dataset_id              UUID,
    provider_id             UUID        NOT NULL,
    function_code           TEXT        NOT NULL,
    commitment_id           UUID,

    -- FOCUS record.
    billing_account_id      TEXT        NOT NULL,
    charge_period_start     TIMESTAMPTZ NOT NULL,
    charge_period_end       TIMESTAMPTZ NOT NULL,
    -- One currency for all three amounts; CostEvent::new enforces it in Rust
    -- and the CHECK below keeps it true for anything written by other means.
    currency                TEXT        NOT NULL,
    -- NUMERIC, never DOUBLE PRECISION. Scale 12 carries
    -- sub-cent token pricing without rounding at rest.
    billed_cost             NUMERIC(38, 12) NOT NULL,
    effective_cost          NUMERIC(38, 12) NOT NULL,
    list_cost               NUMERIC(38, 12) NOT NULL,
    service_name            TEXT        NOT NULL,
    service_category        TEXT        NOT NULL,
    charge_category         TEXT        NOT NULL,
    resource_id             TEXT,
    region_id               TEXT,
    tags                    JSONB       NOT NULL DEFAULT '{}'::jsonb,

    -- Provenance back to the provider invoice line.
    source_provider         TEXT        NOT NULL,
    source_external_id      TEXT        NOT NULL,
    source_export_ref       TEXT        NOT NULL,
    source_billing_period   TEXT        NOT NULL,
    source_content_hash     TEXT        NOT NULL,
    source_ingested_at      TIMESTAMPTZ NOT NULL,

    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT cost_events_period_is_positive
        CHECK (charge_period_end > charge_period_start),
    CONSTRAINT cost_events_currency_is_iso4217
        CHECK (currency ~ '^[A-Z]{3}$'),
    CONSTRAINT cost_events_function_code_known
        CHECK (function_code IN ('COGS', 'RND', 'OPEX'))
);

-- Idempotency. Both polling and webhooks double-deliver, so
-- a provider row's identity — scoped to the tenant, because two tenants can
-- legitimately hold the same provider-side id — is what ingestion upserts on.
CREATE UNIQUE INDEX cost_events_source_identity
    ON cost_events (tenant_id, source_provider, source_external_id);

-- Attribution-coverage queries (the 80% build gate, the cost-spine design) scan
-- by tenant and ingestion time.
CREATE INDEX cost_events_tenant_ingested
    ON cost_events (tenant_id, source_ingested_at DESC);

-- Cost-per-run is the headline query this whole spine exists to answer.
CREATE INDEX cost_events_tenant_run
    ON cost_events (tenant_id, run_id)
    WHERE run_id IS NOT NULL;

CREATE INDEX cost_events_tenant_project_period
    ON cost_events (tenant_id, project_id, charge_period_start DESC);

-- --------------------------------------------------------------------------
-- Row-level security.
--
-- FORCE is not optional here. Plain ENABLE exempts the table owner, and
-- application connections very often *are* the owner in development — which
-- means the isolation looks correct in every test right up until it is
-- load-bearing in production.
--
-- The tenant comes from a transaction-local GUC set via set_config(..., true).
-- current_setting(..., true) yields NULL when unset, so an unscoped connection
-- matches no rows rather than all of them: this fails closed.
--
-- See the multi-tenancy design for the PgBouncer caveat — transaction-mode pooling
-- is safe with SET LOCAL precisely because both the setting and the statements
-- live inside one transaction.
-- --------------------------------------------------------------------------
ALTER TABLE cost_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE cost_events FORCE ROW LEVEL SECURITY;

CREATE POLICY cost_events_tenant_isolation ON cost_events
    USING (tenant_id = NULLIF(current_setting('falkr.tenant_id', TRUE), '')::UUID)
    WITH CHECK (tenant_id = NULLIF(current_setting('falkr.tenant_id', TRUE), '')::UUID);
