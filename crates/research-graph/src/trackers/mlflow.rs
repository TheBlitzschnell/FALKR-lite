//! MLflow connector — **mocked**, deliberately.
//!
//! This crate ships one real tracker connector (W&B, see [`super::wandb`]) rather
//! than two half-finished ones. This mock exists for two reasons:
//!
//! 1. It keeps [`TrackerConnector`] honest. A trait with one implementation
//!    drifts toward that implementation's assumptions; a second one, even a
//!    mock, makes W&B-specific leakage a compile error rather than a discovery.
//! 2. It lets downstream code (the `worker` polling loop, tests of the store)
//!    be written against two sources now, so adding the real MLflow client
//!    later changes one `impl` and nothing else.
//!
//! What is missing for the real thing: an MLflow REST client
//! (`/api/2.0/mlflow/runs/search`), and a decision about how MLflow's
//! `experiment_id` maps onto our [`ExperimentId`] — MLflow experiments are
//! per-tracking-server integers, so the mapping needs a stored correspondence
//! rather than a cast.
//!
//! [`ExperimentId`]: falkr_core::ExperimentId

use crate::connector::{RawRunRecord, RunCursor, TrackerConnector, TrackerError};
use crate::entities::{Run, TrackerSource};

/// A stand-in that returns whatever runs it was seeded with.
#[derive(Debug, Clone, Default)]
pub struct MockMlflowConnector {
    runs: Vec<Run>,
}

impl MockMlflowConnector {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_run(mut self, run: Run) -> Self {
        self.runs.push(run);
        self
    }
}

#[async_trait::async_trait]
impl TrackerConnector for MockMlflowConnector {
    fn source(&self) -> TrackerSource {
        TrackerSource::Mlflow
    }

    async fn fetch_runs_since(
        &self,
        _cursor: RunCursor,
    ) -> Result<Vec<RawRunRecord>, TrackerError> {
        self.runs
            .iter()
            .map(|run| {
                serde_json::to_value(run)
                    .map(|payload| RawRunRecord {
                        source: TrackerSource::Mlflow,
                        payload,
                    })
                    .map_err(|e| TrackerError::Transport {
                        tracker: TrackerSource::Mlflow,
                        detail: e.to_string(),
                    })
            })
            .collect()
    }

    fn normalize(&self, raw: RawRunRecord) -> Result<Run, TrackerError> {
        serde_json::from_value(raw.payload).map_err(|e| TrackerError::UnusableField {
            field: "payload",
            value: e.to_string(),
        })
    }
}
