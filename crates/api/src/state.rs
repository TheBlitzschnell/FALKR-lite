//! Application state: the one place concrete infrastructure meets domain traits.
//!
//! The binary crates are the only ones permitted to do this wiring. Every
//! field here is a `dyn Trait` defined by a domain crate and
//! implemented in `infra`, so handlers depend on the trait and never on
//! Postgres.

use std::sync::Arc;

use crate::auth::TenantResolver;
use falkr_cost_spine::store::CostEventStore;
use falkr_research_graph::store::ResearchGraphStore;

/// Shared, cheaply cloneable handler state.
#[derive(Clone)]
pub struct AppState {
    pub tenants: Arc<dyn TenantResolver>,
    pub costs: Arc<dyn CostEventStore>,
    pub research: Arc<dyn ResearchGraphStore>,
    /// The ledger's store is built per tenant rather than shared:
    /// `PgLedgerEventStore` binds a tenant at construction, so one instance
    /// cannot serve two.
    pub ledger: Arc<falkr_infra::LedgerStores>,
}

impl AppState {
    /// A ledger store scoped to `tenant`.
    #[must_use]
    pub fn ledger_for(&self, tenant: falkr_core::TenantId) -> falkr_infra::PgLedgerEventStore {
        self.ledger.for_tenant(tenant)
    }
}

// No manual `FromRef<AppState> for AppState`: axum provides a blanket
// `impl<T: Clone> FromRef<T> for T`, which this would conflict with.
