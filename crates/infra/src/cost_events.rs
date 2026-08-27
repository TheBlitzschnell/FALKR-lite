//! Postgres implementation of [`CostEventStore`].
//!
//! The SQL is written out rather than generated: when an audit
//! asks what touched a cost figure, the answer should be readable in this file.

use chrono::{DateTime, Utc};
use falkr_core::{
    CommitmentId, CostEventId, Currency, CustomerId, DatasetId, Dimensions, FunctionCode, ModelId,
    Money, ProjectId, ProviderId, RunId, TeamId, TenantId,
};
use falkr_cost_spine::event::{
    ChargeCategory, CostEvent, ProviderKind, ServiceCategory, SourceRef,
};
use falkr_cost_spine::store::{CostEventStore, Coverage, StoreError, UpsertOutcome};
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

impl From<sqlx::Error> for StoreErrorShim {
    fn from(e: sqlx::Error) -> Self {
        Self(StoreError::Backend(e.to_string()))
    }
}

/// Local newtype so `?` can convert `sqlx::Error` into the domain's
/// `StoreError` without `infra` owning either type's `From` impl.
struct StoreErrorShim(StoreError);

impl From<crate::db::DbError> for StoreErrorShim {
    fn from(e: crate::db::DbError) -> Self {
        Self(StoreError::Backend(e.to_string()))
    }
}

impl From<StoreErrorShim> for StoreError {
    fn from(s: StoreErrorShim) -> Self {
        s.0
    }
}

/// A [`CostEventStore`] backed by Postgres with row-level security.
pub struct PgCostEventStore {
    pool: PgPool,
}

impl PgCostEventStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Columns selected whenever a full [`CostEvent`] is rebuilt.
const SELECT_COLUMNS: &str = "
    id, tenant_id, project_id, run_id, team_id, customer_id, model_id,
    dataset_id, provider_id, function_code, commitment_id,
    billing_account_id, charge_period_start, charge_period_end, currency,
    billed_cost, effective_cost, list_cost, service_name, service_category,
    charge_category, resource_id, region_id, tags,
    source_provider, source_external_id, source_export_ref,
    source_billing_period, source_content_hash, source_ingested_at
";

fn decode_event(row: &sqlx::postgres::PgRow) -> Result<CostEvent, StoreError> {
    let bad = |what: &str, value: String| {
        StoreError::Backend(format!("stored {what} is not a recognized value: {value}"))
    };

    let currency_code: String = row.try_get("currency").map_err(to_backend)?;
    let currency =
        Currency::from_code(&currency_code).ok_or_else(|| bad("currency", currency_code))?;
    let money = |col: &str| -> Result<Money, StoreError> {
        Ok(Money::new(row.try_get(col).map_err(to_backend)?, currency))
    };

    let function_code_raw: String = row.try_get("function_code").map_err(to_backend)?;
    let function_code = FunctionCode::from_stored(&function_code_raw)
        .ok_or_else(|| bad("function_code", function_code_raw))?;

    let mut dims = Dimensions::new(
        TenantId::from_uuid(row.try_get("tenant_id").map_err(to_backend)?),
        ProjectId::from_uuid(row.try_get("project_id").map_err(to_backend)?),
        TeamId::from_uuid(row.try_get("team_id").map_err(to_backend)?),
        ProviderId::from_uuid(row.try_get("provider_id").map_err(to_backend)?),
        function_code,
    );
    if let Some(id) = row
        .try_get::<Option<Uuid>, _>("run_id")
        .map_err(to_backend)?
    {
        dims = dims.with_run(RunId::from_uuid(id));
    }
    if let Some(id) = row
        .try_get::<Option<Uuid>, _>("customer_id")
        .map_err(to_backend)?
    {
        dims = dims.with_customer(CustomerId::from_uuid(id));
    }
    if let Some(id) = row
        .try_get::<Option<Uuid>, _>("model_id")
        .map_err(to_backend)?
    {
        dims = dims.with_model(ModelId::from_uuid(id));
    }
    if let Some(id) = row
        .try_get::<Option<Uuid>, _>("dataset_id")
        .map_err(to_backend)?
    {
        dims = dims.with_dataset(DatasetId::from_uuid(id));
    }
    if let Some(id) = row
        .try_get::<Option<Uuid>, _>("commitment_id")
        .map_err(to_backend)?
    {
        dims = dims.with_commitment(CommitmentId::from_uuid(id));
    }

    let provider_raw: String = row.try_get("source_provider").map_err(to_backend)?;
    let provider = ProviderKind::from_stored(&provider_raw)
        .ok_or_else(|| bad("source_provider", provider_raw))?;
    let category_raw: String = row.try_get("charge_category").map_err(to_backend)?;
    let charge_category = ChargeCategory::from_stored(&category_raw)
        .ok_or_else(|| bad("charge_category", category_raw))?;
    let service_raw: String = row.try_get("service_category").map_err(to_backend)?;
    let service_category = ServiceCategory::from_stored(&service_raw)
        .ok_or_else(|| bad("service_category", service_raw))?;

    // Reconstructed directly rather than through `CostEvent::new`, because the
    // id must be the stored one, not a freshly generated one. The invariants
    // `new` checks are enforced at write time and by the table's CHECK
    // constraints, so a stored row has already satisfied them.
    Ok(CostEvent {
        id: CostEventId::from_uuid(row.try_get("id").map_err(to_backend)?),
        billing_account_id: row.try_get("billing_account_id").map_err(to_backend)?,
        charge_period_start: row.try_get("charge_period_start").map_err(to_backend)?,
        charge_period_end: row.try_get("charge_period_end").map_err(to_backend)?,
        billed_cost: money("billed_cost")?,
        effective_cost: money("effective_cost")?,
        list_cost: money("list_cost")?,
        service_name: row.try_get("service_name").map_err(to_backend)?,
        service_category,
        charge_category,
        resource_id: row.try_get("resource_id").map_err(to_backend)?,
        region_id: row.try_get("region_id").map_err(to_backend)?,
        tags: row.try_get("tags").map_err(to_backend)?,
        dims,
        source_ref: SourceRef {
            provider,
            external_id: row.try_get("source_external_id").map_err(to_backend)?,
            export_ref: row.try_get("source_export_ref").map_err(to_backend)?,
            billing_period: row.try_get("source_billing_period").map_err(to_backend)?,
            content_hash: row.try_get("source_content_hash").map_err(to_backend)?,
            ingested_at: row.try_get("source_ingested_at").map_err(to_backend)?,
        },
    })
}

