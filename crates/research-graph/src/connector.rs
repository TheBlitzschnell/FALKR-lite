//! Experiment-tracker ingestion.

use chrono::{DateTime, Utc};

use crate::entities::{ExternalRef, Run, TrackerSource};

/// Where a tracker poll left off.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RunCursor {
    /// Resume *at* this timestamp, not after it. A run updated in the same
    /// second as the last poll would otherwise be missed, and re-reading it is
    /// free because upsert is idempotent.
    pub updated_since: Option<DateTime<Utc>>,
}

/// One untouched tracker record, retained verbatim for audit.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawRunRecord {
    pub source: TrackerSource,
    pub payload: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum TrackerError {
    // Field is `tracker`, not `source`: thiserror treats a field named
    // `source` as the underlying error cause and tries to derive
    // `Error::source()` from it.
    #[error("could not reach {tracker}: {detail}")]
    Transport {
        tracker: TrackerSource,
        detail: String,
    },
    #[error("credentials rejected by {0}")]
    Unauthorized(TrackerSource),
    #[error("required field `{0}` is missing from the tracker payload")]
    MissingField(&'static str),
    #[error("field `{field}` held an unusable value: {value}")]
    UnusableField { field: &'static str, value: String },
}

/// Pulls runs from an experiment tracker and maps them onto the research graph.
#[async_trait::async_trait]
pub trait TrackerConnector: Send + Sync {
    fn source(&self) -> TrackerSource;

    /// Fetches records updated at or after `cursor`.
    async fn fetch_runs_since(&self, cursor: RunCursor) -> Result<Vec<RawRunRecord>, TrackerError>;

    /// Maps one tracker record onto a [`Run`].
    ///
    /// Synchronous and pure: the resulting `Run` carries an [`ExternalRef`],
    /// and it is the *store* that decides whether that ref is new or already
    /// known. Normalization never queries the database, so it cannot make the
    /// idempotency decision by accident.
    fn normalize(&self, raw: RawRunRecord) -> Result<Run, TrackerError>;
}

/// The idempotency key a normalized run will upsert on.
#[must_use]
pub fn idempotency_key(run: &Run) -> &ExternalRef {
    &run.external_ref
}
