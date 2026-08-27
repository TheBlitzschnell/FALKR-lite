//! Persistence boundary for the research graph.
//!
//! This crate defines the traits; `infra` implements them against Postgres.

use falkr_core::{Money, RunId, TenantId};

use crate::entities::{Experiment, ExternalRef, Run};

/// What an idempotent run upsert did.
///
/// `Updated` covers the ordinary case of a tracker reporting progress on a run
/// it already told us about — a status change from `running` to `finished`, an
/// `ended_at` appearing. `Unchanged` is the pure double-delivery case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RunUpsert {
    Inserted(RunId),
    Updated(RunId),
    Unchanged(RunId),
}

impl RunUpsert {
    #[must_use]
    pub const fn run_id(self) -> RunId {
        match self {
            Self::Inserted(id) | Self::Updated(id) | Self::Unchanged(id) => id,
        }
    }

    /// Whether this upsert created a new run.
    ///
    /// The property every duplicate-delivery test asserts: two deliveries of
    /// the same tracker record produce exactly one `Inserted`.
    #[must_use]
    pub const fn is_new(self) -> bool {
        matches!(self, Self::Inserted(_))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResearchStoreError {
    #[error("storage backend failed: {0}")]
    Backend(String),
    #[error("experiment {0} does not exist")]
    UnknownExperiment(String),
}

/// Persistence for the research object graph.
#[async_trait::async_trait]
pub trait ResearchGraphStore: Send + Sync {
    async fn upsert_experiment(&self, experiment: &Experiment) -> Result<(), ResearchStoreError>;

    /// Upserts a run, keyed on `(tenant_id, external_ref)`.
    ///
    /// **Must be idempotent.** A tracker webhook and a poll can deliver the same
    /// run within the same second; two `Run` rows for one training job would
    /// double-count every `CostEvent` attributed to it.
    async fn upsert_run(&self, run: &Run) -> Result<RunUpsert, ResearchStoreError>;

    async fn find_run_by_external_ref(
        &self,
        tenant_id: TenantId,
        external_ref: &ExternalRef,
    ) -> Result<Option<Run>, ResearchStoreError>;

    async fn find_run(
        &self,
        tenant_id: TenantId,
        run_id: RunId,
    ) -> Result<Option<Run>, ResearchStoreError>;

    /// Recomputes a run's attributed cost from the cost spine and stores it.
    ///
    /// A projection, recomputed from `cost_events`, never authored: this is the
    /// number that makes "what did this training run cost" a single read.
    async fn refresh_attributed_cost(
        &self,
        tenant_id: TenantId,
        run_id: RunId,
    ) -> Result<Money, ResearchStoreError>;
}
