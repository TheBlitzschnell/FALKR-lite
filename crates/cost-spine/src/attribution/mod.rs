//! Two-tier attribution: getting a [`Dimensions`] onto every cost record.
//!
//! Tier one is [`tags`] — provider cost-allocation tags and Kubernetes labels
//! map straight onto the spine. It is fast, exact where it works, and covers
//! most dedicated resources.
//!
//! Tier two is [`telemetry`] — for shared GPU nodes, where tags alone
//! systematically under-attribute because several workloads share one billable
//! resource (fractional GPU, MIG, time-slicing). Tags say the node belongs to a
//! cluster; they cannot say that run A used 70% of it and run B used 30%.
//!
//! There is a build gate on the gap between the two: if
//! attribution coverage falls below 80%, the telemetry path stops being
//! optional. [`crate::store::CostEventStore::attribution_coverage`] is what
//! measures it.
//!
//! [`Dimensions`]: falkr_core::Dimensions

pub mod tags;
pub mod telemetry;

/// Why a cost record could not be placed on the dimensional spine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttributionError {
    #[error("no tag mapped to the required dimension `{0}`")]
    MissingRequiredDimension(&'static str),
    #[error("tag `{key}` held `{value}`, which is not a valid {expected}")]
    UnparseableTag {
        key: String,
        value: String,
        expected: &'static str,
    },
    #[error("tags were not a JSON object")]
    TagsNotAnObject,
    #[error("telemetry attribution is not implemented yet; see the cost-spine design")]
    TelemetryUnavailable,
}
