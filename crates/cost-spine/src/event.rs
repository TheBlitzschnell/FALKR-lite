//! The FOCUS-shaped cost record.
//!
//! Field-for-field this follows the FinOps FOCUS specification rather than any
//! one provider's billing schema, which is what lets `normalize()` be a column
//! mapping instead of a bespoke translation layer per provider.

use chrono::{DateTime, Utc};
use falkr_core::{CostEventId, Currency, Dimensions, Money};

/// Which provider a cost record came from.
///
/// Distinct from `ProviderId`, which is a tenant-scoped identifier for a
/// specific billing relationship: a tenant can hold two AWS accounts with
/// different `ProviderId`s, both of `ProviderKind::Aws`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderKind {
    Aws,
    Gcp,
    Azure,
    CoreWeave,
    Oci,
    /// Internally-operated capacity — on-prem clusters, colocated GPUs.
    Internal,
}

impl ProviderKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aws => "aws",
            Self::Gcp => "gcp",
            Self::Azure => "azure",
            Self::CoreWeave => "coreweave",
            Self::Oci => "oci",
            Self::Internal => "internal",
        }
    }
}

impl ProviderKind {
    /// Parses the persisted representation.
    #[must_use]
    pub fn from_stored(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "aws" => Some(Self::Aws),
            "gcp" => Some(Self::Gcp),
            "azure" => Some(Self::Azure),
            "coreweave" => Some(Self::CoreWeave),
            "oci" => Some(Self::Oci),
            "internal" => Some(Self::Internal),
            _ => None,
        }
    }
}

impl core::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// FOCUS `ChargeCategory`.
///
/// `Purchase` is the one with special downstream handling: reserved instances,
/// committed-use discounts and capacity blocks do not post to the ledger
/// directly, they create or top up a `Commitment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeCategory {
    Usage,
    Purchase,
    Tax,
    Credit,
    Adjustment,
}

impl ChargeCategory {
    /// Whether this charge feeds the commitment engine rather than posting
    /// straight to the ledger.
    #[must_use]
    pub const fn creates_commitment(self) -> bool {
        matches!(self, Self::Purchase)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Purchase => "purchase",
            Self::Tax => "tax",
            Self::Credit => "credit",
            Self::Adjustment => "adjustment",
        }
    }
}

impl ChargeCategory {
    /// Parses the persisted representation.
    #[must_use]
    pub fn from_stored(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "usage" => Some(Self::Usage),
            "purchase" => Some(Self::Purchase),
            "tax" => Some(Self::Tax),
            "credit" => Some(Self::Credit),
            "adjustment" => Some(Self::Adjustment),
            _ => None,
        }
    }
}

/// FOCUS `ServiceCategory`, the top-level classification of what was bought.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ServiceCategory {
    AiAndMachineLearning,
    Analytics,
    BusinessApplications,
    Compute,
    Databases,
    DeveloperTools,
    Identity,
    Integration,
    InternetOfThings,
    ManagementAndGovernance,
    Media,
    Migration,
    Mobile,
    Multicloud,
    Networking,
    Security,
    Storage,
    Web,
    Other,
}

impl ServiceCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AiAndMachineLearning => "ai_and_machine_learning",
            Self::Analytics => "analytics",
            Self::BusinessApplications => "business_applications",
            Self::Compute => "compute",
            Self::Databases => "databases",
            Self::DeveloperTools => "developer_tools",
            Self::Identity => "identity",
            Self::Integration => "integration",
            Self::InternetOfThings => "internet_of_things",
            Self::ManagementAndGovernance => "management_and_governance",
            Self::Media => "media",
            Self::Migration => "migration",
            Self::Mobile => "mobile",
            Self::Multicloud => "multicloud",
            Self::Networking => "networking",
            Self::Security => "security",
            Self::Storage => "storage",
            Self::Web => "web",
            Self::Other => "other",
        }
    }

    /// Parses the persisted (snake_case) representation.
    #[must_use]
    pub fn from_stored(s: &str) -> Option<Self> {
        let candidates = [
            Self::AiAndMachineLearning,
            Self::Analytics,
            Self::BusinessApplications,
            Self::Compute,
            Self::Databases,
            Self::DeveloperTools,
            Self::Identity,
            Self::Integration,
            Self::InternetOfThings,
            Self::ManagementAndGovernance,
            Self::Media,
            Self::Migration,
            Self::Mobile,
            Self::Multicloud,
            Self::Networking,
            Self::Security,
            Self::Storage,
            Self::Web,
            Self::Other,
        ];
        let s = s.trim();
        candidates.into_iter().find(|c| c.as_str() == s)
    }

    /// Parses the FOCUS spec's human-readable value, e.g. `"AI and Machine
    /// Learning"`. Unrecognized values map to [`ServiceCategory::Other`] rather
    /// than failing: a provider inventing a new category should not stall
    /// ingestion of an otherwise valid invoice line.
    #[must_use]
    pub fn from_focus_value(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "ai and machine learning" => Self::AiAndMachineLearning,
            "analytics" => Self::Analytics,
            "business applications" => Self::BusinessApplications,
            "compute" => Self::Compute,
            "databases" => Self::Databases,
            "developer tools" => Self::DeveloperTools,
            "identity" => Self::Identity,
            "integration" => Self::Integration,
            "internet of things" => Self::InternetOfThings,
            "management and governance" => Self::ManagementAndGovernance,
            "media" => Self::Media,
            "migration" => Self::Migration,
            "mobile" => Self::Mobile,
            "multicloud" => Self::Multicloud,
            "networking" => Self::Networking,
            "security" => Self::Security,
            "storage" => Self::Storage,
            "web" => Self::Web,
            _ => Self::Other,
        }
    }
}

