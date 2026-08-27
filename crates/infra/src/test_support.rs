//! Containerised Postgres fixture, shared across integration suites.
//!
//! Behind the `test-support` feature so it never reaches a binary. It lives in
//! `infra` rather than in each crate's `tests/common` because `api` and
//! `worker` need exactly the same fixture, and three copies of container setup
//! is three places for the RLS role handling to drift.
//!
//! # Running suites that use this
//!
//! ```text
//! export DOCKER_HOST="unix://$HOME/.colima/default/docker.sock"   # Colima only
//! cargo test --workspace -- --include-ignored
//! ```
//!
//! # Why a dedicated role
//!
//! `FORCE ROW LEVEL SECURITY` subjects the table *owner* to its policies, but
//! superusers and `BYPASSRLS` roles still bypass them. The default container
//! user is a superuser, so a suite that connects as `postgres` would report
//! perfect isolation while proving nothing. This creates a plain `LOGIN` role
//! and hands back a pool connected as that — which is also what production
//! should do.

#![allow(
    clippy::expect_used,
    reason = "a test fixture that cannot start its database has nothing useful \
              to return; the ban targets production code, and this \
              module is behind a dev-only feature"
)]

use sqlx::PgPool;
use testcontainers::ContainerAsync;
use testcontainers::runners::AsyncRunner as _;
use testcontainers_modules::postgres::Postgres as PostgresImage;

const APP_ROLE: &str = "falkr_app";
const APP_PASSWORD: &str = "falkr_app_pw";

/// A running Postgres with migrations applied and an unprivileged app pool.
pub struct TestDb {
    /// Held so the container outlives the pools.
    _container: ContainerAsync<PostgresImage>,
    /// Connected as the unprivileged application role — RLS applies.
    pub pool: PgPool,
    /// The same connection string, for tests that build their own pool.
    pub url: String,
}

/// Starts Postgres, runs migrations, and returns an unprivileged pool.
///
/// # Panics
///
/// Panics if the container cannot start or migrations fail — there is no useful
/// way for a test to continue past either.
pub async fn setup() -> TestDb {
    let container = PostgresImage::default()
        .start()
        .await
        .expect("start postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("map postgres port");

    let admin_url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let admin_pool = crate::connect(&admin_url, 4).await.expect("admin pool");
    crate::migrate(&admin_pool).await.expect("migrate");

    sqlx::query(&format!(
        "CREATE ROLE {APP_ROLE} LOGIN PASSWORD '{APP_PASSWORD}'"
    ))
    .execute(&admin_pool)
    .await
    .expect("create app role");
    for stmt in [
        format!("GRANT USAGE ON SCHEMA public TO {APP_ROLE}"),
        format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {APP_ROLE}"
        ),
    ] {
        sqlx::query(&stmt)
            .execute(&admin_pool)
            .await
            .expect("grant to app role");
    }
    admin_pool.close().await;

    let app_url = format!("postgres://{APP_ROLE}:{APP_PASSWORD}@127.0.0.1:{port}/postgres");
    let pool = crate::connect(&app_url, 8).await.expect("app pool");

    TestDb {
        _container: container,
        pool,
        url: app_url,
    }
}
