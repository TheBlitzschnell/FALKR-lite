//! HTTP-level integration tests.
//!
//! These drive the real router with `tower::ServiceExt::oneshot` — no socket,
//! no port, the same routing table and extractors production uses — against a
//! real Postgres with row-level security enforced.
//!
//! The load-bearing test is [`a_client_supplied_tenant_header_is_ignored`]:
//! everything else in this system trusts that the tenant on a request came from
//! a verified credential.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the unwrap/expect ban targets production code, not tests"
)]

use std::sync::Arc;

use api::auth::{Scope, StaticTokenResolver};
use api::state::AppState;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use falkr_core::{Currency, ExperimentId, FunctionCode, Money, ProjectId, TenantId};
use falkr_infra::test_support::{TestDb, setup};
use falkr_infra::{LedgerStores, PgCostEventStore, PgResearchGraphStore};
use falkr_research_graph::entities::{Experiment, ExternalRef, Run, RunStatus, TrackerSource};
use falkr_research_graph::store::ResearchGraphStore;
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

const ALICE_TOKEN: &str = "alice-secret-token";
const BOB_TOKEN: &str = "bob-secret-token";

struct Harness {
    db: TestDb,
    router: axum::Router,
    alice: TenantId,
    bob: TenantId,
}

async fn harness() -> Harness {
    let db = setup().await;
    let alice = TenantId::new();
    let bob = TenantId::new();

    let tenants = StaticTokenResolver::new()
        .with_token(ALICE_TOKEN, alice, vec![Scope::Read, Scope::Ingest])
        .with_token(BOB_TOKEN, bob, vec![Scope::Read, Scope::Ingest]);

    let state = AppState {
        tenants: Arc::new(tenants),
        costs: Arc::new(PgCostEventStore::new(db.pool.clone())),
        research: Arc::new(PgResearchGraphStore::new(db.pool.clone())),
        ledger: Arc::new(LedgerStores::new(db.pool.clone())),
    };

    let router = api::router(state);
    Harness {
        db,
        router,
        alice,
        bob,
    }
}

async fn send(router: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = router.clone().oneshot(req).await.expect("router response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn get(path: &str, token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().uri(path).method("GET");
    if let Some(t) = token {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).expect("request")
}

/// Seeds a run for `tenant` and returns its id.
async fn seed_run(h: &Harness, tenant: TenantId) -> falkr_core::RunId {
    let store = PgResearchGraphStore::new(h.db.pool.clone());
    let experiment = Experiment {
        id: ExperimentId::new(),
        tenant_id: tenant,
        project_id: ProjectId::new(),
        name: "http-test".to_owned(),
    };
    store.upsert_experiment(&experiment).await.unwrap();

    let run = Run {
        id: falkr_core::RunId::new(),
        tenant_id: tenant,
        experiment_id: experiment.id,
        external_ref: ExternalRef::new(TrackerSource::Wandb, format!("http/{}", Utc::now())),
        status: RunStatus::Finished,
        started_at: Utc::now(),
        ended_at: Some(Utc::now()),
        attributed_cost: Money::zero(Currency::Usd),
        function_code: FunctionCode::RnD,
        capitalization_status: falkr_core::CapitalizationStatus::PendingReview,
    };
    store.upsert_run(&run).await.unwrap().run_id()
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn health_needs_no_credential() {
    let h = harness().await;
    let response = h
        .router
        .clone()
        .oneshot(get("/health", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn an_unauthenticated_request_is_rejected() {
    let h = harness().await;
    let run = seed_run(&h, h.alice).await;

    let (status, body) = send(&h.router, get(&format!("/v1/runs/{}", run.as_uuid()), None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "unauthenticated");

    // A bad token is rejected identically — the response must not reveal
    // whether a token existed.
    let (status, other) = send(
        &h.router,
        get(&format!("/v1/runs/{}", run.as_uuid()), Some("not-a-token")),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, other);
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn a_client_supplied_tenant_header_is_ignored() {
    // THE test for this layer. If a caller could name its own tenant, RLS would
    // stop being a security boundary and become an instruction the caller
    // writes — the failure the multi-tenancy design guards against, arriving
    // through the front door instead of the pooler.
    let h = harness().await;
    let alice_run = seed_run(&h, h.alice).await;

    // Bob's credential, plus every header a confused or hostile client might
    // use to assert a different tenant.
    let req = Request::builder()
        .uri(format!("/v1/runs/{}", alice_run.as_uuid()))
        .method("GET")
        .header("authorization", format!("Bearer {BOB_TOKEN}"))
        .header("x-tenant-id", h.alice.as_uuid().to_string())
        .header("tenant-id", h.alice.as_uuid().to_string())
        .header("x-falkr-tenant", h.alice.as_uuid().to_string())
        .body(Body::empty())
        .unwrap();

    let (status, _) = send(&h.router, req).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "Bob's credential must decide the tenant, not Bob's headers"
    );
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn one_tenant_cannot_read_anothers_run() {
    // Checked in both directions. A one-way test passes just as well against a
    // handler that returns nothing to anybody.
    let h = harness().await;
    let alice_run = seed_run(&h, h.alice).await;
    let bob_run = seed_run(&h, h.bob).await;

    let alice_path = format!("/v1/runs/{}", alice_run.as_uuid());
    let bob_path = format!("/v1/runs/{}", bob_run.as_uuid());

    // Each sees their own.
    let (status, body) = send(&h.router, get(&alice_path, Some(ALICE_TOKEN))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], alice_run.as_uuid().to_string());

    let (status, body) = send(&h.router, get(&bob_path, Some(BOB_TOKEN))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], bob_run.as_uuid().to_string());

    // Neither sees the other's.
    let (status, _) = send(&h.router, get(&alice_path, Some(BOB_TOKEN))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&h.router, get(&bob_path, Some(ALICE_TOKEN))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn money_is_serialized_as_a_string_never_a_json_number() {
    // A JSON number is an IEEE 754 double to most clients, which would undo the
    // exact-decimal guarantee at the last possible moment — after the ledger,
    // the rating engine and Postgres have all preserved it.
    let h = harness().await;
    let run = seed_run(&h, h.alice).await;

    let (status, body) = send(
        &h.router,
        get(
            &format!("/v1/runs/{}/cost", run.as_uuid()),
            Some(ALICE_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["attributed_cost"].is_string(),
        "attributed_cost came back as {:?}",
        body["attributed_cost"]
    );
    assert_eq!(body["currency"], "USD");
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn a_missing_run_is_a_404_not_an_empty_result() {
    let h = harness().await;
    let (status, body) = send(
        &h.router,
        get(
            &format!("/v1/runs/{}", uuid::Uuid::now_v7()),
            Some(ALICE_TOKEN),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn coverage_reports_the_build_gate() {
    let h = harness().await;
    let (status, body) = send(&h.router, get("/v1/cost/coverage", Some(ALICE_TOKEN))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total_events"], 0);
    // An empty window is not a failing window.
    assert_eq!(body["below_build_gate"], false);
}
