//! Cost-spine and commitment endpoints.

use axum::Json;
use axum::extract::{Query, State};
use chrono::{DateTime, Utc};

use crate::auth::{Scope, TenantContext};
use crate::error::{ApiError, storage};
use crate::state::AppState;

#[derive(serde::Deserialize)]
pub struct CoverageQuery {
    /// Defaults to 24 hours ago — the window the cost-spine design defines the
    /// build gate over.
    pub since: Option<DateTime<Utc>>,
}

#[derive(serde::Serialize)]
pub struct CoverageView {
    pub total_events: i64,
    pub run_attributed: i64,
    pub fraction: String,
    /// Whether coverage has fallen below the 80% line that makes the telemetry
    /// attribution path mandatory rather than optional.
    pub below_build_gate: bool,
}

pub async fn attribution_coverage(
    State(app): State<AppState>,
    ctx: TenantContext,
    Query(q): Query<CoverageQuery>,
) -> Result<Json<CoverageView>, ApiError> {
    ctx.require(Scope::Read)?;
    let since = q
        .since
        .unwrap_or_else(|| Utc::now() - chrono::Duration::hours(24));

    let coverage = app
        .costs
        .attribution_coverage(ctx.tenant_id, since)
        .await
        .map_err(storage)?;

    Ok(Json(CoverageView {
        total_events: coverage.total_events,
        run_attributed: coverage.run_attributed,
        fraction: coverage.fraction().to_string(),
        below_build_gate: coverage.below_build_gate(),
    }))
}
