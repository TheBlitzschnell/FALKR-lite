//! # falkr-cost-spine
//!
//! FOCUS-normalized cost and usage ingestion, and the attribution that puts a
//! [`Dimensions`] on every cost record.
//!
//! This is the first module in the build order because nothing downstream has
//! anything to attribute to without it: the ledger cannot tag a transaction
//! with dimensions that do not exist yet.
//!
//! ## Shape
//!
//! ```text
//! provider export ──▶ CostConnector::fetch_since ──▶ RawCostRecord
//!                                                        │
//!                              CostConnector::normalize ─┤
//!                                                        ▼
//!                         attribution::tags ──▶ Dimensions ──▶ CostEvent
//!                              (fallback: attribution::telemetry)
//!                                                        │
//!                                CostEventStore::upsert ─┘  (idempotent)
//! ```
//!
//! ## Invariants this crate is responsible for
//!
//! - Every emitted [`CostEvent`] carries a fully populated [`Dimensions`] —
//!   never backfilled.
//! - Ingestion is idempotent on the provider's row identity, and distinguishes
//!   a restatement from a duplicate.
//! - No money touches a float, and apportionment conserves totals exactly.
//!
//! [`Dimensions`]: falkr_core::Dimensions

// `effective_cost` arithmetic lives here, so the float ban applies
// with the same force as it does in `ledger`.
#![deny(clippy::float_arithmetic)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

pub mod attribution;
pub mod connector;
pub mod event;
pub mod pricing;
pub mod providers;
pub mod store;

pub use connector::{ConnectorError, CostConnector, Cursor, NormalizeError, RawCostRecord};
pub use event::{
    ChargeCategory, CostEvent, CostEventError, ProviderKind, ServiceCategory, SourceRef,
};
pub use pricing::{
    CommitmentCoverage, CostAmounts, Discount, PricingError, PricingInputs, apportion,
};
pub use store::{CostEventStore, Coverage, StoreError, UpsertOutcome};
