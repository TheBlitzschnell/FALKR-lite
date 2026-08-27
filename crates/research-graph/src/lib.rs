//! # falkr-research-graph
//!
//! Experiment → Run → Job → Checkpoint → Model → DatasetVersion, mirroring the
//! W&B/MLflow object model so ingestion is a mapping rather than a translation
//!.
//!
//! This is what makes cost-per-run possible: a `CostEvent` from the cost spine
//! carries a `run_id` in its `Dimensions`, and this crate is what that id
//! points at.
//!
//! ## Idempotency is the load-bearing property
//!
//! Connectors upsert on [`ExternalRef`]. Both polling and webhook delivery
//! double-fire, and a duplicated `Run` double-counts every cost attributed to
//! it — so this is mandatory, not best-effort.

#![deny(clippy::float_arithmetic)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

pub mod connector;
pub mod entities;
pub mod store;
pub mod trackers;

pub use connector::{RawRunRecord, RunCursor, TrackerConnector, TrackerError};
pub use entities::{
    Checkpoint, DataSource, DatasetVersion, Experiment, ExternalRef, Job, Model, ModelVersion,
    RightsBasis, Run, RunStatus, TrackerSource,
};
pub use store::{ResearchGraphStore, ResearchStoreError, RunUpsert};