fn to_backend(e: sqlx::Error) -> StoreError {
    StoreError::Backend(e.to_string())
}

#[async_trait::async_trait]
impl CostEventStore for PgCostEventStore {
    /// Idempotent batch upsert.
    ///
    /// `ON CONFLICT ... DO UPDATE ... WHERE content_hash IS DISTINCT FROM` is
    /// what separates the three outcomes:
    ///
    /// - no conflict → **inserted** (`xmax = 0` on the returned row)
    /// - conflict, content differs → **restated**, the row is updated in place
    /// - conflict, content identical → the `WHERE` suppresses the update, no
    ///   row comes back, and it is counted as a **duplicate**
    ///
    /// Suppressing the no-op update matters beyond bookkeeping: it keeps
    /// `updated_at` honest and avoids writing a new row version every time a
    /// poller re-reads an unchanged export.
    async fn upsert(&self, events: &[CostEvent]) -> Result<UpsertOutcome, StoreError> {
        let Some(first) = events.first() else {
            return Ok(UpsertOutcome::default());
        };

        // Every event in a batch must belong to one tenant: the transaction
        // carries exactly one tenant context, and a mixed batch would be
        // rejected by the RLS WITH CHECK clause anyway. Failing here gives a
        // comprehensible error instead of a policy violation.
        let tenant_id = first.dims.tenant_id;
        if events.iter().any(|e| e.dims.tenant_id != tenant_id) {
            return Err(StoreError::Backend(
                "batch spans multiple tenants; upsert one tenant at a time".to_owned(),
            ));
        }

        let mut tx = crate::db::begin_tenant_tx(&self.pool, tenant_id)
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;

        let mut outcome = UpsertOutcome::default();
        for event in events {
            let row = sqlx::query(
                "
                INSERT INTO cost_events (
                    id, tenant_id, project_id, run_id, team_id, customer_id,
                    model_id, dataset_id, provider_id, function_code, commitment_id,
                    billing_account_id, charge_period_start, charge_period_end,
                    currency, billed_cost, effective_cost, list_cost,
                    service_name, service_category, charge_category,
                    resource_id, region_id, tags,
                    source_provider, source_external_id, source_export_ref,
                    source_billing_period, source_content_hash, source_ingested_at
                )
                VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                    $12, $13, $14, $15, $16, $17, $18, $19, $20, $21,
                    $22, $23, $24, $25, $26, $27, $28, $29, $30
                )
                ON CONFLICT (tenant_id, source_provider, source_external_id)
                DO UPDATE SET
                    project_id            = EXCLUDED.project_id,
                    run_id                = EXCLUDED.run_id,
                    team_id               = EXCLUDED.team_id,
                    customer_id           = EXCLUDED.customer_id,
                    model_id              = EXCLUDED.model_id,
                    dataset_id            = EXCLUDED.dataset_id,
                    provider_id           = EXCLUDED.provider_id,
                    function_code         = EXCLUDED.function_code,
                    commitment_id         = EXCLUDED.commitment_id,
                    billing_account_id    = EXCLUDED.billing_account_id,
                    charge_period_start   = EXCLUDED.charge_period_start,
                    charge_period_end     = EXCLUDED.charge_period_end,
                    currency              = EXCLUDED.currency,
                    billed_cost           = EXCLUDED.billed_cost,
                    effective_cost        = EXCLUDED.effective_cost,
                    list_cost             = EXCLUDED.list_cost,
                    service_name          = EXCLUDED.service_name,
                    service_category      = EXCLUDED.service_category,
                    charge_category       = EXCLUDED.charge_category,
                    resource_id           = EXCLUDED.resource_id,
                    region_id             = EXCLUDED.region_id,
                    tags                  = EXCLUDED.tags,
                    source_export_ref     = EXCLUDED.source_export_ref,
                    source_billing_period = EXCLUDED.source_billing_period,
                    source_content_hash   = EXCLUDED.source_content_hash,
                    source_ingested_at    = EXCLUDED.source_ingested_at,
                    updated_at            = now()
                WHERE cost_events.source_content_hash IS DISTINCT FROM EXCLUDED.source_content_hash
                RETURNING (xmax = 0) AS inserted
                ",
            )
            .bind(event.id.as_uuid())
            .bind(event.dims.tenant_id.as_uuid())
            .bind(event.dims.project_id.as_uuid())
            .bind(event.dims.run_id.map(RunId::as_uuid))
            .bind(event.dims.team_id.as_uuid())
            .bind(event.dims.customer_id.map(CustomerId::as_uuid))
            .bind(event.dims.model_id.map(ModelId::as_uuid))
            .bind(event.dims.dataset_id.map(DatasetId::as_uuid))
            .bind(event.dims.provider_id.as_uuid())
            .bind(event.dims.function_code.as_str())
            .bind(event.dims.commitment_id.map(CommitmentId::as_uuid))
            .bind(&event.billing_account_id)
            .bind(event.charge_period_start)
            .bind(event.charge_period_end)
            .bind(event.currency().code())
            .bind(event.billed_cost.amount())
            .bind(event.effective_cost.amount())
            .bind(event.list_cost.amount())
            .bind(&event.service_name)
            .bind(event.service_category.as_str())
            .bind(event.charge_category.as_str())
            .bind(event.resource_id.as_deref())
            .bind(event.region_id.as_deref())
            .bind(&event.tags)
            .bind(event.source_ref.provider.as_str())
            .bind(&event.source_ref.external_id)
            .bind(&event.source_ref.export_ref)
            .bind(&event.source_ref.billing_period)
            .bind(&event.source_ref.content_hash)
            .bind(event.source_ref.ingested_at)
            .fetch_optional(&mut *tx)
            .await
            .map_err(to_backend)?;

            match row {
                None => outcome.duplicate += 1,
                Some(r) => {
                    let inserted: bool = r.try_get("inserted").map_err(to_backend)?;
                    if inserted {
                        outcome.inserted += 1;
                    } else {
                        outcome.restated += 1;
                    }
                }
            }
        }

