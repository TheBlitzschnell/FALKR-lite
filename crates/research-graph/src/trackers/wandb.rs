//! Weights & Biases connector.
//!
//! Maps the W&B run object onto [`Run`]. The mapping decisions worth knowing:
//!
//! - **Identity is `(entity, project, run_id)`, not `run_id` alone.** W&B run
//!   ids are only unique within a project, so keying on the bare id would
//!   collide across projects in the same tenant and silently merge two training
//!   runs into one.
//! - **`state` maps to [`RunStatus`] with no default.** An unrecognized state
//!   is an error, not a guess — a run wrongly recorded as `finished` stops
//!   accruing attributed cost.
//! - **A run with no `heartbeatAt` and a terminal state still needs an
//!   `ended_at`.** W&B omits it in some crash paths; we fall back to the last
//!   heartbeat, and leave it `None` rather than inventing `now()`.

use chrono::{DateTime, Utc};
use falkr_core::{
    CapitalizationStatus, Currency, ExperimentId, FunctionCode, Money, RunId, TenantId,
};

use crate::connector::{RawRunRecord, RunCursor, TrackerConnector, TrackerError};
use crate::entities::{ExternalRef, Run, RunStatus, TrackerSource};

/// Source of W&B run records.
///
/// Abstracted so the mapping can be exercised against real payload shapes
/// without network access or an API key. The HTTP-backed implementation lands
/// alongside a background polling loop.
#[async_trait::async_trait]
pub trait WandbApi: Send + Sync {
    /// Returns raw run objects updated at or after the cursor.
    async fn runs_since(&self, cursor: &RunCursor) -> Result<Vec<serde_json::Value>, TrackerError>;
}

/// An in-memory W&B API, for tests and for replaying a captured response.
#[derive(Debug, Clone, Default)]
pub struct InMemoryWandbApi {
    runs: Vec<serde_json::Value>,
}

impl InMemoryWandbApi {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_run(mut self, run: serde_json::Value) -> Self {
        self.runs.push(run);
        self
    }
}

#[async_trait::async_trait]
impl WandbApi for InMemoryWandbApi {
    async fn runs_since(
        &self,
        _cursor: &RunCursor,
    ) -> Result<Vec<serde_json::Value>, TrackerError> {
        Ok(self.runs.clone())
    }
}

/// Maps W&B runs into the research graph.
pub struct WandbConnector<A: WandbApi> {
    api: A,
    tenant_id: TenantId,
    /// The experiment a fetched run belongs to.
    ///
    /// W&B's "project" is the closest analogue to an [`Experiment`]; resolving
    /// a W&B project name to an `ExperimentId` is a lookup the caller owns,
    /// because it depends on how a tenant has chosen to organize its projects.
    ///
    /// [`Experiment`]: crate::entities::Experiment
    experiment_id: ExperimentId,
    /// Currency the tenant's costs are denominated in.
    currency: Currency,
}

impl<A: WandbApi> WandbConnector<A> {
    #[must_use]
    pub const fn new(
        api: A,
        tenant_id: TenantId,
        experiment_id: ExperimentId,
        currency: Currency,
    ) -> Self {
        Self {
            api,
            tenant_id,
            experiment_id,
            currency,
        }
    }
}

fn str_field<'a>(v: &'a serde_json::Value, name: &'static str) -> Result<&'a str, TrackerError> {
    v.get(name)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(TrackerError::MissingField(name))
}

fn optional_str<'a>(v: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    v.get(name)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn timestamp(value: &str, field: &'static str) -> Result<DateTime<Utc>, TrackerError> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| TrackerError::UnusableField {
            field,
            value: value.to_owned(),
        })
}

/// Builds the identity used for idempotent upsert.
///
/// See the module docs: W&B run ids are unique per project, not globally.
fn external_id(run: &serde_json::Value) -> Result<String, TrackerError> {
    let entity = str_field(run, "entity")?;
    let project = str_field(run, "project")?;
    let id = str_field(run, "id")?;
    Ok(format!("{entity}/{project}/{id}"))
}

#[async_trait::async_trait]
impl<A: WandbApi> TrackerConnector for WandbConnector<A> {
    fn source(&self) -> TrackerSource {
        TrackerSource::Wandb
    }

    async fn fetch_runs_since(&self, cursor: RunCursor) -> Result<Vec<RawRunRecord>, TrackerError> {
        Ok(self
            .api
            .runs_since(&cursor)
            .await?
            .into_iter()
            .map(|payload| RawRunRecord {
                source: TrackerSource::Wandb,
                payload,
            })
            .collect())
    }

