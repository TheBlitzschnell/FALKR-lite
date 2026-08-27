//! Tag-based attribution — the fast path.
//!
//! Provider cost-allocation tags and Kubernetes labels are read straight into
//! the dimensional spine. Which tag key carries which dimension is configuration
//! rather than a constant, because the key an organization already tags with
//! (`team`, `cost-center`, `wandb_run`) is whatever it happens to be, and
//! forcing a rename across an existing estate is how a rollout stalls.

use falkr_core::{
    CommitmentId, CustomerId, DatasetId, Dimensions, FunctionCode, ModelId, ProjectId, ProviderId,
    RunId, TeamId, TenantId,
};
use uuid::Uuid;

use super::AttributionError;

/// Which tag keys carry which dimensions, in priority order.
///
/// Each field holds candidate keys tried left to right, so an organization
/// migrating from `team` to `falkr:team` can list both during the transition
/// without a flag day.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TagAttributionRules {
    pub project_keys: Vec<String>,
    pub run_keys: Vec<String>,
    pub team_keys: Vec<String>,
    pub customer_keys: Vec<String>,
    pub model_keys: Vec<String>,
    pub dataset_keys: Vec<String>,
    pub commitment_keys: Vec<String>,
    /// Tag keys whose value names the workload's purpose, used to derive a
    /// [`FunctionCode`] when no explicit rule matches.
    pub workload_keys: Vec<String>,
}

impl Default for TagAttributionRules {
    /// Conventional defaults covering the `falkr:` namespace, the bare keys most
    /// teams already use, and the labels W&B and MLflow set on Kubernetes pods.
    fn default() -> Self {
        let keys = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        Self {
            project_keys: keys(&["falkr:project", "project", "project_id"]),
            run_keys: keys(&[
                "falkr:run",
                "run",
                "run_id",
                "wandb_run_id",
                "mlflow_run_id",
            ]),
            team_keys: keys(&["falkr:team", "team", "team_id", "cost-center"]),
            customer_keys: keys(&["falkr:customer", "customer", "customer_id"]),
            model_keys: keys(&["falkr:model", "model", "model_id"]),
            dataset_keys: keys(&["falkr:dataset", "dataset", "dataset_id"]),
            commitment_keys: keys(&["falkr:commitment", "commitment_id"]),
            workload_keys: keys(&["falkr:workload", "workload", "workload_type", "purpose"]),
        }
    }
}

/// Values that cannot be derived from tags and must be supplied by the
/// ingestion pipeline itself.
///
/// `tenant_id` is never read from a tag, deliberately: a provider-controlled
/// string deciding which tenant a cost lands in would make tenant isolation a
/// function of whatever someone typed into a tag field. It comes from the
/// connector's own configured credentials instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttributionContext {
    pub tenant_id: TenantId,
    pub provider_id: ProviderId,
    /// Used when no tag identifies a project. `None` means an unattributable
    /// row is an error rather than a fallback.
    pub default_project_id: Option<ProjectId>,
    /// Used when no tag identifies a team.
    pub default_team_id: Option<TeamId>,
}

/// How a workload's purpose maps to a function code.
///
/// Production inference is `Cogs`, training and experimentation is `RnD`,
/// internal tooling is `OpEx`. A run-attributed record with no other signal is
/// treated as `RnD`, since a training run is what a `run_id` denotes.
#[must_use]
pub fn function_code_for(workload: Option<&str>, run_attributed: bool) -> FunctionCode {
    match workload
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("inference" | "serving" | "production" | "prod" | "api") => FunctionCode::Cogs,
        Some("training" | "train" | "experiment" | "eval" | "finetune" | "fine-tune") => {
            FunctionCode::RnD
        }
        Some("internal" | "tooling" | "platform" | "ci") => FunctionCode::OpEx,
        _ if run_attributed => FunctionCode::RnD,
        _ => FunctionCode::OpEx,
    }
}

