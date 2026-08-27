//! The provider-ingestion boundary.

use chrono::{DateTime, Utc};

use crate::event::{CostEvent, ProviderKind};

/// Where a connector left off, so the next poll resumes rather than refetching.
///
/// Deliberately not just a timestamp: providers restate an open billing period,
/// so resuming purely by "everything after time T" would miss corrections to
/// rows already seen. `export_ref` lets a connector re-read a restated export in
/// full and rely on idempotent upsert to sort out what actually changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cursor {
    pub last_charge_period_end: Option<DateTime<Utc>>,
    pub last_export_ref: Option<String>,
}

impl Cursor {
    #[must_use]
    pub const fn beginning() -> Self {
        Self {
            last_charge_period_end: None,
            last_export_ref: None,
        }
    }
}

/// One untouched row as the provider delivered it, plus enough provenance to
/// trace it back.
///
/// The raw row is retained verbatim rather than parsed eagerly: when an auditor
/// asks why a number is what it is, "here is the invoice line we received" is
/// the answer, and a lossy early parse cannot produce it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawCostRecord {
    pub provider: ProviderKind,
    pub export_ref: String,
    pub billing_period: String,
    pub row: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("could not reach the provider export: {0}")]
    Transport(String),
    #[error("export {export_ref} is malformed: {detail}")]
    MalformedExport { export_ref: String, detail: String },
    #[error("credentials rejected by {provider}")]
    Unauthorized { provider: ProviderKind },
}

#[derive(Debug, thiserror::Error)]
pub enum NormalizeError {
    #[error("required FOCUS column `{0}` is missing")]
    MissingColumn(&'static str),
    #[error("column `{column}` held an unparseable value: {value}")]
    UnparseableValue { column: &'static str, value: String },
    #[error("row could not be attributed: {0}")]
    Attribution(#[from] crate::attribution::AttributionError),
    #[error(transparent)]
    CostEvent(#[from] crate::event::CostEventError),
}

/// A source of provider cost data.
///
/// `fetch_since` is I/O over the network and stays on the async executor;
/// `normalize` is pure CPU work and is deliberately synchronous, so a caller
/// batching a large export can decide for itself whether to hand the batch to
/// `spawn_blocking`.
#[async_trait::async_trait]
pub trait CostConnector: Send + Sync {
    /// Which provider this connector speaks for.
    fn provider(&self) -> ProviderKind;

    /// Pulls raw records delivered since `cursor`.
    async fn fetch_since(&self, cursor: Cursor) -> Result<Vec<RawCostRecord>, ConnectorError>;

    /// Maps one raw provider row onto the normalized FOCUS shape, attributing
    /// it to the dimensional spine in the process.
    fn normalize(&self, raw: RawCostRecord) -> Result<CostEvent, NormalizeError>;
}
