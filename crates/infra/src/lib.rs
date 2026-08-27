//! # falkr-infra
//!
//! The persistence layer: Postgres connection pooling, SQLx adapters,
//! row-level-security context, and migrations.
//!
//! This crate depends *on* the domain crates in order to implement the traits
//! they define — the dotted "provides impls to" arrow in the workspace layout.
//! The arrow never points the other way; a domain crate that imports `sqlx` or
//! `sea_orm` has broken the rule, and `scripts/check-deps.sh` fails the build
//! when one does.
//!
//! It is also the only crate that enables `falkr-core`'s `sqlx` feature, which
//! is what gives the domain-ID newtypes their `sqlx::Type` derives at the
//! database boundary.
//!
//! SeaORM arrives with the customer-extensible entities in `compliance`/`hr`
//!; nothing needs runtime-flexible schema yet.

pub mod cost_events;
pub mod db;
pub mod ledger_store;
pub mod research_graph;

#[cfg(feature = "test-support")]
pub mod test_support;

pub use cost_events::PgCostEventStore;
pub use db::{
    DbError, PoolConfig, TENANT_GUC, begin_tenant_tx, connect, connect_with,
    current_tenant_context, migrate,
};
pub use ledger_store::{LedgerStoreError, LedgerStores, PgLedgerEventStore};
pub use research_graph::PgResearchGraphStore;
