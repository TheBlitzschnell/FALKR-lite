//! Postgres implementation of [`ResearchGraphStore`].

use falkr_core::{
    CapitalizationStatus, Currency, ExperimentId, FunctionCode, Money, RunId, TenantId,
};
use falkr_research_graph::entities::{Experiment, ExternalRef, Run, RunStatus, TrackerSource};
use falkr_research_graph::store::{ResearchGraphStore, ResearchStoreError, RunUpsert};
use rust_decimal::Decimal;
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

fn backend(e: sqlx::Error) -> ResearchStoreError {
    ResearchStoreError::Backend(e.to_string())
}

fn corrupt(what: &str, value: String) -> ResearchStoreError {
    ResearchStoreError::Backend(format!("stored {what} is not a recognized value: {value}"))
}

pub struct PgResearchGraphStore {
    pool: PgPool,
}

impl PgResearchGraphStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn decode_run(row: &sqlx::postgres::PgRow) -> Result<Run, ResearchStoreError> {
    let source_raw: String = row.try_get("external_source").map_err(backend)?;
    let source = TrackerSource::from_stored(&source_raw)
        .ok_or_else(|| corrupt("external_source", source_raw))?;
    let status_raw: String = row.try_get("status").map_err(backend)?;
    let status =
        RunStatus::from_stored(&status_raw).ok_or_else(|| corrupt("status", status_raw))?;
    let fc_raw: String = row.try_get("function_code").map_err(backend)?;
    let function_code =
        FunctionCode::from_stored(&fc_raw).ok_or_else(|| corrupt("function_code", fc_raw))?;
    let cap_raw: String = row.try_get("capitalization_status").map_err(backend)?;
    let capitalization_status = CapitalizationStatus::from_stored(&cap_raw)
        .ok_or_else(|| corrupt("capitalization_status", cap_raw))?;
    let currency_raw: String = row.try_get("attributed_cost_currency").map_err(backend)?;
    let currency = Currency::from_code(&currency_raw)
        .ok_or_else(|| corrupt("attributed_cost_currency", currency_raw))?;

    Ok(Run {
        id: RunId::from_uuid(row.try_get("id").map_err(backend)?),
        tenant_id: TenantId::from_uuid(row.try_get("tenant_id").map_err(backend)?),
        experiment_id: ExperimentId::from_uuid(row.try_get("experiment_id").map_err(backend)?),
        external_ref: ExternalRef {
            source,
            external_id: row.try_get("external_id").map_err(backend)?,
        },
        status,
        started_at: row.try_get("started_at").map_err(backend)?,
        ended_at: row.try_get("ended_at").map_err(backend)?,
        attributed_cost: Money::new(row.try_get("attributed_cost").map_err(backend)?, currency),
        function_code,
        capitalization_status,
    })
}

const RUN_COLUMNS: &str = "
    id, tenant_id, experiment_id, external_source, external_id, status,
    started_at, ended_at, attributed_cost, attributed_cost_currency,
    function_code, capitalization_status
";

#[async_trait::async_trait]
impl ResearchGraphStore for PgResearchGraphStore {
    async fn upsert_experiment(&self, experiment: &Experiment) -> Result<(), ResearchStoreError> {
        let mut tx = crate::db::begin_tenant_tx(&self.pool, experiment.tenant_id)
            .await
            .map_err(|e| ResearchStoreError::Backend(e.to_string()))?;

        sqlx::query(
            "INSERT INTO experiments (id, tenant_id, project_id, name)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, updated_at = now()",
        )
        .bind(experiment.id.as_uuid())
        .bind(experiment.tenant_id.as_uuid())
        .bind(experiment.project_id.as_uuid())
        .bind(&experiment.name)
        .execute(&mut *tx)
        .await
        .map_err(backend)?;

        tx.commit().await.map_err(backend)?;
        Ok(())
    }

