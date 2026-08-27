-- Event-sourced ledger.
--
-- This is the append-only log. Balances are projections rebuilt from it, and
-- LedgerEvents are the only way a balance changes — there is
-- deliberately no balances table here for anything to UPDATE.

CREATE TABLE ledger_events (
    -- Uniquely identifies an event among all events from all aggregates.
    id                  UUID PRIMARY KEY,
    aggregate_id        UUID NOT NULL,
    tenant_id           UUID NOT NULL,

    -- The serialized LedgerEvent. JSONB rather than a column per variant: the
    -- event enum is versioned domain data, and a schema migration every time a
    -- variant gains a field would make the log the least flexible part of the
    -- system instead of the most durable.
    payload             JSONB NOT NULL,

    occurred_on         TIMESTAMPTZ NOT NULL,
    sequence_number     INTEGER NOT NULL,
    version             INTEGER,

    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ledger_events_sequence_is_positive CHECK (sequence_number >= 0)
);

-- Optimistic concurrency. Two writers that both read an aggregate at sequence
-- N and both try to append N+1: one commits, the other violates this and must
-- reload and retry. This is the actual protection against lost updates — the
-- advisory lock in the store is an optimization on top of it, not a substitute.
CREATE UNIQUE INDEX ledger_events_aggregate_sequence
    ON ledger_events (aggregate_id, sequence_number);

-- Replay reads the whole stream for an aggregate in order.
CREATE INDEX ledger_events_replay
    ON ledger_events (tenant_id, aggregate_id, sequence_number);

-- Time-ordered scans for reporting periods.
CREATE INDEX ledger_events_tenant_occurred
    ON ledger_events (tenant_id, occurred_on DESC);

-- An append-only log means exactly that: no UPDATE, no DELETE. Enforced in the
-- database rather than only in review, because "the ledger is immutable" is the
-- product's entire pitch and a single stray UPDATE would falsify it.
--
-- Deletion for tenant offboarding is a privileged, out-of-band operation: it
-- runs as a role that this trigger exempts, rather than by relaxing the rule.
CREATE OR REPLACE FUNCTION ledger_events_reject_mutation()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION
        'ledger_events is append-only; % is not permitted. Post a reversing entry instead.',
        TG_OP;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER ledger_events_no_update
    BEFORE UPDATE ON ledger_events
    FOR EACH ROW EXECUTE FUNCTION ledger_events_reject_mutation();

CREATE TRIGGER ledger_events_no_delete
    BEFORE DELETE ON ledger_events
    FOR EACH ROW EXECUTE FUNCTION ledger_events_reject_mutation();

ALTER TABLE ledger_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE ledger_events FORCE ROW LEVEL SECURITY;
CREATE POLICY ledger_events_tenant_isolation ON ledger_events
    USING (tenant_id = NULLIF(current_setting('falkr.tenant_id', TRUE), '')::UUID)
    WITH CHECK (tenant_id = NULLIF(current_setting('falkr.tenant_id', TRUE), '')::UUID);
