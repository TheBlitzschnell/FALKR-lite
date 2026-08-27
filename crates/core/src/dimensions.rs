//! The dimensional spine.
//!
//! Every row in `ledger_events`, `billing_events` and `cost_events` carries a
//! [`Dimensions`], fully populated **at write time**. A `None` on an optional
//! field means "not applicable to this transaction" — it never means "not yet
//! known". Backfilling dimensions later means re-deriving them from provider
//! invoices after the fact, which is exactly the failure this design exists to
//! prevent.

use crate::ids::{
    CommitmentId, CustomerId, DatasetId, ModelId, ProjectId, ProviderId, RunId, TeamId, TenantId,
};

/// Where a cost lands in the income statement.
///
/// Assigned at posting time by rule, never by hand: production-inference
/// compute is `Cogs`, training and experimentation is `RnD` (ASC 730, expensed
/// by default), internal-use development is `OpEx`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum FunctionCode {
    Cogs,
    RnD,
    OpEx,
}

impl FunctionCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cogs => "COGS",
            Self::RnD => "RND",
            Self::OpEx => "OPEX",
        }
    }

    /// Parses the representation written by [`FunctionCode::as_str`].
    ///
    /// `None` for anything unrecognized —
    /// a function code is what decides whether a cost hits gross margin or R&D,
    /// so guessing is not an option.
    #[must_use]
    pub fn from_stored(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "COGS" => Some(Self::Cogs),
            "RND" => Some(Self::RnD),
            "OPEX" => Some(Self::OpEx),
            _ => None,
        }
    }
}

impl core::fmt::Display for FunctionCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a run's cost is expensed as incurred or capitalized as an asset.
///
/// Which one applies is a **configurable policy decision**, never a hardcoded
/// rule: ASC 350-40 is actively moving — ASU 2025-06 changes
/// the test itself, effective FY2028 — so this records the *outcome* of
/// evaluating a `CapitalizationPolicy`, not a rule baked into Rust.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum CapitalizationStatus {
    /// ASC 730 default for research and experimentation.
    Expensed,
    Capitalized,
    /// Awaiting evaluation against the applicable policy.
    PendingReview,
}

impl CapitalizationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Expensed => "EXPENSED",
            Self::Capitalized => "CAPITALIZED",
            Self::PendingReview => "PENDING_REVIEW",
        }
    }

    #[must_use]
    pub fn from_stored(s: &str) -> Option<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "EXPENSED" => Some(Self::Expensed),
            "CAPITALIZED" => Some(Self::Capitalized),
            "PENDING_REVIEW" => Some(Self::PendingReview),
            _ => None,
        }
    }
}

/// The full dimensional spine carried by every financial transaction.
///
/// Field order matches the dimensional spine exactly. Deliberately not
/// `#[non_exhaustive]` and deliberately without a `Default`: per the project conventions the
/// spine is default-frozen, changing it is a considered decision, and a
/// defaulted `Dimensions` would invent a tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Dimensions {
    pub tenant_id: TenantId,
    /// Populated for training/experiment-attributed cost.
    pub project_id: ProjectId,
    pub run_id: Option<RunId>,
    pub team_id: TeamId,
    /// Populated for customer-funded or customer-billed activity.
    pub customer_id: Option<CustomerId>,
    pub model_id: Option<ModelId>,
    pub dataset_id: Option<DatasetId>,
    /// AWS, GCP, CoreWeave, internal, etc.
    pub provider_id: ProviderId,
    pub function_code: FunctionCode,
    pub commitment_id: Option<CommitmentId>,
}

impl Dimensions {
    /// Constructs a spine from the five fields that are never optional.
    ///
    /// The optional dimensions are attached with the `with_*` builders. This
    /// shape is what makes "fully populated at write time" the path of least
    /// resistance: you cannot get a `Dimensions` without deciding the tenant,
    /// project, team, provider and function code.
    #[must_use]
    pub const fn new(
        tenant_id: TenantId,
        project_id: ProjectId,
        team_id: TeamId,
        provider_id: ProviderId,
        function_code: FunctionCode,
    ) -> Self {
        Self {
            tenant_id,
            project_id,
            run_id: None,
            team_id,
            customer_id: None,
            model_id: None,
            dataset_id: None,
            provider_id,
            function_code,
            commitment_id: None,
        }
    }

    #[must_use]
    pub const fn with_run(mut self, run_id: RunId) -> Self {
        self.run_id = Some(run_id);
        self
    }

    #[must_use]
    pub const fn with_customer(mut self, customer_id: CustomerId) -> Self {
        self.customer_id = Some(customer_id);
        self
    }

    #[must_use]
    pub const fn with_model(mut self, model_id: ModelId) -> Self {
        self.model_id = Some(model_id);
        self
    }

    #[must_use]
    pub const fn with_dataset(mut self, dataset_id: DatasetId) -> Self {
        self.dataset_id = Some(dataset_id);
        self
    }

    #[must_use]
    pub const fn with_commitment(mut self, commitment_id: CommitmentId) -> Self {
        self.commitment_id = Some(commitment_id);
        self
    }

    /// Whether this transaction is attributed to a specific training or
    /// inference run.
    ///
    /// Attribution coverage — the share of `CostEvent`s where this is true
    /// within 24h of ingestion — is the metric behind the 80% build gate in
    /// the gate that decides whether the telemetry fallback path is
    /// still optional.
    #[must_use]
    pub const fn is_run_attributed(&self) -> bool {
        self.run_id.is_some()
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        reason = "the expect/unwrap ban targets production code, not tests"
    )]

    use super::*;

    fn spine() -> Dimensions {
        Dimensions::new(
            TenantId::new(),
            ProjectId::new(),
            TeamId::new(),
            ProviderId::new(),
            FunctionCode::RnD,
        )
    }

    #[test]
    fn required_fields_are_set_and_optional_ones_start_unset() {
        let d = spine();
        assert_eq!(d.function_code, FunctionCode::RnD);
        assert!(d.run_id.is_none());
        assert!(d.customer_id.is_none());
        assert!(d.model_id.is_none());
        assert!(d.dataset_id.is_none());
        assert!(d.commitment_id.is_none());
        assert!(!d.is_run_attributed());
    }

    #[test]
    fn builders_attach_optional_dimensions() {
        let run = RunId::new();
        let customer = CustomerId::new();
        let d = spine().with_run(run).with_customer(customer);
        assert_eq!(d.run_id, Some(run));
        assert_eq!(d.customer_id, Some(customer));
        assert!(d.is_run_attributed());
    }

    #[test]
    fn survives_a_serde_round_trip() {
        // Dimensions are persisted alongside every event; a silent shape change
        // here would be a data-migration problem, so pin the round trip.
        let d = spine().with_model(ModelId::new());
        let json = serde_json::to_string(&d).expect("serialize");
        let back: Dimensions = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(d, back);
    }
}