    /// Idempotent on `(tenant_id, external_source, external_id)`.
    ///
    /// The conflict target is the tracker's identity for the run, **not** our
    /// surrogate `id`: normalization mints a fresh `RunId` on every delivery,
    /// so keying on `id` would insert a new row each time and double-count the
    /// run's cost. `RETURNING` reports which of the three things happened —
    /// insert, genuine update, or a re-delivery that changed nothing.
    async fn upsert_run(&self, run: &Run) -> Result<RunUpsert, ResearchStoreError> {
        let mut tx = crate::db::begin_tenant_tx(&self.pool, run.tenant_id)
            .await
            .map_err(|e| ResearchStoreError::Backend(e.to_string()))?;

        let row = sqlx::query(
            "
            INSERT INTO runs (
                id, tenant_id, experiment_id, external_source, external_id,
                status, started_at, ended_at, attributed_cost,
                attributed_cost_currency, function_code, capitalization_status
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (tenant_id, external_source, external_id)
            DO UPDATE SET
                status                = EXCLUDED.status,
                started_at            = EXCLUDED.started_at,
                ended_at              = EXCLUDED.ended_at,
                function_code         = EXCLUDED.function_code,
                capitalization_status = EXCLUDED.capitalization_status,
                updated_at            = now()
            WHERE
                runs.status                IS DISTINCT FROM EXCLUDED.status
                OR runs.started_at         IS DISTINCT FROM EXCLUDED.started_at
                OR runs.ended_at           IS DISTINCT FROM EXCLUDED.ended_at
                OR runs.function_code      IS DISTINCT FROM EXCLUDED.function_code
                OR runs.capitalization_status IS DISTINCT FROM EXCLUDED.capitalization_status
            RETURNING id, (xmax = 0) AS inserted
            ",
        )
        .bind(run.id.as_uuid())
        .bind(run.tenant_id.as_uuid())
        .bind(run.experiment_id.as_uuid())
        .bind(run.external_ref.source.as_str())
        .bind(&run.external_ref.external_id)
        .bind(run.status.as_str())
        .bind(run.started_at)
        .bind(run.ended_at)
        .bind(run.attributed_cost.amount())
        .bind(run.attributed_cost.currency().code())
        .bind(run.function_code.as_str())
        .bind(run.capitalization_status.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend)?;

        let outcome = match row {
            Some(r) => {
                let id = RunId::from_uuid(r.try_get("id").map_err(backend)?);
                if r.try_get::<bool, _>("inserted").map_err(backend)? {
                    RunUpsert::Inserted(id)
                } else {
                    RunUpsert::Updated(id)
                }
            }
            // The DO UPDATE ... WHERE suppressed the write because nothing
            // changed. The row exists; find its id so the caller still gets one.
            None => {
                let existing: Uuid = sqlx::query_scalar(
                    "SELECT id FROM runs
                     WHERE external_source = $1 AND external_id = $2",
                )
                .bind(run.external_ref.source.as_str())
                .bind(&run.external_ref.external_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(backend)?;
                RunUpsert::Unchanged(RunId::from_uuid(existing))
            }
        };

        tx.commit().await.map_err(backend)?;
        Ok(outcome)
    }

    async fn find_run_by_external_ref(
        &self,
        tenant_id: TenantId,
        external_ref: &ExternalRef,
    ) -> Result<Option<Run>, ResearchStoreError> {
        let mut tx = crate::db::begin_tenant_tx(&self.pool, tenant_id)
            .await
            .map_err(|e| ResearchStoreError::Backend(e.to_string()))?;

        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM runs
             WHERE external_source = $1 AND external_id = $2"
        );
        let row = sqlx::query(&sql)
            .bind(external_ref.source.as_str())
            .bind(&external_ref.external_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;

        tx.commit().await.map_err(backend)?;
        row.as_ref().map(decode_run).transpose()
    }

    async fn find_run(
        &self,
        tenant_id: TenantId,
        run_id: RunId,
    ) -> Result<Option<Run>, ResearchStoreError> {
        let mut tx = crate::db::begin_tenant_tx(&self.pool, tenant_id)
            .await
            .map_err(|e| ResearchStoreError::Backend(e.to_string()))?;

        let sql = format!("SELECT {RUN_COLUMNS} FROM runs WHERE id = $1");
        let row = sqlx::query(&sql)
            .bind(run_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(backend)?;

        tx.commit().await.map_err(backend)?;
        row.as_ref().map(decode_run).transpose()
    }

    /// Recomputes cost-per-run from the cost spine.
    ///
    /// This is the join the whole dimensional spine exists to make possible:
    /// every `CostEvent` whose `dims.run_id` is this run, summed. It is a
    /// projection — recomputed, never authored — so running it twice is
    /// harmless and running it after late-arriving cost is required.
    async fn refresh_attributed_cost(
        &self,
        tenant_id: TenantId,
        run_id: RunId,
    ) -> Result<Money, ResearchStoreError> {
        let mut tx = crate::db::begin_tenant_tx(&self.pool, tenant_id)
            .await
            .map_err(|e| ResearchStoreError::Backend(e.to_string()))?;

        let currency_raw: String =
            sqlx::query_scalar("SELECT attributed_cost_currency FROM runs WHERE id = $1")
                .bind(run_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(backend)?
                .ok_or_else(|| ResearchStoreError::Backend(format!("run {run_id} not found")))?;
        let currency = Currency::from_code(&currency_raw)
            .ok_or_else(|| corrupt("attributed_cost_currency", currency_raw.clone()))?;

        // Only costs denominated in the run's own currency are summed. A run
        // accruing cost in two currencies needs an explicit FX decision, and
        // silently adding them would produce a number that means nothing.
        let total: Option<Decimal> = sqlx::query_scalar(
            "SELECT SUM(effective_cost) FROM cost_events
             WHERE run_id = $1 AND currency = $2",
        )
        .bind(run_id.as_uuid())
        .bind(&currency_raw)
        .fetch_one(&mut *tx)
        .await
        .map_err(backend)?;

        let attributed = Money::new(total.unwrap_or(Decimal::ZERO), currency);

        sqlx::query("UPDATE runs SET attributed_cost = $1, updated_at = now() WHERE id = $2")
            .bind(attributed.amount())
            .bind(run_id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;

        tx.commit().await.map_err(backend)?;
        Ok(attributed)
    }
}
