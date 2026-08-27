//! Axum server bootstrap.

use std::sync::Arc;

use anyhow::Context as _;
use api::auth::{Scope, StaticTokenResolver};
use api::state::AppState;
use falkr_core::TenantId;
use falkr_infra::{LedgerStores, PgCostEventStore, PgResearchGraphStore, PoolConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL must be set")?;
    // Set when running behind PgBouncer or another transaction-mode pooler; it
    // disables the prepared-statement cache. See falkr_infra::db.
    let behind_pooler = std::env::var("DATABASE_TRANSACTION_POOLER")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let pool = falkr_infra::connect_with(
        &database_url,
        PoolConfig {
            max_connections: 16,
            behind_transaction_pooler: behind_pooler,
        },
    )
    .await
    .context("connecting to Postgres")?;

    falkr_infra::migrate(&pool)
        .await
        .context("running migrations")?;

    // Development-only credential wiring. A deployment supplies a real
    // TenantResolver — an OIDC/JWT verifier or a key store — as a different
    // implementation of the same trait; nothing else changes.
    let tenants = match std::env::var("FALKR_DEV_TOKEN") {
        Ok(token) => {
            let tenant = std::env::var("FALKR_DEV_TENANT_ID")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(TenantId::from_uuid)
                .context("FALKR_DEV_TENANT_ID must be a UUID when FALKR_DEV_TOKEN is set")?;
            tracing::warn!(
                "using the development token resolver — do not run this configuration in production"
            );
            StaticTokenResolver::new().with_token(
                token,
                tenant,
                vec![Scope::Read, Scope::Ingest, Scope::Post],
            )
        }
        // No tokens configured means every authenticated route returns 401.
        // Failing closed is the only safe default for a credential store.
        Err(_) => {
            tracing::warn!("no FALKR_DEV_TOKEN set; all authenticated routes will return 401");
            StaticTokenResolver::new()
        }
    };

    let state = AppState {
        tenants: Arc::new(tenants),
        costs: Arc::new(PgCostEventStore::new(pool.clone())),
        research: Arc::new(PgResearchGraphStore::new(pool.clone())),
        ledger: Arc::new(LedgerStores::new(pool)),
    };

    let addr = std::env::var("FALKR_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!(%addr, "falkr api listening");

    axum::serve(listener, api::router(state))
        .await
        .context("server error")?;
    Ok(())
}
