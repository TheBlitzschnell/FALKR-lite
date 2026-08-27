//! Synthetic FOCUS-shaped cost data goes
//! in, a correctly-tagged `CostEvent` with a populated `Dimensions` comes out.
//!
//! This half needs no database. Part two — that the event persists, survives
//! double delivery, and is invisible to another tenant — lives in
//! `crates/infra/tests/cost_events_pg.rs` and needs a real Postgres.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the unwrap/expect ban targets production code, not tests"
)]

use falkr_core::{Currency, FunctionCode, ProjectId, ProviderId, RunId, TeamId, TenantId};
use falkr_cost_spine::attribution::tags::{AttributionContext, TagAttributionRules};
use falkr_cost_spine::connector::{CostConnector, Cursor};
use falkr_cost_spine::event::{ChargeCategory, ProviderKind, ServiceCategory};
use falkr_cost_spine::providers::aws::{AwsFocusConnector, InMemoryExportSource};
use rust_decimal_macros::dec;
use uuid::Uuid;

/// A FOCUS 1.x export as AWS Data Exports delivers it: standard FOCUS columns
/// plus AWS's `resourceTags/user:` prefix on cost allocation tags.
fn focus_csv(project: Uuid, run: Uuid) -> String {
    format!(
        "BillingAccountId,BillingCurrency,ChargePeriodStart,ChargePeriodEnd,BilledCost,EffectiveCost,ListCost,ServiceName,ServiceCategory,ChargeCategory,ResourceId,RegionId,resourceTags/user:falkr:project,resourceTags/user:falkr:run,resourceTags/user:falkr:workload\n\
         123456789012,USD,2026-08-01T00:00:00Z,2026-08-01T01:00:00Z,32.7700,29.4930,32.7700,Amazon Elastic Compute Cloud,Compute,usage,i-0abc123gpu,us-east-1,{project},{run},training\n\
         123456789012,USD,2026-08-01T00:00:00Z,2026-08-01T01:00:00Z,4.1000,4.1000,4.1000,Amazon Simple Storage Service,Storage,usage,checkpoint-bucket,us-east-1,{project},{run},training\n\
         123456789012,USD,2026-08-01T00:00:00Z,2026-08-01T01:00:00Z,12.5000,11.2500,12.5000,Amazon SageMaker,AI and Machine Learning,usage,endpoint-prod-1,us-east-1,{project},,inference\n\
         123456789012,USD,2026-08-01T00:00:00Z,2026-09-01T00:00:00Z,50000.0000,50000.0000,50000.0000,Amazon Elastic Compute Cloud,Compute,purchase,ri-p5-48xl,us-east-1,{project},,\n"
    )
}

struct Fixture {
    connector: AwsFocusConnector<InMemoryExportSource>,
    project: ProjectId,
    run: RunId,
}

fn fixture() -> Fixture {
    let project = Uuid::now_v7();
    let run = Uuid::now_v7();
    let source = InMemoryExportSource::new().with_export(
        "focus/2026-08/export-00001.csv",
        "2026-08",
        &focus_csv(project, run),
    );
    let ctx = AttributionContext {
        tenant_id: TenantId::new(),
        provider_id: ProviderId::new(),
        default_project_id: None,
        // No team tag in this export; the pipeline supplies the default.
        default_team_id: Some(TeamId::new()),
    };
    Fixture {
        connector: AwsFocusConnector::new(
            source,
            TagAttributionRules::default(),
            ctx,
            Currency::Usd,
        ),
        project: ProjectId::from_uuid(project),
        run: RunId::from_uuid(run),
    }
}

