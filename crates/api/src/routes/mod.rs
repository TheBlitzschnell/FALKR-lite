//! HTTP handlers wiring the domain crates together.

pub mod cost;
pub mod ledger;
pub mod research;

use axum::Router;
use axum::routing::get;

use crate::state::AppState;

/// The full router.
///
/// Exposed as a function rather than built inside `main` so integration tests
/// can drive it with `tower::ServiceExt::oneshot` — no socket, no port, and the
/// same routing table production uses.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/runs/{run_id}", get(research::get_run))
        .route("/v1/runs/{run_id}/cost", get(research::get_run_cost))
        .route("/v1/cost/coverage", get(cost::attribution_coverage))
        .route("/v1/ledger/{aggregate_id}/balance", get(ledger::balance))
        .with_state(state)
}

/// Liveness. Deliberately unauthenticated and touching nothing — a health check
/// that queries the database reports the database's health, not the service's,
/// and takes the service down with it during a failover.
async fn health() -> &'static str {
    "ok"
}
