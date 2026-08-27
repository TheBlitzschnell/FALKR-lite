//! The research object graph.
//!
//! Mirrors the W&B / MLflow object model deliberately, so ingestion is a
//! mapping rather than a translation layer. Where this shape and a tracker's
//! shape disagree, the tracker usually wins — a model that fights its own data
//! source costs more than it saves.

use chrono::{DateTime, Utc};
use falkr_core::{
    CapitalizationStatus, CheckpointId, DatasetVersionId, ExperimentId, FunctionCode, JobId,
    LicenseId, ModelId, ModelVersionId, Money, ProjectId, RunId, TenantId,
};

/// Which experiment tracker a record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TrackerSource {
    Wandb,
    Mlflow,
}

impl TrackerSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wandb => "wandb",
            Self::Mlflow => "mlflow",
        }
    }

    #[must_use]
    pub fn from_stored(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "wandb" => Some(Self::Wandb),
            "mlflow" => Some(Self::Mlflow),
            _ => None,
        }
    }
}

impl core::fmt::Display for TrackerSource {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The ingestion idempotency key.
///
/// `(source, external_id)` is what a tracker calls this run, and it is what
/// every upsert keys on. Both polling and webhook delivery double-fire, and a
/// duplicated `Run` double-counts cost — so this is not a convenience, it is
/// the mechanism that makes re-delivery harmless.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ExternalRef {
    pub source: TrackerSource,
    pub external_id: String,
}

impl ExternalRef {
    #[must_use]
    pub fn new(source: TrackerSource, external_id: impl Into<String>) -> Self {
        Self {
            source,
            external_id: external_id.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunStatus {
    Running,
    Finished,
    Failed,
    Crashed,
    Killed,
}

impl RunStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::Crashed => "crashed",
            Self::Killed => "killed",
        }
    }

    #[must_use]
    pub fn from_stored(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "running" => Some(Self::Running),
            "finished" => Some(Self::Finished),
            "failed" => Some(Self::Failed),
            "crashed" => Some(Self::Crashed),
            "killed" => Some(Self::Killed),
            _ => None,
        }
    }

    /// Whether the run has stopped, by any route.
    ///
    /// Matters for cost attribution: a terminal run's charge window is closed,
    /// so its attributed cost should stop moving.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Experiment {
    pub id: ExperimentId,
    pub tenant_id: TenantId,
    pub project_id: ProjectId,
    pub name: String,
}

/// A single training or evaluation run.
///
/// `attributed_cost` is a projection — the sum of `CostEvent`s whose
/// `dims.run_id` is this run — not an authored figure. It is stored so
/// cost-per-run is one read rather than an aggregate over the whole cost table,
/// and it is recomputed from the cost spine, never edited.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Run {
    pub id: RunId,
    pub tenant_id: TenantId,
    pub experiment_id: ExperimentId,
    pub external_ref: ExternalRef,
    pub status: RunStatus,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub attributed_cost: Money,
    pub function_code: FunctionCode,
    pub capitalization_status: CapitalizationStatus,
}

/// A unit of scheduled work within a run — one worker, one sweep trial, one
/// distributed rank. Kept distinct from `Run` because cost attribution often
/// lands here (a pod is a job, not a run) and rolls up.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Job {
    pub id: JobId,
    pub tenant_id: TenantId,
    pub run_id: RunId,
    pub external_ref: Option<ExternalRef>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    /// e.g. the Kubernetes pod or Slurm job that executed this.
    pub resource_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub tenant_id: TenantId,
    pub run_id: RunId,
    pub storage_cost: Money,
    pub storage_uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelVersion {
    pub id: ModelVersionId,
    pub model_id: ModelId,
    pub version: String,
    /// The run that produced this version, when it came from one.
    pub produced_by_run: Option<RunId>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Model {
    pub id: ModelId,
    pub tenant_id: TenantId,
    pub name: String,
    pub versions: Vec<ModelVersion>,
    /// Links to the compliance registry.
    pub license_id: Option<LicenseId>,
}

/// Where training data came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DataSource {
    Licensed,
    Scraped,
    UserContributed,
    Synthetic,
    PubliclyAvailable,
    Purchased,
}

/// The legal basis for using a dataset.
///
/// Recorded per dataset version because downstream disclosure obligations ask
/// for it, and "we think it was fine" is not an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RightsBasis {
    OwnedOutright,
    LicensedForTraining,
    ConsentGiven,
    PublicDomain,
    FairUseAsserted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DatasetVersion {
    pub id: DatasetVersionId,
    pub tenant_id: TenantId,
    pub source: DataSource,
    pub rights_basis: RightsBasis,
    pub acquisition_cost: Option<Money>,
}