    fn normalize(&self, raw: RawRunRecord) -> Result<Run, TrackerError> {
        let p = &raw.payload;

        let state = str_field(p, "state")?;
        let status = RunStatus::from_stored(state).ok_or_else(|| TrackerError::UnusableField {
            field: "state",
            value: state.to_owned(),
        })?;

        let started_at = timestamp(str_field(p, "createdAt")?, "createdAt")?;
        let heartbeat = optional_str(p, "heartbeatAt")
            .map(|s| timestamp(s, "heartbeatAt"))
            .transpose()?;
        // A terminal run's end is its last heartbeat when W&B omits an explicit
        // one. Left `None` for a still-running run rather than defaulted, so a
        // half-open charge window stays visibly half-open.
        let ended_at = match optional_str(p, "endedAt") {
            Some(s) => Some(timestamp(s, "endedAt")?),
            None if status.is_terminal() => heartbeat,
            None => None,
        };

        // Training and experimentation is R&D by default (ASC 730). Whether a
        // specific run is later capitalized is a policy evaluation, recorded
        // through the ledger's CapitalizationStatusChanged event — never
        // decided here.
        Ok(Run {
            id: RunId::new(),
            tenant_id: self.tenant_id,
            experiment_id: self.experiment_id,
            external_ref: ExternalRef::new(TrackerSource::Wandb, external_id(p)?),
            status,
            started_at,
            ended_at,
            attributed_cost: Money::zero(self.currency),
            function_code: FunctionCode::RnD,
            capitalization_status: CapitalizationStatus::PendingReview,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "the unwrap/expect ban targets production code, not tests"
    )]

    use serde_json::json;

    use super::*;

    fn connector() -> WandbConnector<InMemoryWandbApi> {
        WandbConnector::new(
            InMemoryWandbApi::new(),
            TenantId::new(),
            ExperimentId::new(),
            Currency::Usd,
        )
    }

    fn record(payload: serde_json::Value) -> RawRunRecord {
        RawRunRecord {
            source: TrackerSource::Wandb,
            payload,
        }
    }

    #[test]
    fn maps_a_finished_run() {
        let run = connector()
            .normalize(record(json!({
                "id": "abc123",
                "entity": "acme-ai",
                "project": "llm-pretrain",
                "state": "finished",
                "createdAt": "2026-08-01T00:00:00Z",
                "heartbeatAt": "2026-08-01T06:30:00Z",
            })))
            .unwrap();

        assert_eq!(run.status, RunStatus::Finished);
        assert_eq!(run.external_ref.external_id, "acme-ai/llm-pretrain/abc123");
        assert_eq!(run.external_ref.source, TrackerSource::Wandb);
        assert_eq!(run.function_code, FunctionCode::RnD);
        assert_eq!(
            run.capitalization_status,
            CapitalizationStatus::PendingReview
        );
        assert!(
            run.ended_at.is_some(),
            "terminal run falls back to heartbeat"
        );
    }

    #[test]
    fn a_running_run_has_no_end_time() {
        let run = connector()
            .normalize(record(json!({
                "id": "abc123",
                "entity": "acme-ai",
                "project": "llm-pretrain",
                "state": "running",
                "createdAt": "2026-08-01T00:00:00Z",
                "heartbeatAt": "2026-08-01T06:30:00Z",
            })))
            .unwrap();
        assert_eq!(run.ended_at, None, "an open charge window stays open");
        assert!(!run.status.is_terminal());
    }

    #[test]
    fn identity_includes_entity_and_project_not_just_run_id() {
        // Same run id, different W&B project — these are two distinct runs and
        // must not collide on upsert.
        let c = connector();
        let a = c
            .normalize(record(json!({
                "id": "dup", "entity": "acme-ai", "project": "pretrain",
                "state": "finished", "createdAt": "2026-08-01T00:00:00Z",
            })))
            .unwrap();
        let b = c
            .normalize(record(json!({
                "id": "dup", "entity": "acme-ai", "project": "finetune",
                "state": "finished", "createdAt": "2026-08-01T00:00:00Z",
            })))
            .unwrap();
        assert_ne!(a.external_ref, b.external_ref);
    }

    #[test]
    fn an_unknown_state_is_an_error_not_a_guess() {
        let err = connector()
            .normalize(record(json!({
                "id": "abc", "entity": "e", "project": "p",
                "state": "preempted", "createdAt": "2026-08-01T00:00:00Z",
            })))
            .unwrap_err();
        assert!(matches!(
            err,
            TrackerError::UnusableField { field: "state", .. }
        ));
    }

    #[test]
    fn a_missing_required_field_is_reported_by_name() {
        let err = connector()
            .normalize(record(json!({
                "id": "abc",
                "project": "p",
                "state": "finished",
                "createdAt": "2026-08-01T00:00:00Z",
                // "entity" deliberately absent
            })))
            .unwrap_err();
        assert!(matches!(err, TrackerError::MissingField("entity")));
    }

    #[tokio::test]
    async fn repeated_fetches_produce_the_same_external_ref() {
        // Two deliveries of the same run must key to the same identity — the
        // store's upsert is what makes them one row, and it can only do that if
        // the key is stable.
        let payload = json!({
            "id": "abc123", "entity": "acme-ai", "project": "llm-pretrain",
            "state": "running", "createdAt": "2026-08-01T00:00:00Z",
        });
        let c = WandbConnector::new(
            InMemoryWandbApi::new().with_run(payload),
            TenantId::new(),
            ExperimentId::new(),
            Currency::Usd,
        );

        let first = c.fetch_runs_since(RunCursor::default()).await.unwrap();
        let second = c.fetch_runs_since(RunCursor::default()).await.unwrap();
        let a = c.normalize(first[0].clone()).unwrap();
        let b = c.normalize(second[0].clone()).unwrap();

        assert_eq!(a.external_ref, b.external_ref);
        // The surrogate ids differ — identity comes from the tracker, not from
        // us, which is exactly why upsert keys on external_ref.
        assert_ne!(a.id, b.id);
    }
}
