//! W&B ingestion is idempotent, and a `CostEvent` can be tagged with a real
//! `run_id` from the research graph.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the unwrap/expect ban targets production code, not tests"
)]

use falkr_core::{Currency, ExperimentId, Money, ProjectId, ProviderId, TeamId, TenantId};
use falkr_cost_spine::attribution::tags::{AttributionContext, TagAttributionRules};
use falkr_cost_spine::connector::{CostConnector, Cursor};
use falkr_cost_spine::providers::aws::{AwsFocusConnector, InMemoryExportSource};
use falkr_cost_spine::store::CostEventStore;
use falkr_infra::{PgCostEventStore, PgResearchGraphStore};
use falkr_research_graph::connector::{RunCursor, TrackerConnector};
use falkr_research_graph::entities::{Experiment, RunStatus, TrackerSource};
use falkr_research_graph::store::{ResearchGraphStore, RunUpsert};
use falkr_research_graph::trackers::wandb::{InMemoryWandbApi, WandbConnector};
use rust_decimal_macros::dec;
use serde_json::json;

mod common;
use common::setup;

fn wandb_run(state: &str, ended: Option<&str>) -> serde_json::Value {
    let mut v = json!({
        "id": "3xk9qz1a",
        "entity": "acme-ai",
        "project": "llm-pretrain",
        "state": state,
        "createdAt": "2026-08-01T00:00:00Z",
        "heartbeatAt": "2026-08-01T06:30:00Z",
    });
    if let Some(e) = ended {
        v["endedAt"] = json!(e);
    }
    v
}

