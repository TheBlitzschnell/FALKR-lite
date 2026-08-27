//! Ledger projections.

use axum::Json;
use axum::extract::{Path, State};
use falkr_core::Currency;
use uuid::Uuid;

use crate::auth::{Scope, TenantContext};
use crate::error::{ApiError, storage};
use crate::state::AppState;

#[derive(serde::Serialize)]
pub struct AccountBalance {
    pub account: Uuid,
    pub balance: String,
    pub currency: String,
}

#[derive(serde::Serialize)]
pub struct BalanceView {
    pub aggregate_id: Uuid,
    pub entry_count: usize,
    pub event_count: usize,
    pub accounts: Vec<AccountBalance>,
    /// Zero for a well-formed ledger. Surfaced rather than assumed: a non-zero
    /// value means an unbalanced entry reached the log, and a report that
    /// silently omits it is a report nobody can trust.
    pub trial_balance: String,
}

/// Balances, rebuilt by replaying the event log.
///
/// There is no balances table to read — the projection is derived on demand
///.
pub async fn balance(
    State(app): State<AppState>,
    ctx: TenantContext,
    Path(aggregate_id): Path<Uuid>,
) -> Result<Json<BalanceView>, ApiError> {
    ctx.require(Scope::Read)?;

    let store = app.ledger_for(ctx.tenant_id);
    let state = store.load(aggregate_id).await.map_err(storage)?;
    let event_count = store.event_count(aggregate_id).await.map_err(storage)?;

    let projection = state.inner();
    let accounts: Vec<AccountBalance> = projection
        .accounts()
        .map(|(account, money)| AccountBalance {
            account: account.as_uuid(),
            balance: money.amount().to_string(),
            currency: money.currency().code().to_owned(),
        })
        .collect();

    Ok(Json(BalanceView {
        aggregate_id,
        entry_count: projection.entry_count(),
        event_count,
        accounts,
        trial_balance: projection.trial_balance(Currency::Usd).amount().to_string(),
    }))
}
