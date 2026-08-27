//! Multi-tenancy enforcement.
//!
//! Tenant isolation is a **security boundary**, not a filter. These tests treat
//! it that way: they assert what the database enforces, not what the
//! application intends, and they connect as an unprivileged role because
//! `FORCE ROW LEVEL SECURITY` does not constrain a superuser.
//!
//! Three kinds of test live here:
//!
//! 1. [`every_table_is_protected_by_row_level_security`] — a catalogue audit
//!    that fails when *any* table lacks RLS, FORCE, a policy, or a `tenant_id`.
//!    It is written against `pg_catalog` rather than a list, so a table added
//!    in a later phase is covered without anyone remembering to add it here.
//! 2. [`two_tenants_have_zero_cross_visibility`] — the definition of done.
//! 3. The pooling tests, which cover the failure that
//!    the one that actually breaks RLS in production.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the unwrap/expect ban targets production code, not tests"
)]

use chrono::Utc;
use esrs::Aggregate as _;
use esrs::store::EventStore as _;
use falkr_core::{
    AccountId, CapitalizationStatus, Currency, Dimensions, ExperimentId, FunctionCode, Money,
    ProjectId, ProviderId, RunId, TeamId, TenantId,
};
use falkr_cost_spine::event::{
    ChargeCategory, CostEvent, ProviderKind, ServiceCategory, SourceRef,
};
use falkr_cost_spine::store::CostEventStore;
use falkr_events::{JournalLine, LedgerAggregate, LedgerCommand};
use falkr_infra::{PgCostEventStore, PgLedgerEventStore, PgResearchGraphStore};
use falkr_research_graph::entities::{Experiment, ExternalRef, Run, RunStatus, TrackerSource};
use falkr_research_graph::store::ResearchGraphStore;
use rust_decimal_macros::dec;
use sqlx::Row as _;
use uuid::Uuid;

mod common;
use common::setup;

