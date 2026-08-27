//! A normalized `CostEvent` persists to
//! real Postgres, survives double delivery, applies a restatement, and is
//! invisible to another tenant under row-level security.
//!
//! # Running these
//!
//! They need a container runtime (Docker, Colima, Podman). They are marked
//! `#[ignore]` so `cargo test --workspace` stays green on a machine without
//! one; CI runs `cargo test --workspace -- --include-ignored`, so they are not
//! optional there.
//!
//! ```text
//! cargo test -p falkr-infra -- --include-ignored
//! ```
//!
//! Colima does not create `/var/run/docker.sock`, so point testcontainers at
//! its socket first:
//!
//! ```text
//! export DOCKER_HOST="unix://$HOME/.colima/default/docker.sock"
//! ```
//!
//! # Why a dedicated role
//!
//! `FORCE ROW LEVEL SECURITY` subjects the table *owner* to its policies, but
//! superusers and `BYPASSRLS` roles still bypass them. The default container
//! user is a superuser, so a test suite that connects as `postgres` would
//! report perfect isolation while proving nothing at all. These tests create a
//! plain `LOGIN` role and connect as that — which is also what production
//! should do.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the unwrap/expect ban targets production code, not tests"
)]

use chrono::{TimeZone as _, Utc};
use falkr_core::{Currency, ProjectId, ProviderId, TeamId, TenantId};
use falkr_cost_spine::attribution::tags::{AttributionContext, TagAttributionRules};
use falkr_cost_spine::connector::{CostConnector, Cursor};
use falkr_cost_spine::event::{CostEvent, ProviderKind};
use falkr_cost_spine::providers::aws::{AwsFocusConnector, InMemoryExportSource};
use falkr_cost_spine::store::CostEventStore;
use falkr_infra::PgCostEventStore;
use rust_decimal_macros::dec;
use uuid::Uuid;

mod common;
use common::setup;

fn focus_csv(project: Uuid, run: Uuid, effective: &str) -> String {
    format!(
        "BillingAccountId,BillingCurrency,ChargePeriodStart,ChargePeriodEnd,BilledCost,EffectiveCost,ListCost,ServiceName,ServiceCategory,ChargeCategory,ResourceId,RegionId,resourceTags/user:falkr:project,resourceTags/user:falkr:run,resourceTags/user:falkr:workload\n\
         123456789012,USD,2026-08-01T00:00:00Z,2026-08-01T01:00:00Z,32.7700,{effective},32.7700,Amazon Elastic Compute Cloud,Compute,usage,i-0abc123gpu,us-east-1,{project},{run},training\n"
    )
}

