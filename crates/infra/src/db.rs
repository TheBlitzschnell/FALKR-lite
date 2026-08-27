//! Postgres connection handling and tenant context.
//!
//! # The one thing that actually breaks RLS in production
//!
//! Connection pooling, silently dropping the tenant-context session variable. It is an easy mistake to make months
//! after everything looked fine in dev, because it does not fail in dev — a
//! direct connection has no pooler in between.
//!
//! This module addresses it in three ways rather than in a comment:
//!
//! 1. **Transaction-scoped, not session-scoped.** [`begin_tenant_tx`] sets the
//!    GUC with `set_config(..., is_local => true)`, which dies with the
//!    transaction. That is exactly what makes it safe under PgBouncer's
//!    transaction pooling: the setting and the statements that depend on it
//!    live inside one transaction, which a transaction pooler keeps on one
//!    server connection by definition. A session-scoped `SET` would be handed
//!    back to the pool and applied to somebody else's queries, or dropped.
//! 2. **Verified, at no extra cost.** `set_config` returns the value it
//!    applied, so [`begin_tenant_tx`] reads that return value and fails loudly
//!    if it is not what was asked for. A pooler that swallows the setting is
//!    caught at the point of failure instead of surfacing as mysteriously
//!    empty result sets.
//! 3. **Pooler-aware pool construction.** See [`PoolConfig`].

use falkr_core::TenantId;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Postgres, Transaction};

/// The transaction-local GUC that row-level security policies read.
///
/// Kept in one place because a typo here does not fail — it silently makes
/// every policy match nothing.
pub const TENANT_GUC: &str = "falkr.tenant_id";

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("invalid connection string: {0}")]
    InvalidUrl(String),
    #[error(
        "tenant context did not take effect: asked for {expected}, connection reports {actual:?}. \
         A transaction pooler is dropping the setting — see the module docs and \
         the multi-tenancy design."
    )]
    TenantContextRejected {
        expected: String,
        actual: Option<String>,
    },
}

/// How to build the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolConfig {
    pub max_connections: u32,
    /// Set when connecting through a **transaction-mode** pooler such as
    /// PgBouncer.
    ///
    /// This disables SQLx's prepared-statement cache. SQLx prepares statements
    /// and reuses them by name on the same session; a transaction pooler hands
    /// each transaction whichever server connection is free, so the second use
    /// of a cached statement can land on a connection that has never seen it
    /// and fails with `prepared statement "sqlx_s_1" does not exist`. The
    /// failure is intermittent and load-dependent, which is the worst kind to
    /// diagnose from a production incident.
    ///
    /// Leave `false` for a direct connection or a session-mode pooler, where
    /// the cache is a real performance win.
    pub behind_transaction_pooler: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 10,
            behind_transaction_pooler: false,
        }
    }
}

/// Opens a pool with default settings.
pub async fn connect(url: &str, max_connections: u32) -> Result<PgPool, DbError> {
    connect_with(
        url,
        PoolConfig {
            max_connections,
            ..PoolConfig::default()
        },
    )
    .await
}

/// Opens a pool, honouring [`PoolConfig`].
pub async fn connect_with(url: &str, config: PoolConfig) -> Result<PgPool, DbError> {
    let mut options: PgConnectOptions = url
        .parse()
        .map_err(|e: sqlx::Error| DbError::InvalidUrl(e.to_string()))?;

    if config.behind_transaction_pooler {
        options = options.statement_cache_capacity(0);
    }

    // Deliberately no `after_connect` hook that sets session state. Anything
    // set there is session-scoped, and a transaction pooler gives no guarantee
    // that the connection carrying it is the connection a later transaction
    // runs on. Session-level setup and RLS are incompatible by construction.
    let pool = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .connect_with(options)
        .await?;
    Ok(pool)
}

/// Runs the workspace migrations.
pub async fn migrate(pool: &PgPool) -> Result<(), DbError> {
    sqlx::migrate!("../../migrations").run(pool).await?;
    Ok(())
}

/// Begins a transaction with the tenant context set for row-level security.
///
/// Every read and write of a tenant-scoped table must go through here.
///
/// The setting is applied with `set_config(..., true)` — `SET LOCAL` in
/// function form — for two reasons:
///
/// - **It takes a bind parameter.** `SET LOCAL` cannot, so the naive version
///   interpolates a tenant id into SQL text; this keeps a tenant identifier
///   from ever being concatenated into a statement.
/// - **It is transaction-scoped.** The setting dies with the transaction, so a
///   connection returned to the pool cannot carry one tenant's context into
///   another tenant's query.
///
/// The applied value is read back from `set_config`'s own return — the same
/// round trip, no extra cost — so a pooler that drops the setting produces
/// [`DbError::TenantContextRejected`] rather than silently empty results.
pub async fn begin_tenant_tx<'a>(
    pool: &'a PgPool,
    tenant_id: TenantId,
) -> Result<Transaction<'a, Postgres>, DbError> {
    let mut tx = pool.begin().await?;
    let expected = tenant_id.as_uuid().to_string();

    let applied: Option<String> = sqlx::query_scalar("SELECT set_config($1, $2, true)")
        .bind(TENANT_GUC)
        .bind(&expected)
        .fetch_one(&mut *tx)
        .await?;

    if applied.as_deref() != Some(expected.as_str()) {
        return Err(DbError::TenantContextRejected {
            expected,
            actual: applied,
        });
    }

    Ok(tx)
}

/// The tenant context currently visible on a connection, if any.
///
/// Returns `None` when unset, which is the state a pooled connection must be in
/// between transactions. Exposed so tests can assert that directly rather than
/// inferring it from query results.
pub async fn current_tenant_context(pool: &PgPool) -> Result<Option<String>, DbError> {
    let value: Option<String> = sqlx::query_scalar("SELECT NULLIF(current_setting($1, true), '')")
        .bind(TENANT_GUC)
        .fetch_one(pool)
        .await?;
    Ok(value)
}