/// Tables exempt from tenant scoping, with the reason.
///
/// Deliberately tiny and explicit: an exemption list is where tenant isolation
/// goes to die, so anything added here needs a defensible reason written down.
const EXEMPT: &[(&str, &str)] = &[(
    "_sqlx_migrations",
    "migration bookkeeping, owned by the schema rather than by any tenant",
)];

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn every_table_is_protected_by_row_level_security() {
    let db = setup().await;

    let rows = sqlx::query(
        "
        SELECT c.relname            AS table_name,
               c.relrowsecurity     AS rls_enabled,
               c.relforcerowsecurity AS rls_forced,
               (SELECT COUNT(*) FROM pg_policy p WHERE p.polrelid = c.oid) AS policy_count,
               EXISTS (
                   SELECT 1 FROM pg_attribute a
                   WHERE a.attrelid = c.oid
                     AND a.attname = 'tenant_id'
                     AND a.attnum > 0
                     AND NOT a.attisdropped
               ) AS has_tenant_id
        FROM pg_class c
        JOIN pg_namespace n ON n.oid = c.relnamespace
        WHERE n.nspname = 'public' AND c.relkind = 'r'
        ORDER BY c.relname
        ",
    )
    .fetch_all(&db.pool)
    .await
    .expect("catalogue query");

    assert!(!rows.is_empty(), "no tables found — did migrations run?");

    let exempt: Vec<&str> = EXEMPT.iter().map(|(name, _)| *name).collect();
    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0_usize;

    for row in &rows {
        let name: String = row.get("table_name");
        if exempt.contains(&name.as_str()) {
            continue;
        }
        checked += 1;

        let enabled: bool = row.get("rls_enabled");
        let forced: bool = row.get("rls_forced");
        let policies: i64 = row.get("policy_count");
        let has_tenant_id: bool = row.get("has_tenant_id");

        if enabled {
            // FORCE is not optional. Plain ENABLE exempts the table owner, and
            // the application role very often *is* the owner — which means
            // isolation looks correct in every test right up until it is
            // load-bearing in production.
            if !forced {
                failures.push(format!("{name}: RLS is enabled but not FORCEd"));
            }
            if policies == 0 {
                failures.push(format!(
                    "{name}: RLS is on but no policy exists (denies all)"
                ));
            }
        } else {
            // One finding per table when RLS is absent entirely; the FORCE and
            // policy checks would just be noise on top of it.
            failures.push(format!("{name}: ROW LEVEL SECURITY is not enabled"));
        }
        if !has_tenant_id {
            failures.push(format!("{name}: no tenant_id column to scope by"));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {checked} tables are not tenant-isolated:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
    assert!(
        checked >= 5,
        "expected the full schema, saw only {checked} tables"
    );
}

/// Every tenant-scoped table, discovered from the catalogue.
async fn tenant_tables(pool: &sqlx::PgPool) -> Vec<String> {
    sqlx::query(
        "SELECT c.relname AS table_name
         FROM pg_class c
         JOIN pg_namespace n ON n.oid = c.relnamespace
         JOIN pg_attribute a ON a.attrelid = c.oid
         WHERE n.nspname = 'public'
           AND c.relkind = 'r'
           AND a.attname = 'tenant_id'
           AND NOT a.attisdropped
         ORDER BY c.relname",
    )
    .fetch_all(pool)
    .await
    .expect("table list")
    .iter()
    .map(|r| r.get::<String, _>("table_name"))
    .collect()
}

/// Writes one valid row into every tenant-scoped table for `tenant`.
///
/// Seeded through the **real stores** rather than through generic minimal
/// inserts. A generic insert cannot satisfy this schema's NOT NULL columns or
/// its CHECK constraints, and working around them would mean testing isolation
/// against rows the application could never produce.
///
/// Returns the tables actually populated, so the assertions can refuse to pass
/// vacuously.
async fn seed_every_table(pool: &sqlx::PgPool, tenant: TenantId) -> Vec<String> {
    let mut seeded: Vec<String> = Vec::new();

    let research = PgResearchGraphStore::new(pool.clone());
    let experiment = Experiment {
        id: ExperimentId::new(),
        tenant_id: tenant,
        project_id: ProjectId::new(),
        name: "isolation-probe".to_owned(),
    };
    research
        .upsert_experiment(&experiment)
        .await
        .expect("experiment");
    seeded.push("experiments".to_owned());

    let run = Run {
        id: RunId::new(),
        tenant_id: tenant,
        experiment_id: experiment.id,
        external_ref: ExternalRef::new(TrackerSource::Wandb, format!("probe/{}", RunId::new())),
        status: RunStatus::Finished,
        started_at: Utc::now(),
        ended_at: Some(Utc::now()),
        attributed_cost: Money::zero(Currency::Usd),
        function_code: FunctionCode::RnD,
        capitalization_status: CapitalizationStatus::PendingReview,
    };
    research.upsert_run(&run).await.expect("run");
    seeded.push("runs".to_owned());

    let mut tx = falkr_infra::begin_tenant_tx(pool, tenant).await.unwrap();
    sqlx::query(
        "INSERT INTO checkpoints
             (id, tenant_id, run_id, storage_cost, storage_cost_currency, storage_uri)
         VALUES ($1, $2, $3, 1, 'USD', 's3://probe')",
    )
    .bind(Uuid::now_v7())
    .bind(tenant.as_uuid())
    .bind(run.id.as_uuid())
    .execute(&mut *tx)
    .await
    .expect("checkpoint");
    tx.commit().await.unwrap();
    seeded.push("checkpoints".to_owned());

    let costs = PgCostEventStore::new(pool.clone());
    let dims = Dimensions::new(
        tenant,
        ProjectId::new(),
        TeamId::new(),
        ProviderId::new(),
        FunctionCode::Cogs,
    );
    let cost_event = CostEvent::new(
        "probe-account".to_owned(),
        Utc::now() - chrono::Duration::hours(1),
        Utc::now(),
        Money::new(dec!(1), Currency::Usd),
        Money::new(dec!(1), Currency::Usd),
        Money::new(dec!(1), Currency::Usd),
        "probe-service".to_owned(),
        ServiceCategory::Compute,
        ChargeCategory::Usage,
        None,
        None,
        serde_json::json!({}),
        dims,
        SourceRef {
            provider: ProviderKind::Aws,
            external_id: format!("probe-{}", Uuid::now_v7()),
            export_ref: "probe".to_owned(),
            billing_period: "2026-08".to_owned(),
            content_hash: "probe".to_owned(),
            ingested_at: Utc::now(),
        },
    )
    .expect("cost event");
    costs.upsert(&[cost_event]).await.expect("cost upsert");
    seeded.push("cost_events".to_owned());

    let ledger_store = PgLedgerEventStore::new(pool.clone(), tenant);
    let mut state = ledger_store.load(Uuid::now_v7()).await.expect("load");
    let events = LedgerAggregate::handle_command(
        state.inner(),
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(AccountId::new(), Money::new(dec!(1), Currency::Usd)),
                JournalLine::credit(AccountId::new(), Money::new(dec!(1), Currency::Usd)),
            ],
            dims,
        },
    )
    .expect("balanced");
    ledger_store
        .persist(&mut state, events)
        .await
        .expect("persist");
    seeded.push("ledger_events".to_owned());

    seeded.sort();
    seeded.dedup();
    seeded
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn two_tenants_have_zero_cross_visibility() {
    // Alice's data is written into every
    // tenant-scoped table through the real stores; Bob must see none of it,
    // anywhere.
    let db = setup().await;
    let alice = TenantId::new();
    let bob = TenantId::new();

    let tables = tenant_tables(&db.pool).await;
    let seeded = seed_every_table(&db.pool, alice).await;

    // The check that keeps this from passing vacuously: every tenant-scoped
    // table in the schema must have been populated.
    let missing: Vec<&String> = tables.iter().filter(|t| !seeded.contains(t)).collect();
    assert!(
        missing.is_empty(),
        "these tenant-scoped tables were never seeded, so isolation is untested on them: {missing:?}"
    );

    for table in &tables {
        let mut alice_tx = falkr_infra::begin_tenant_tx(&db.pool, alice).await.unwrap();
        let alice_count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&mut *alice_tx)
            .await
            .unwrap();
        assert!(alice_count > 0, "{table}: Alice cannot see her own row");

        let mut bob_tx = falkr_infra::begin_tenant_tx(&db.pool, bob).await.unwrap();
        let bob_count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&mut *bob_tx)
            .await
            .unwrap();
        assert_eq!(
            bob_count, 0,
            "{table}: Bob can see {bob_count} of Alice's rows"
        );
    }
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn a_tenant_cannot_write_rows_belonging_to_another() {
    // Isolation has to hold on writes too. The policy's WITH CHECK clause is
    // what stops a tenant from planting a row under someone else's id — a read
    // policy alone would let it happen and only surface later.
    let db = setup().await;
    let alice = TenantId::new();
    let bob = TenantId::new();

    let mut tx = falkr_infra::begin_tenant_tx(&db.pool, alice).await.unwrap();
    let smuggled = sqlx::query(
        "INSERT INTO experiments (id, tenant_id, project_id, name)
         VALUES ($1, $2, $3, 'smuggled')",
    )
    .bind(Uuid::now_v7())
    .bind(bob.as_uuid())
    .bind(Uuid::now_v7())
    .execute(&mut *tx)
    .await;

    assert!(
        smuggled.is_err(),
        "Alice must not be able to insert a row owned by Bob"
    );
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn an_unscoped_connection_sees_nothing_across_every_table() {
    // Fail closed. If the tenant GUC is missing — the exact PgBouncer failure
    // pooling can cause — every policy must match nothing rather
    // than everything.
    let db = setup().await;
    let alice = TenantId::new();

    seed_every_table(&db.pool, alice).await;

    for table in &tenant_tables(&db.pool).await {
        // Deliberately bypasses begin_tenant_tx: no GUC is set.
        let leaked: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(leaked, 0, "{table}: unscoped connection saw {leaked} rows");
    }
}

// --- the pooling failure mode --------------------------------------------

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn tenant_context_does_not_survive_its_transaction() {
    // This is the property that makes the arrangement safe behind a
    // transaction pooler. If the GUC outlived its transaction, a connection
    // returned to the pool would carry one tenant's context into the next
    // caller's queries — a cross-tenant read with no bug at the call site.
    let db = setup().await;
    let alice = TenantId::new();

    let mut tx = falkr_infra::begin_tenant_tx(&db.pool, alice).await.unwrap();
    let inside: Option<String> =
        sqlx::query_scalar("SELECT NULLIF(current_setting('falkr.tenant_id', true), '')")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(
        inside.as_deref(),
        Some(alice.as_uuid().to_string().as_str()),
        "the context must be visible inside its own transaction"
    );
    tx.commit().await.unwrap();

    // Same pool, therefore very likely the same physical connection.
    let after = falkr_infra::current_tenant_context(&db.pool).await.unwrap();
    assert_eq!(
        after, None,
        "tenant context leaked past its transaction and into the pool"
    );
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn a_rolled_back_transaction_leaves_no_context_behind() {
    let db = setup().await;
    let alice = TenantId::new();

    let tx = falkr_infra::begin_tenant_tx(&db.pool, alice).await.unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(
        falkr_infra::current_tenant_context(&db.pool).await.unwrap(),
        None,
        "a rollback must clear the context as thoroughly as a commit"
    );
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn consecutive_transactions_do_not_inherit_each_others_tenant() {
    // The realistic shape of the leak: two tenants served in sequence off one
    // pooled connection.
    let db = setup().await;
    let alice = TenantId::new();
    let bob = TenantId::new();

    let mut tx = falkr_infra::begin_tenant_tx(&db.pool, alice).await.unwrap();
    sqlx::query(
        "INSERT INTO experiments (id, tenant_id, project_id, name)
         VALUES ($1, $2, $3, 'alice-v1')",
    )
    .bind(Uuid::now_v7())
    .bind(alice.as_uuid())
    .bind(Uuid::now_v7())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = falkr_infra::begin_tenant_tx(&db.pool, bob).await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM experiments")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(visible, 0, "Bob inherited Alice's context from the pool");
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn a_pooler_configured_pool_still_isolates_tenants() {
    // The statement cache is disabled behind a transaction pooler, which
    // changes how every query is sent. Isolation must not depend on that
    // choice.
    let db = setup().await;
    let alice = TenantId::new();
    let bob = TenantId::new();

    let pooled = falkr_infra::connect_with(
        &db.url,
        falkr_infra::PoolConfig {
            max_connections: 2,
            behind_transaction_pooler: true,
        },
    )
    .await
    .expect("pooler-mode pool");

    let mut tx = falkr_infra::begin_tenant_tx(&pooled, alice).await.unwrap();
    sqlx::query(
        "INSERT INTO experiments (id, tenant_id, project_id, name)
         VALUES ($1, $2, $3, 'alice-pooled')",
    )
    .bind(Uuid::now_v7())
    .bind(alice.as_uuid())
    .bind(Uuid::now_v7())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = falkr_infra::begin_tenant_tx(&pooled, bob).await.unwrap();
    let visible: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM experiments")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(visible, 0);

    // And the same pool still serves Alice her own row, twice — the second
    // read is the one that would fail if a cached prepared statement outlived
    // its connection.
    for _ in 0..2 {
        let mut tx = falkr_infra::begin_tenant_tx(&pooled, alice).await.unwrap();
        let own: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM experiments")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(own, 1);
    }
}