        tx.commit().await.map_err(to_backend)?;
        Ok(outcome)
    }

    async fn find_by_external_id(
        &self,
        tenant_id: TenantId,
        provider: ProviderKind,
        external_id: &str,
    ) -> Result<Option<CostEvent>, StoreError> {
        let mut tx = crate::db::begin_tenant_tx(&self.pool, tenant_id)
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;

        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM cost_events
             WHERE source_provider = $1 AND source_external_id = $2"
        );
        let row = sqlx::query(&sql)
            .bind(provider.as_str())
            .bind(external_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(to_backend)?;

        tx.commit().await.map_err(to_backend)?;
        row.as_ref().map(decode_event).transpose()
    }

    async fn attribution_coverage(
        &self,
        tenant_id: TenantId,
        since: DateTime<Utc>,
    ) -> Result<Coverage, StoreError> {
        let mut tx = crate::db::begin_tenant_tx(&self.pool, tenant_id)
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;

        let row = sqlx::query(
            "
            SELECT
                COUNT(*)                                        AS total_events,
                COUNT(*) FILTER (WHERE run_id IS NOT NULL)      AS run_attributed
            FROM cost_events
            WHERE source_ingested_at >= $1
            ",
        )
        .bind(since)
        .fetch_one(&mut *tx)
        .await
        .map_err(to_backend)?;

        let coverage = Coverage {
            total_events: row.try_get("total_events").map_err(to_backend)?,
            run_attributed: row.try_get("run_attributed").map_err(to_backend)?,
        };
        tx.commit().await.map_err(to_backend)?;
        Ok(coverage)
    }
}
