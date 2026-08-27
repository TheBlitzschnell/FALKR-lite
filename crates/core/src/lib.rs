//! # falkr-core
//!
//! Domain vocabulary shared by every other crate in the workspace: the
//! dimensional spine, exact money, and the domain-ID newtypes.
//!
//! This crate depends on **nothing else in this workspace** and on no
//! persistence library. `sqlx` appears only as an optional
//! dependency behind the `sqlx` feature, which exists so `infra` can derive
//! `sqlx::Type` on the ID newtypes; it is off by default, so `core` and every
//! domain crate resolve without it.

// Also set workspace-wide in Cargo.toml; repeated here because
// `Money` arithmetic lives in this file's neighbours and the rule should be
// visible to anyone reading the crate root.
#![deny(clippy::float_arithmetic)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

pub mod dimensions;
pub mod ids;
pub mod money;

pub use dimensions::{CapitalizationStatus, Dimensions, FunctionCode};
pub use ids::{
    AccountId, CheckpointId, CommitmentId, ContractId, CostEventId, CreditPoolId, CustomerId,
    DatasetId, DatasetVersionId, EmployeeId, EntryId, ExperimentId, GrantId, JobId, LicenseId,
    ModelId, ModelVersionId, PolicyId, ProjectId, ProviderId, RunId, TeamId, TenantId,
    UsageEventId,
};
pub use money::{Currency, CurrencyMismatch, Money};