/// Reads a [`Dimensions`] out of a provider tag map.
///
/// Fails rather than guessing when a required dimension is absent and no
/// default was configured — an unattributed cost that silently lands on a
/// placeholder project is worse than one that visibly fails to ingest, because
/// the first kind is discovered at quarter close.
pub fn attribute(
    tags: &serde_json::Value,
    rules: &TagAttributionRules,
    ctx: &AttributionContext,
) -> Result<Dimensions, AttributionError> {
    let map = tags.as_object().ok_or(AttributionError::TagsNotAnObject)?;

    let lookup = |candidates: &[String]| -> Option<(String, String)> {
        candidates.iter().find_map(|k| {
            map.get(k)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| (k.clone(), v.to_owned()))
        })
    };

    let parse_id =
        |found: Option<(String, String)>,
         what: &'static str|
         -> Result<Option<Uuid>, AttributionError> {
            match found {
                None => Ok(None),
                Some((key, value)) => Uuid::parse_str(&value).map(Some).map_err(|_| {
                    AttributionError::UnparseableTag {
                        key,
                        value,
                        expected: what,
                    }
                }),
            }
        };

    let project_id = parse_id(lookup(&rules.project_keys), "project id")?
        .map(ProjectId::from_uuid)
        .or(ctx.default_project_id)
        .ok_or(AttributionError::MissingRequiredDimension("project_id"))?;

    let team_id = parse_id(lookup(&rules.team_keys), "team id")?
        .map(TeamId::from_uuid)
        .or(ctx.default_team_id)
        .ok_or(AttributionError::MissingRequiredDimension("team_id"))?;

    let run_id = parse_id(lookup(&rules.run_keys), "run id")?.map(RunId::from_uuid);
    let customer_id =
        parse_id(lookup(&rules.customer_keys), "customer id")?.map(CustomerId::from_uuid);
    let model_id = parse_id(lookup(&rules.model_keys), "model id")?.map(ModelId::from_uuid);
    let dataset_id = parse_id(lookup(&rules.dataset_keys), "dataset id")?.map(DatasetId::from_uuid);
    let commitment_id =
        parse_id(lookup(&rules.commitment_keys), "commitment id")?.map(CommitmentId::from_uuid);

    let workload = lookup(&rules.workload_keys).map(|(_, v)| v);
    let function_code = function_code_for(workload.as_deref(), run_id.is_some());

    let mut dims = Dimensions::new(
        ctx.tenant_id,
        project_id,
        team_id,
        ctx.provider_id,
        function_code,
    );
    if let Some(id) = run_id {
        dims = dims.with_run(id);
    }
    if let Some(id) = customer_id {
        dims = dims.with_customer(id);
    }
    if let Some(id) = model_id {
        dims = dims.with_model(id);
    }
    if let Some(id) = dataset_id {
        dims = dims.with_dataset(id);
    }
    if let Some(id) = commitment_id {
        dims = dims.with_commitment(id);
    }
    Ok(dims)
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

    fn ctx() -> AttributionContext {
        AttributionContext {
            tenant_id: TenantId::new(),
            provider_id: ProviderId::new(),
            default_project_id: None,
            default_team_id: Some(TeamId::new()),
        }
    }

    #[test]
    fn maps_tags_onto_the_full_spine() {
        let project = Uuid::now_v7();
        let run = Uuid::now_v7();
        let tags = json!({
            "falkr:project": project.to_string(),
            "falkr:run": run.to_string(),
            "falkr:workload": "training",
        });
        let dims = attribute(&tags, &TagAttributionRules::default(), &ctx()).unwrap();
        assert_eq!(dims.project_id.as_uuid(), project);
        assert_eq!(dims.run_id.map(RunId::as_uuid), Some(run));
        assert_eq!(dims.function_code, FunctionCode::RnD);
        assert!(dims.is_run_attributed());
    }

    #[test]
    fn falls_back_through_key_aliases_in_order() {
        // No `falkr:project`, but a bare `project` — both are configured.
        let project = Uuid::now_v7();
        let tags = json!({ "project": project.to_string() });
        let dims = attribute(&tags, &TagAttributionRules::default(), &ctx()).unwrap();
        assert_eq!(dims.project_id.as_uuid(), project);
    }

    #[test]
    fn refuses_to_invent_a_project_when_none_is_tagged() {
        let tags = json!({ "team": Uuid::now_v7().to_string() });
        assert_eq!(
            attribute(&tags, &TagAttributionRules::default(), &ctx()),
            Err(AttributionError::MissingRequiredDimension("project_id"))
        );
    }

    #[test]
    fn uses_a_configured_default_project_when_present() {
        let default = ProjectId::new();
        let mut c = ctx();
        c.default_project_id = Some(default);
        let dims = attribute(&json!({}), &TagAttributionRules::default(), &c).unwrap();
        assert_eq!(dims.project_id, default);
        assert!(!dims.is_run_attributed());
    }

    #[test]
    fn reports_an_unparseable_tag_rather_than_dropping_it() {
        let tags = json!({ "falkr:project": "not-a-uuid" });
        let err = attribute(&tags, &TagAttributionRules::default(), &ctx()).unwrap_err();
        assert!(matches!(err, AttributionError::UnparseableTag { .. }));
    }

    #[test]
    fn ignores_blank_tag_values() {
        // AWS emits empty strings for tags that exist but are unset on a
        // resource; treating "" as a value would fail the UUID parse.
        let mut c = ctx();
        c.default_project_id = Some(ProjectId::new());
        let tags = json!({ "falkr:project": "   " });
        assert!(attribute(&tags, &TagAttributionRules::default(), &c).is_ok());
    }

    #[test]
    fn derives_function_codes_from_workload() {
        assert_eq!(
            function_code_for(Some("inference"), false),
            FunctionCode::Cogs
        );
        assert_eq!(
            function_code_for(Some("Training"), false),
            FunctionCode::RnD
        );
        assert_eq!(function_code_for(Some("ci"), false), FunctionCode::OpEx);
        // A run-attributed row with no workload tag is a training run.
        assert_eq!(function_code_for(None, true), FunctionCode::RnD);
        assert_eq!(function_code_for(None, false), FunctionCode::OpEx);
    }
}