async fn seed_experiment(store: &PgResearchGraphStore, tenant: TenantId) -> ExperimentId {
    let experiment = Experiment {
        id: ExperimentId::new(),
        tenant_id: tenant,
        project_id: ProjectId::new(),
        name: "llm-pretrain".to_owned(),
    };
    store
        .upsert_experiment(&experiment)
        .await
        .expect("experiment");
    experiment.id
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn duplicate_wandb_delivery_produces_exactly_one_run() {
    // Idempotency, stated as an explicit test rather than assumed: a
    // webhook and a poll delivering the same run must not create two rows. Two
    // Run rows for one training job double-count every cost attributed to it.
    let db = setup().await;
    let store = PgResearchGraphStore::new(db.pool.clone());
    let tenant = TenantId::new();
    let experiment = seed_experiment(&store, tenant).await;

    let connector = WandbConnector::new(
        InMemoryWandbApi::new().with_run(wandb_run("running", None)),
        tenant,
        experiment,
        Currency::Usd,
    );

    // Delivery one.
    let batch = connector
        .fetch_runs_since(RunCursor::default())
        .await
        .unwrap();
    let first = connector.normalize(batch[0].clone()).unwrap();
    let a = store.upsert_run(&first).await.expect("first upsert");
    assert!(a.is_new(), "first delivery inserts");

    // Delivery two — the identical record, normalized afresh so it carries a
    // different surrogate RunId, exactly as a real re-delivery would.
    let batch = connector
        .fetch_runs_since(RunCursor::default())
        .await
        .unwrap();
    let second = connector.normalize(batch[0].clone()).unwrap();
    assert_ne!(first.id, second.id, "a fresh surrogate id per delivery");
    assert_eq!(
        first.external_ref, second.external_ref,
        "same tracker identity"
    );

    let b = store.upsert_run(&second).await.expect("second upsert");
    assert!(!b.is_new(), "second delivery must not insert");
    assert_eq!(b, RunUpsert::Unchanged(a.run_id()));
    assert_eq!(a.run_id(), b.run_id(), "both resolve to the same run");

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
        .fetch_one(
            &mut *falkr_infra::begin_tenant_tx(&db.pool, tenant)
                .await
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(count, 1, "one training job, one row");
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn a_run_progressing_to_finished_updates_rather_than_duplicating() {
    let db = setup().await;
    let store = PgResearchGraphStore::new(db.pool.clone());
    let tenant = TenantId::new();
    let experiment = seed_experiment(&store, tenant).await;

    let running = WandbConnector::new(
        InMemoryWandbApi::new().with_run(wandb_run("running", None)),
        tenant,
        experiment,
        Currency::Usd,
    );
    let batch = running
        .fetch_runs_since(RunCursor::default())
        .await
        .unwrap();
    let open = running.normalize(batch[0].clone()).unwrap();
    let inserted = store.upsert_run(&open).await.unwrap();
    assert!(inserted.is_new());

    // The same run, now finished. Real change → a genuine update.
    let finished = WandbConnector::new(
        InMemoryWandbApi::new().with_run(wandb_run("finished", Some("2026-08-01T07:00:00Z"))),
        tenant,
        experiment,
        Currency::Usd,
    );
    let batch = finished
        .fetch_runs_since(RunCursor::default())
        .await
        .unwrap();
    let closed = finished.normalize(batch[0].clone()).unwrap();
    let updated = store.upsert_run(&closed).await.unwrap();

    assert_eq!(updated, RunUpsert::Updated(inserted.run_id()));

    let stored = store
        .find_run_by_external_ref(tenant, &closed.external_ref)
        .await
        .unwrap()
        .expect("run exists");
    assert_eq!(stored.status, RunStatus::Finished);
    assert!(stored.ended_at.is_some());
    assert_eq!(stored.id, inserted.run_id(), "the surrogate id is stable");
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn a_cost_event_is_attributed_to_a_real_run_and_rolls_up() {
    // The join the whole dimensional spine exists to make possible: a FOCUS
    // cost line tagged with a run_id that is a real Run from the research
    // graph, summing into that run's attributed cost.
    let db = setup().await;
    let research = PgResearchGraphStore::new(db.pool.clone());
    let costs = PgCostEventStore::new(db.pool.clone());

    let tenant = TenantId::new();
    let experiment = seed_experiment(&research, tenant).await;

    // 1. Ingest the run from W&B.
    let connector = WandbConnector::new(
        InMemoryWandbApi::new().with_run(wandb_run("finished", Some("2026-08-01T07:00:00Z"))),
        tenant,
        experiment,
        Currency::Usd,
    );
    let batch = connector
        .fetch_runs_since(RunCursor::default())
        .await
        .unwrap();
    let run = connector.normalize(batch[0].clone()).unwrap();
    let run_id = store_run(&research, &run).await;
    assert_eq!(run.attributed_cost, Money::zero(Currency::Usd));

    // 2. Ingest two AWS cost lines tagged with that run's real id.
    let project = ProjectId::new();
    let csv = format!(
        "BillingAccountId,BillingCurrency,ChargePeriodStart,ChargePeriodEnd,BilledCost,EffectiveCost,ListCost,ServiceName,ServiceCategory,ChargeCategory,ResourceId,RegionId,resourceTags/user:falkr:project,resourceTags/user:falkr:run,resourceTags/user:falkr:workload\n\
         123456789012,USD,2026-08-01T00:00:00Z,2026-08-01T01:00:00Z,32.7700,29.4930,32.7700,Amazon Elastic Compute Cloud,Compute,usage,i-0gpu,us-east-1,{p},{r},training\n\
         123456789012,USD,2026-08-01T01:00:00Z,2026-08-01T02:00:00Z,32.7700,29.4930,32.7700,Amazon Elastic Compute Cloud,Compute,usage,i-0gpu,us-east-1,{p},{r},training\n",
        p = project.as_uuid(),
        r = run_id.as_uuid(),
    );
    let aws = AwsFocusConnector::new(
        InMemoryExportSource::new().with_export("focus/2026-08/e1.csv", "2026-08", &csv),
        TagAttributionRules::default(),
        AttributionContext {
            tenant_id: tenant,
            provider_id: ProviderId::new(),
            default_project_id: None,
            default_team_id: Some(TeamId::new()),
        },
        Currency::Usd,
    );
    let events: Vec<_> = aws
        .fetch_since(Cursor::beginning())
        .await
        .unwrap()
        .into_iter()
        .map(|r| aws.normalize(r).unwrap())
        .collect();

    assert_eq!(events.len(), 2);
    for e in &events {
        assert_eq!(e.dims.run_id.map(|r| r.as_uuid()), Some(run_id.as_uuid()));
    }
    assert_eq!(costs.upsert(&events).await.unwrap().inserted, 2);

    // 3. Roll the cost up onto the run.
    let attributed = research
        .refresh_attributed_cost(tenant, run_id)
        .await
        .expect("refresh");
    assert_eq!(attributed, Money::new(dec!(58.9860), Currency::Usd));

    let reloaded = research
        .find_run(tenant, run_id)
        .await
        .unwrap()
        .expect("run");
    assert_eq!(reloaded.attributed_cost, attributed);

    // Attribution coverage is now 100% — both cost events carry a run.
    let coverage = costs
        .attribution_coverage(tenant, "2026-01-01T00:00:00Z".parse().unwrap())
        .await
        .unwrap();
    assert_eq!(coverage.total_events, 2);
    assert_eq!(coverage.run_attributed, 2);
    assert!(!coverage.below_build_gate());
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn runs_are_invisible_across_tenants() {
    let db = setup().await;
    let store = PgResearchGraphStore::new(db.pool.clone());

    let alice = TenantId::new();
    let bob = TenantId::new();
    let experiment = seed_experiment(&store, alice).await;

    let connector = WandbConnector::new(
        InMemoryWandbApi::new().with_run(wandb_run("finished", None)),
        alice,
        experiment,
        Currency::Usd,
    );
    let batch = connector
        .fetch_runs_since(RunCursor::default())
        .await
        .unwrap();
    let run = connector.normalize(batch[0].clone()).unwrap();
    store.upsert_run(&run).await.unwrap();

    assert!(
        store
            .find_run_by_external_ref(alice, &run.external_ref)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .find_run_by_external_ref(bob, &run.external_ref)
            .await
            .unwrap()
            .is_none(),
        "another tenant must not see this run, even knowing its W&B id"
    );
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn the_mlflow_mock_satisfies_the_same_trait() {
    // The second implementation exists so `TrackerConnector` cannot quietly
    // grow W&B-shaped assumptions. If this stops compiling, the trait has.
    use falkr_research_graph::trackers::mlflow::MockMlflowConnector;

    let db = setup().await;
    let store = PgResearchGraphStore::new(db.pool.clone());
    let tenant = TenantId::new();
    let experiment = seed_experiment(&store, tenant).await;

    let wandb = WandbConnector::new(
        InMemoryWandbApi::new().with_run(wandb_run("finished", None)),
        tenant,
        experiment,
        Currency::Usd,
    );
    let batch = wandb.fetch_runs_since(RunCursor::default()).await.unwrap();
    let mut run = wandb.normalize(batch[0].clone()).unwrap();
    run.external_ref.source = TrackerSource::Mlflow;

    let mlflow = MockMlflowConnector::new().with_run(run.clone());
    let connectors: Vec<&dyn TrackerConnector> = vec![&wandb, &mlflow];
    assert_eq!(connectors[0].source(), TrackerSource::Wandb);
    assert_eq!(connectors[1].source(), TrackerSource::Mlflow);

    let raw = mlflow.fetch_runs_since(RunCursor::default()).await.unwrap();
    let round_tripped = mlflow.normalize(raw[0].clone()).unwrap();
    assert_eq!(round_tripped.external_ref, run.external_ref);
    assert!(store.upsert_run(&round_tripped).await.unwrap().is_new());
}

async fn store_run(
    store: &PgResearchGraphStore,
    run: &falkr_research_graph::entities::Run,
) -> falkr_core::RunId {
    store.upsert_run(run).await.expect("upsert run").run_id()
}