#[tokio::test]
async fn focus_export_normalizes_into_dimensioned_cost_events() {
    let f = fixture();
    let raw = f
        .connector
        .fetch_since(Cursor::beginning())
        .await
        .expect("fetch");
    assert_eq!(raw.len(), 4, "one record per FOCUS row");

    let events: Vec<_> = raw
        .into_iter()
        .map(|r| f.connector.normalize(r).expect("normalize"))
        .collect();

    // --- the GPU compute line ---
    let gpu = &events[0];
    assert_eq!(gpu.billing_account_id, "123456789012");
    assert_eq!(gpu.service_category, ServiceCategory::Compute);
    assert_eq!(gpu.charge_category, ChargeCategory::Usage);
    assert_eq!(gpu.resource_id.as_deref(), Some("i-0abc123gpu"));
    assert_eq!(gpu.region_id.as_deref(), Some("us-east-1"));
    assert_eq!(gpu.currency(), Currency::Usd);
    assert_eq!(gpu.effective_cost.amount(), dec!(29.4930));
    assert_eq!(gpu.list_cost.amount(), dec!(32.7700));

    // The dimensional spine is fully populated at write time.
    assert_eq!(gpu.dims.project_id, f.project);
    assert_eq!(gpu.dims.run_id, Some(f.run));
    assert_eq!(gpu.dims.function_code, FunctionCode::RnD);
    assert!(gpu.dims.is_run_attributed());

    // --- the storage line shares the run ---
    assert_eq!(events[1].dims.run_id, Some(f.run));
    assert_eq!(events[1].service_category, ServiceCategory::Storage);

    // --- the inference endpoint is COGS, and carries no run ---
    let inference = &events[2];
    assert_eq!(inference.dims.function_code, FunctionCode::Cogs);
    assert_eq!(inference.dims.run_id, None);
    assert_eq!(
        inference.service_category,
        ServiceCategory::AiAndMachineLearning
    );

    // --- the reserved-instance purchase routes to the commitment engine ---
    let purchase = &events[3];
    assert_eq!(purchase.charge_category, ChargeCategory::Purchase);
    assert!(
        purchase.charge_category.creates_commitment(),
        "Purchase lines create a Commitment rather than posting to the ledger \
         directly"
    );
}

#[tokio::test]
async fn aws_user_tag_prefix_is_stripped_before_rules_are_applied() {
    let f = fixture();
    let raw = f
        .connector
        .fetch_since(Cursor::beginning())
        .await
        .expect("fetch");
    let event = f.connector.normalize(raw[0].clone()).expect("normalize");

    // The rules match `falkr:project`, not `resourceTags/user:falkr:project`.
    assert!(event.tags.get("falkr:project").is_some());
    assert!(event.tags.get("resourceTags/user:falkr:project").is_none());
}

#[tokio::test]
async fn redelivery_produces_identical_idempotency_keys() {
    // The same export fetched twice must yield the same external id per row —
    // this is what makes the store's upsert a no-op instead of a duplicate
    //.
    let f = fixture();
    let first = f.connector.fetch_since(Cursor::beginning()).await.unwrap();
    let second = f.connector.fetch_since(Cursor::beginning()).await.unwrap();

    let keys = |records: Vec<falkr_cost_spine::RawCostRecord>| -> Vec<String> {
        records
            .into_iter()
            .map(|r| f.connector.normalize(r).unwrap().source_ref.external_id)
            .collect()
    };
    let a = keys(first);
    let b = keys(second);
    assert_eq!(a, b);

    // And the ids are actually distinct per row, not a constant.
    let mut unique = a.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        a.len(),
        "each invoice line has its own identity"
    );
}

#[tokio::test]
async fn a_restated_row_changes_its_content_hash_but_not_its_identity() {
    // AWS restates the open billing period until the invoice finalizes. A
    // corrected cost must be recognizable as the *same* line with *different*
    // content, or the correction silently fails to land.
    let project = Uuid::now_v7();
    let run = Uuid::now_v7();
    let original = focus_csv(project, run);
    let restated = original.replace("32.7700,29.4930", "32.7700,27.1000");

    let ctx = AttributionContext {
        tenant_id: TenantId::new(),
        provider_id: ProviderId::new(),
        default_project_id: None,
        default_team_id: Some(TeamId::new()),
    };
    let build = |csv: &str| {
        AwsFocusConnector::new(
            InMemoryExportSource::new().with_export(
                "focus/2026-08/export-00001.csv",
                "2026-08",
                csv,
            ),
            TagAttributionRules::default(),
            ctx,
            Currency::Usd,
        )
    };

    let before = build(&original);
    let after = build(&restated);
    let a = before
        .normalize(before.fetch_since(Cursor::beginning()).await.unwrap()[0].clone())
        .unwrap();
    let b = after
        .normalize(after.fetch_since(Cursor::beginning()).await.unwrap()[0].clone())
        .unwrap();

    assert_eq!(
        a.source_ref.external_id, b.source_ref.external_id,
        "identity is stable across a restatement"
    );
    assert_ne!(
        a.source_ref.content_hash, b.source_ref.content_hash,
        "content hash moves, which is how the store tells restatement from duplicate"
    );
    assert_eq!(b.effective_cost.amount(), dec!(27.1000));
}

#[tokio::test]
async fn provider_is_reported_as_aws() {
    assert_eq!(fixture().connector.provider(), ProviderKind::Aws);
}
