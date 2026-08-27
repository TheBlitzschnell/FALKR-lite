//! Research-graph endpoints, including cost-per-run.

use axum::Json;
use axum::extract::{Path, State};
use falkr_core::RunId;
use uuid::Uuid;

use crate::auth::{Scope, TenantContext};
use crate::error::{ApiError, storage};
use crate::state::AppState;

#[derive(serde::Serialize)]
pub struct RunView {
    pub id: Uuid,
    pub experiment_id: Uuid,
    pub external_source: String,
    pub external_id: String,
    pub status: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub function_code: String,
    pub capitalization_status: String,
    pub attributed_cost: String,
    pub currency: String,
}

pub async fn get_run(
    State(app): State<AppState>,
    ctx: TenantContext,
    Path(run_id): Path<Uuid>,
) -> Result<Json<RunView>, ApiError> {
    ctx.require(Scope::Read)?;

    let run = app
        .research
        .find_run(ctx.tenant_id, RunId::from_uuid(run_id))
        .await
        .map_err(storage)?
        .ok_or(ApiError::NotFound("run"))?;

    Ok(Json(RunView {
        id: run.id.as_uuid(),
        experiment_id: run.experiment_id.as_uuid(),
        external_source: run.external_ref.source.as_str().to_owned(),
        external_id: run.external_ref.external_id,
        status: run.status.as_str().to_owned(),
        started_at: run.started_at,
        ended_at: run.ended_at,
        function_code: run.function_code.as_str().to_owned(),
        capitalization_status: run.capitalization_status.as_str().to_owned(),
        attributed_cost: run.attributed_cost.amount().to_string(),
        currency: run.attributed_cost.currency().code().to_owned(),
    }))
}

#[derive(serde::Serialize)]
pub struct RunCostView {
    pub run_id: Uuid,
    /// Serialized as a string, never a JSON number.
    ///
    /// A JSON number is an IEEE 754 double to most clients, which would undo
    /// the exact-decimal guarantee at the last possible moment — after the
    /// ledger, the rating engine and the database have all preserved it.
    pub attributed_cost: String,
    pub currency: String,
}

/// Cost-per-run: the query the whole dimensional spine exists to answer.
///
/// Recomputes from the cost spine rather than reading the stored projection, so
/// late-arriving provider data is reflected. The recompute is idempotent.
pub async fn get_run_cost(
    State(app): State<AppState>,
    ctx: TenantContext,
    Path(run_id): Path<Uuid>,
) -> Result<Json<RunCostView>, ApiError> {
    ctx.require(Scope::Read)?;
    let run_id = RunId::from_uuid(run_id);

    if app
        .research
        .find_run(ctx.tenant_id, run_id)
        .await
        .map_err(storage)?
        .is_none()
    {
        return Err(ApiError::NotFound("run"));
    }

    let cost = app
        .research
        .refresh_attributed_cost(ctx.tenant_id, run_id)
        .await
        .map_err(storage)?;

    Ok(Json(RunCostView {
        run_id: run_id.as_uuid(),
        attributed_cost: cost.amount().to_string(),
        currency: cost.currency().code().to_owned(),
    }))
}
