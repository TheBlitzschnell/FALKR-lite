//! Telemetry-based attribution — the fallback for shared GPU nodes.
//!
//! # Status: interface defined, implementation not written
//!
//! This tier is **deliberately unimplemented**. The trait and its
//! data shapes are here so the ingestion pipeline can be written against the
//! real boundary rather than being retrofitted later, and so the gap is visible
//! in the type system instead of living in someone's notes.
//!
//! ## Why it exists
//!
//! Tag-based attribution ([`super::tags`]) assumes one billable resource maps
//! to one workload. On shared GPU nodes that assumption breaks: with fractional
//! GPU allocation, MIG partitioning, or time-slicing, a single node carries
//! several runs at once, and its tags describe the node, not the runs. Cost
//! lands on the cluster and stops there — which is precisely the "97.5% of AI
//! cost hides under 'compute'" failure this system is built to
//! avoid.
//!
//! ## What is missing
//!
//! Implementing [`TelemetryAttributor`] for [`DcgmPrometheusAttributor`] needs,
//! in order:
//!
//! 1. **A Prometheus query client.** Range queries against `DCGM_FI_DEV_GPU_UTIL`
//!    and `DCGM_FI_DEV_FB_USED`, exported by NVIDIA DCGM Exporter, over the
//!    record's charge period. No HTTP client is wired into this crate yet.
//! 2. **A pod-to-workload join.** DCGM series carry `pod`/`namespace` labels;
//!    turning those into a `RunId` means joining against the Kubernetes labels
//!    the training job was submitted with, which is the same label set
//!    [`super::tags`] already reads.
//! 3. **A weighting policy decision.** Utilization-seconds, memory-seconds, or
//!    allocated-fraction-seconds give materially different answers for a job
//!    that reserves a whole GPU and leaves it idle. Which one is *correct* is a
//!    policy question about whether idle reserved capacity is charged to the
//!    reserver or to overhead, and that is an open question — it
//!    needs deciding before this is written, not during.
//! 4. **A residual policy.** Node cost not attributable to any pod (idle time,
//!    system daemons, the gap between allocation and utilization) has to land
//!    somewhere explicit rather than being silently dropped or silently
//!    smeared.
//!
//! The apportionment arithmetic itself is already written and tested —
//! [`crate::pricing::apportion`] conserves the total exactly under any weights,
//! so once weights exist the split is a solved problem.
//!
//! ## When it stops being optional
//!
//! There is a build gate: if the share of `CostEvent`s
//! carrying a non-null `project_id` within 24h of ingestion drops below 80%,
//! this ships before any further modules do.
//! [`crate::store::CostEventStore::attribution_coverage`] reports that number.

use chrono::{DateTime, Utc};
use falkr_core::{Money, RunId};

use super::AttributionError;

/// One workload's share of a shared node's cost.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AttributedShare {
    pub run_id: RunId,
    /// The relative weight this workload earned over the charge period, by
    /// whichever measure the attributor uses.
    pub weight: rust_decimal::Decimal,
    pub cost: Money,
}

/// The window a node's cost is being apportioned across.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChargeWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Apportions a shared node's cost across the workloads that ran on it.
#[async_trait::async_trait]
pub trait TelemetryAttributor: Send + Sync {
    /// Splits `total` across the workloads observed on `node_resource_id`
    /// during `window`.
    ///
    /// Returns an empty vector when the node genuinely ran nothing — callers
    /// must handle that rather than assuming at least one share.
    async fn apportion_node_cost(
        &self,
        node_resource_id: &str,
        window: ChargeWindow,
        total: Money,
    ) -> Result<Vec<AttributedShare>, AttributionError>;
}

/// DCGM-over-Prometheus attributor.
///
/// See the module docs: constructed and wired, but [`TelemetryAttributor`] is
/// not implemented yet.
#[derive(Debug, Clone)]
pub struct DcgmPrometheusAttributor {
    /// Base URL of the Prometheus instance scraping DCGM Exporter.
    pub prometheus_url: String,
    /// Scrape interval, needed to convert instantaneous utilization samples
    /// into utilization-seconds.
    pub scrape_interval_seconds: u32,
}

impl DcgmPrometheusAttributor {
    #[must_use]
    pub const fn new(prometheus_url: String, scrape_interval_seconds: u32) -> Self {
        Self {
            prometheus_url,
            scrape_interval_seconds,
        }
    }
}

#[async_trait::async_trait]
impl TelemetryAttributor for DcgmPrometheusAttributor {
    async fn apportion_node_cost(
        &self,
        _node_resource_id: &str,
        _window: ChargeWindow,
        _total: Money,
    ) -> Result<Vec<AttributedShare>, AttributionError> {
        // Intentionally unimplemented — see the module documentation above for
        // the four things this needs, and the cost-spine design for the coverage
        // gate that decides when it becomes mandatory. This is a `todo!()`
        // rather than an `Err` so that no caller can mistake "not built" for
        // "ran and found nothing".
        todo!(
            "DCGM/Prometheus telemetry attribution: needs a Prometheus range-query \
             client, a pod->RunId join, a weighting-policy decision \
             (utilization- vs memory- vs allocation-seconds), and a residual \
             policy for unattributable node cost. See module docs and \
             the cost-spine design."
        )
    }
}

/// A no-op attributor for environments with no telemetry wired up.
///
/// Returns an explicit error rather than an empty result, so a pipeline that
/// falls through to telemetry on a low-coverage record fails loudly instead of
/// recording the node's cost as attributable-to-nobody.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableTelemetry;

#[async_trait::async_trait]
impl TelemetryAttributor for UnavailableTelemetry {
    async fn apportion_node_cost(
        &self,
        _node_resource_id: &str,
        _window: ChargeWindow,
        _total: Money,
    ) -> Result<Vec<AttributedShare>, AttributionError> {
        Err(AttributionError::TelemetryUnavailable)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "the unwrap/expect ban targets production code, not tests"
    )]

    use chrono::TimeZone as _;
    use falkr_core::Currency;
    use rust_decimal_macros::dec;

    use super::*;

    fn window() -> ChargeWindow {
        ChargeWindow {
            start: Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap(),
            end: Utc.with_ymd_and_hms(2026, 8, 1, 1, 0, 0).unwrap(),
        }
    }

    #[tokio::test]
    async fn unavailable_telemetry_fails_loudly_rather_than_silently() {
        let result = UnavailableTelemetry
            .apportion_node_cost("i-0abc", window(), Money::new(dec!(100), Currency::Usd))
            .await;
        assert_eq!(result.unwrap_err(), AttributionError::TelemetryUnavailable);
    }
}
