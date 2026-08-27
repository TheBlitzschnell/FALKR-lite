//! Persistence boundary for cost events.
//!
//! This crate defines the trait; `infra` implements it against Postgres
//!. Nothing here knows that Postgres exists.

use chrono::{DateTime, Utc};
use falkr_core::TenantId;
use rust_decimal::Decimal;

use crate::event::{CostEvent, ProviderKind};

/// What an idempotent upsert actually did.
///
/// The three cases are distinguished on purpose. `inserted` and `restated` are
/// both real changes but mean different things to an auditor — a restatement is
/// the provider correcting an invoice line already recorded — and `duplicate`
/// is the double-delivery case that the ingestion-idempotency rule exists to make harmless.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UpsertOutcome {
    pub inserted: usize,
    pub restated: usize,
    pub duplicate: usize,
}

impl UpsertOutcome {
    #[must_use]
    pub const fn total(&self) -> usize {
        self.inserted + self.restated + self.duplicate
    }
}

/// Attribution coverage over a window — the number behind the 80% build gate in
/// the cost-spine design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Coverage {
    pub total_events: i64,
    pub run_attributed: i64,
}

impl Coverage {
    /// Share of events carrying a `run_id`, as a fraction in `[0, 1]`.
    ///
    /// An empty window reports full coverage rather than zero: no data is not
    /// the same as badly attributed data, and reporting 0% would trip the gate
    /// on any quiet period.
    #[must_use]
    pub fn fraction(&self) -> Decimal {
        if self.total_events == 0 {
            return Decimal::ONE;
        }
        Decimal::from(self.run_attributed) / Decimal::from(self.total_events)
    }

    /// Whether coverage has fallen below the threshold that makes the telemetry
    /// path mandatory.
    #[must_use]
    pub fn below_build_gate(&self) -> bool {
        self.fraction() < Decimal::new(8, 1)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("storage backend failed: {0}")]
    Backend(String),
    #[error("tenant context was not set on the connection; RLS would silently return nothing")]
    MissingTenantContext,
}

/// Persistence for normalized cost events.
#[async_trait::async_trait]
pub trait CostEventStore: Send + Sync {
    /// Upserts a batch, keyed on `(tenant, provider, external_id)`.
    ///
    /// Must be idempotent: re-delivering the same rows
    /// produces the same state, and a row whose content changed is applied as a
    /// restatement rather than ignored or duplicated.
    async fn upsert(&self, events: &[CostEvent]) -> Result<UpsertOutcome, StoreError>;

    /// Fetches one event by its provider-assigned identity.
    async fn find_by_external_id(
        &self,
        tenant_id: TenantId,
        provider: ProviderKind,
        external_id: &str,
    ) -> Result<Option<CostEvent>, StoreError>;

    /// Measures attribution coverage since `since`.
    async fn attribution_coverage(
        &self,
        tenant_id: TenantId,
        since: DateTime<Utc>,
    ) -> Result<Coverage, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_window_does_not_trip_the_build_gate() {
        let c = Coverage {
            total_events: 0,
            run_attributed: 0,
        };
        assert!(!c.below_build_gate());
        assert_eq!(c.fraction(), Decimal::ONE);
    }

    #[test]
    fn coverage_gate_trips_below_eighty_percent() {
        let under = Coverage {
            total_events: 100,
            run_attributed: 79,
        };
        let at = Coverage {
            total_events: 100,
            run_attributed: 80,
        };
        assert!(under.below_build_gate());
        assert!(!at.below_build_gate());
    }
}