/// Runs the real ingestion path: FOCUS export in, `CostEvent` out.
async fn ingest(tenant_id: TenantId, project: Uuid, run: Uuid, effective: &str) -> Vec<CostEvent> {
    let connector = AwsFocusConnector::new(
        InMemoryExportSource::new().with_export(
            "focus/2026-08/export-00001.csv",
            "2026-08",
            &focus_csv(project, run, effective),
        ),
        TagAttributionRules::default(),
        AttributionContext {
            tenant_id,
            provider_id: ProviderId::new(),
            default_project_id: None,
            default_team_id: Some(TeamId::new()),
        },
        Currency::Usd,
    );
    connector
        .fetch_since(Cursor::beginning())
        .await
        .expect("fetch")
        .into_iter()
        .map(|r| connector.normalize(r).expect("normalize"))
        .collect()
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn focus_data_round_trips_through_postgres_with_dimensions_intact() {
    let db = setup().await;
    let store = PgCostEventStore::new(db.pool.clone());

    let tenant = TenantId::new();
    let project = Uuid::now_v7();
    let run = Uuid::now_v7();
    let events = ingest(tenant, project, run, "29.4930").await;

    let outcome = store.upsert(&events).await.expect("upsert");
    assert_eq!(outcome.inserted, 1);
    assert_eq!(outcome.duplicate, 0);
    assert_eq!(outcome.restated, 0);

    let stored = store
        .find_by_external_id(tenant, ProviderKind::Aws, &events[0].source_ref.external_id)
        .await
        .expect("find")
        .expect("row exists");

    // The whole point of the spine: every dimension survives the round trip.
    assert_eq!(stored.dims.tenant_id, tenant);
    assert_eq!(stored.dims.project_id, ProjectId::from_uuid(project));
    assert_eq!(stored.dims.run_id.map(|r| r.as_uuid()), Some(run));
    assert_eq!(stored.dims.function_code, events[0].dims.function_code);
    assert_eq!(stored.dims.team_id, events[0].dims.team_id);
    assert_eq!(stored.dims.provider_id, events[0].dims.provider_id);

    // And the money is exact, not merely close.
    assert_eq!(stored.effective_cost.amount(), dec!(29.4930));
    assert_eq!(stored.currency(), Currency::Usd);
    assert_eq!(stored.id, events[0].id);
    assert_eq!(stored.service_name, "Amazon Elastic Compute Cloud");
    assert_eq!(
        stored.tags.get("falkr:run").and_then(|v| v.as_str()),
        Some(run.to_string().as_str())
    );
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn double_delivery_does_not_double_count() {
    // Polling and webhooks both double-fire.
    let db = setup().await;
    let store = PgCostEventStore::new(db.pool.clone());

    let tenant = TenantId::new();
    let project = Uuid::now_v7();
    let run = Uuid::now_v7();

    let first = ingest(tenant, project, run, "29.4930").await;
    assert_eq!(store.upsert(&first).await.expect("first").inserted, 1);

    // A second, independent delivery of the identical export.
    let second = ingest(tenant, project, run, "29.4930").await;
    let outcome = store.upsert(&second).await.expect("second");
    assert_eq!(outcome.duplicate, 1, "identical content is a duplicate");
    assert_eq!(outcome.inserted, 0);
    assert_eq!(outcome.restated, 0);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cost_events")
        .fetch_one(
            &mut *falkr_infra::begin_tenant_tx(&db.pool, tenant)
                .await
                .unwrap(),
        )
        .await
        .expect("count");
    assert_eq!(count, 1, "one invoice line, one row");
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn a_restated_invoice_line_updates_in_place() {
    let db = setup().await;
    let store = PgCostEventStore::new(db.pool.clone());

    let tenant = TenantId::new();
    let project = Uuid::now_v7();
    let run = Uuid::now_v7();

    let original = ingest(tenant, project, run, "29.4930").await;
    store.upsert(&original).await.expect("original");

    // AWS restates the open period: same line, corrected cost.
    let restated = ingest(tenant, project, run, "27.1000").await;
    let outcome = store.upsert(&restated).await.expect("restated");
    assert_eq!(outcome.restated, 1, "changed content is a restatement");
    assert_eq!(outcome.duplicate, 0);
    assert_eq!(outcome.inserted, 0);

    let stored = store
        .find_by_external_id(
            tenant,
            ProviderKind::Aws,
            &original[0].source_ref.external_id,
        )
        .await
        .expect("find")
        .expect("row exists");
    assert_eq!(
        stored.effective_cost.amount(),
        dec!(27.1000),
        "the correction landed"
    );

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cost_events")
        .fetch_one(
            &mut *falkr_infra::begin_tenant_tx(&db.pool, tenant)
                .await
                .unwrap(),
        )
        .await
        .expect("count");
    assert_eq!(count, 1, "restatement updates, it does not append");
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn row_level_security_hides_one_tenants_costs_from_another() {
    let db = setup().await;
    let store = PgCostEventStore::new(db.pool.clone());

    let alice = TenantId::new();
    let bob = TenantId::new();
    let project = Uuid::now_v7();
    let run = Uuid::now_v7();

    let alice_events = ingest(alice, project, run, "29.4930").await;
    store.upsert(&alice_events).await.expect("alice upsert");

    let external_id = &alice_events[0].source_ref.external_id;

    // Alice sees her own row.
    assert!(
        store
            .find_by_external_id(alice, ProviderKind::Aws, external_id)
            .await
            .expect("alice read")
            .is_some()
    );

    // Bob, querying the identical key, sees nothing — enforced by the database,
    // not by a WHERE clause the application remembered to add.
    assert!(
        store
            .find_by_external_id(bob, ProviderKind::Aws, external_id)
            .await
            .expect("bob read")
            .is_none(),
        "cross-tenant read must return nothing under RLS"
    );

    // And a raw count under Bob's context agrees.
    let bob_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cost_events")
        .fetch_one(&mut *falkr_infra::begin_tenant_tx(&db.pool, bob).await.unwrap())
        .await
        .expect("bob count");
    assert_eq!(bob_count, 0);
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn an_unscoped_connection_sees_nothing_rather_than_everything() {
    // The pooling failure mode: if the tenant GUC is
    // missing, the policy must fail closed. A pooled connection that lost its
    // SET LOCAL should return zero rows, never the whole table.
    let db = setup().await;
    let store = PgCostEventStore::new(db.pool.clone());

    let tenant = TenantId::new();
    let events = ingest(tenant, Uuid::now_v7(), Uuid::now_v7(), "29.4930").await;
    store.upsert(&events).await.expect("upsert");

    // Deliberately bypasses begin_tenant_tx, so no GUC is set.
    let leaked: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cost_events")
        .fetch_one(&db.pool)
        .await
        .expect("unscoped count");
    assert_eq!(
        leaked, 0,
        "RLS must fail closed when tenant context is absent"
    );
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn attribution_coverage_reports_the_build_gate_metric() {
    let db = setup().await;
    let store = PgCostEventStore::new(db.pool.clone());

    let tenant = TenantId::new();
    let project = Uuid::now_v7();

    // One run-attributed event, one without a run tag.
    let attributed = ingest(tenant, project, Uuid::now_v7(), "29.4930").await;
    store.upsert(&attributed).await.expect("attributed");

    let unattributed_csv = format!(
        "BillingAccountId,BillingCurrency,ChargePeriodStart,ChargePeriodEnd,BilledCost,EffectiveCost,ListCost,ServiceName,ServiceCategory,ChargeCategory,ResourceId,RegionId,resourceTags/user:falkr:project\n\
         123456789012,USD,2026-08-02T00:00:00Z,2026-08-02T01:00:00Z,9.0000,9.0000,9.0000,Amazon Elastic Compute Cloud,Compute,usage,i-0shared,us-east-1,{project}\n"
    );
    let connector = AwsFocusConnector::new(
        InMemoryExportSource::new().with_export(
            "focus/2026-08/export-00002.csv",
            "2026-08",
            &unattributed_csv,
        ),
        TagAttributionRules::default(),
        AttributionContext {
            tenant_id: tenant,
            provider_id: ProviderId::new(),
            default_project_id: None,
            default_team_id: Some(TeamId::new()),
        },
        Currency::Usd,
    );
    let shared: Vec<CostEvent> = connector
        .fetch_since(Cursor::beginning())
        .await
        .expect("fetch")
        .into_iter()
        .map(|r| connector.normalize(r).expect("normalize"))
        .collect();
    store.upsert(&shared).await.expect("shared");

    let since = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let coverage = store
        .attribution_coverage(tenant, since)
        .await
        .expect("coverage");

    assert_eq!(coverage.total_events, 2);
    assert_eq!(coverage.run_attributed, 1);
    // 50% is well under the 80% gate — this is exactly the shared-GPU-node case
    // the telemetry fallback exists for.
    assert!(coverage.below_build_gate());
}