/// Provenance back to the raw provider invoice line, and the idempotency key.
///
/// `external_id` is the key every ingestion path upserts on.
/// `content_hash` exists because "already seen" is not the same as "unchanged":
/// AWS restates the current month's CUR repeatedly before the invoice finalizes,
/// so a re-delivered row with a different hash is a **restatement to apply**,
/// while an identical hash is a **duplicate to ignore**. Collapsing those two
/// cases into one is how a corrected invoice silently fails to land.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SourceRef {
    pub provider: ProviderKind,
    /// Stable, provider-assigned identity for this invoice line.
    pub external_id: String,
    /// Which export or manifest delivered the row, for audit.
    pub export_ref: String,
    /// Billing period the row belongs to, e.g. `2026-08`.
    pub billing_period: String,
    /// Hash of the raw row as delivered — detects restatement.
    pub content_hash: String,
    pub ingested_at: DateTime<Utc>,
}

/// A single normalized cost record.
///
/// All three amounts share one currency; [`CostEvent::new`] enforces it, so
/// downstream code can sum them without a per-row currency check.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CostEvent {
    pub id: CostEventId,
    pub billing_account_id: String,
    pub charge_period_start: DateTime<Utc>,
    pub charge_period_end: DateTime<Utc>,
    pub billed_cost: Money,
    /// Discount-applied, commitments amortized — **the** number every
    /// downstream report is built on.
    pub effective_cost: Money,
    pub list_cost: Money,
    pub service_name: String,
    pub service_category: ServiceCategory,
    pub charge_category: ChargeCategory,
    pub resource_id: Option<String>,
    pub region_id: Option<String>,
    pub tags: serde_json::Value,
    pub dims: Dimensions,
    pub source_ref: SourceRef,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CostEventError {
    #[error(
        "cost amounts must share one currency: billed={billed}, effective={effective}, list={list}"
    )]
    MixedCurrency {
        billed: Currency,
        effective: Currency,
        list: Currency,
    },
    #[error("charge period ends at or before it starts: {start} -> {end}")]
    InvalidChargePeriod {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
}

impl CostEvent {
    /// Builds a cost event, rejecting the two shapes that would corrupt
    /// downstream aggregation: mixed currencies across the three amounts, and a
    /// non-positive charge period (which would break time-window attribution
    /// and any proration derived from it).
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the FOCUS record shape from the cost-spine design; a \
                  parameter struct here would just be this struct again"
    )]
    pub fn new(
        billing_account_id: String,
        charge_period_start: DateTime<Utc>,
        charge_period_end: DateTime<Utc>,
        billed_cost: Money,
        effective_cost: Money,
        list_cost: Money,
        service_name: String,
        service_category: ServiceCategory,
        charge_category: ChargeCategory,
        resource_id: Option<String>,
        region_id: Option<String>,
        tags: serde_json::Value,
        dims: Dimensions,
        source_ref: SourceRef,
    ) -> Result<Self, CostEventError> {
        let currency = billed_cost.currency();
        if effective_cost.currency() != currency || list_cost.currency() != currency {
            return Err(CostEventError::MixedCurrency {
                billed: currency,
                effective: effective_cost.currency(),
                list: list_cost.currency(),
            });
        }
        if charge_period_end <= charge_period_start {
            return Err(CostEventError::InvalidChargePeriod {
                start: charge_period_start,
                end: charge_period_end,
            });
        }

        Ok(Self {
            id: CostEventId::new(),
            billing_account_id,
            charge_period_start,
            charge_period_end,
            billed_cost,
            effective_cost,
            list_cost,
            service_name,
            service_category,
            charge_category,
            resource_id,
            region_id,
            tags,
            dims,
            source_ref,
        })
    }

    /// The single currency shared by all three amounts.
    #[must_use]
    pub fn currency(&self) -> Currency {
        self.billed_cost.currency()
    }

    /// The idempotency key this record upserts on.
    #[must_use]
    pub fn idempotency_key(&self) -> (&str, ProviderKind) {
        (&self.source_ref.external_id, self.source_ref.provider)
    }
}
