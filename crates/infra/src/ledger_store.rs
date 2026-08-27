//! Postgres event store for the ledger, scoped to one tenant.
//!
//! # Why this exists instead of `esrs::store::postgres::PgStore`
//!
//! `PgStore::persist` opens its own transaction (`self.inner.pool.begin()`).
//! Row-level security on `ledger_events` reads a transaction-local GUC that
//! `begin_tenant_tx` sets, so a transaction the store opens for itself never
//! carries it — and every write is refused by the policy. It fails closed,
//! which is the right failure, but it fails.
//!
//! This is a genuine collision between the ledger design ("use an
//! event-sourcing crate") and §4.8 ("RLS is the enforcement boundary"), and it
//! resolves in favour of §4.8: tenant isolation is a security property and the
//! event store is an implementation detail.
//!
//! Everything worth not hand-rolling still comes from `esrs` — the
//! `Aggregate` shape, `AggregateState`, sequence numbering, the `StoreEvent`
//! envelope, optimistic-locking semantics. Only the storage adapter is ours,
//! and it is the one piece that has to know about tenant context.

use chrono::Utc;
use esrs::store::{EventStore, EventStoreLockGuard, StoreEvent, UnlockOnDrop};
use esrs::{Aggregate as _, AggregateState};
use falkr_core::TenantId;
use falkr_events::{LedgerAggregate, LedgerEvent, LedgerState};
use sqlx::{PgPool, Row as _};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum LedgerStoreError {
    #[error("database error: {0}")]
    Backend(String),
    #[error("stored ledger event {id} could not be deserialized: {detail}")]
    CorruptEvent { id: Uuid, detail: String },
    #[error("event could not be serialized: {0}")]
    Serialize(String),
    #[error(
        "concurrent write to aggregate {aggregate_id} at sequence {sequence_number}; \
         reload the aggregate and retry"
    )]
    ConcurrencyConflict {
        aggregate_id: Uuid,
        sequence_number: i32,
    },
    #[error("ledger_events is append-only; post a reversing entry instead of deleting")]
    AppendOnly,
}

fn backend(e: sqlx::Error) -> LedgerStoreError {
    LedgerStoreError::Backend(e.to_string())
}

/// The lock guard this store hands back.
///
/// Deliberately a no-op. `esrs` documents `lock` as an advisory optimization —
/// "ALL accesses (regardless of this guard) are subject to the usual optimistic
/// locking strategy on write" — and the real protection here is the unique
/// index on `(aggregate_id, sequence_number)`, which turns a lost update into a
/// constraint violation the caller must handle. A session-level advisory lock
/// would additionally be wrong behind PgBouncer in transaction mode, since the
/// connection holding it is not the connection that later writes.
struct NoAdvisoryLock;

impl UnlockOnDrop for NoAdvisoryLock {}

/// Event store for [`LedgerAggregate`], bound to one tenant.
///
/// The tenant is fixed at construction rather than passed per call because
/// `EventStore::persist` has no place to put it — which is also a useful
/// constraint: a store instance cannot accidentally serve two tenants.
pub struct PgLedgerEventStore {
    pool: PgPool,
    tenant_id: TenantId,
}

impl PgLedgerEventStore {
    #[must_use]
    pub const fn new(pool: PgPool, tenant_id: TenantId) -> Self {
        Self { pool, tenant_id }
    }

    #[must_use]
    pub const fn tenant_id(&self) -> TenantId {
        self.tenant_id
    }

    /// Replays an aggregate's whole stream into an [`AggregateState`].
    ///
    /// This is the "rebuild balances from the log" operation: it reads every
    /// event for the aggregate in sequence order and folds them through
    /// `apply_event`. Nothing else produces a balance.
    pub async fn load(
        &self,
        aggregate_id: Uuid,
    ) -> Result<AggregateState<LedgerState>, LedgerStoreError> {
        let events = self.by_aggregate_id(aggregate_id).await?;
        Ok(AggregateState::with_id(aggregate_id)
            .apply_store_events(events, LedgerAggregate::apply_event))
    }
}

/// Builds tenant-scoped ledger stores.
///
/// Exists so callers do not have to hold a `PgPool` themselves. `api` and
/// `worker` are allowed to wire infrastructure, but there is no reason for them
/// to import `sqlx` to do it — the narrower their contact with the database
/// layer, the less there is to reconsider when it changes.
#[derive(Debug, Clone)]
pub struct LedgerStores {
    pool: PgPool,
}

impl LedgerStores {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// A store bound to one tenant.
    #[must_use]
    pub fn for_tenant(&self, tenant_id: TenantId) -> PgLedgerEventStore {
        PgLedgerEventStore::new(self.pool.clone(), tenant_id)
    }
}

impl PgLedgerEventStore {
    /// Number of events in an aggregate's stream.
    ///
    /// Inherent rather than trait-only so a caller can ask without importing
    /// `esrs::store::EventStore`.
    pub async fn event_count(&self, aggregate_id: Uuid) -> Result<usize, LedgerStoreError> {
        Ok(self.by_aggregate_id(aggregate_id).await?.len())
    }
}

#[async_trait::async_trait]
impl EventStore for PgLedgerEventStore {
    type Aggregate = LedgerAggregate;
    type Error = LedgerStoreError;

    async fn lock(&self, _aggregate_id: Uuid) -> Result<EventStoreLockGuard, Self::Error> {
        // See `NoAdvisoryLock`: correctness comes from the unique index, not
        // from this guard.
        Ok(EventStoreLockGuard::new(NoAdvisoryLock))
    }

    async fn by_aggregate_id(
        &self,
        aggregate_id: Uuid,
    ) -> Result<Vec<StoreEvent<LedgerEvent>>, Self::Error> {
        let mut tx = crate::db::begin_tenant_tx(&self.pool, self.tenant_id)
            .await
            .map_err(|e| LedgerStoreError::Backend(e.to_string()))?;

        let rows = sqlx::query(
            "SELECT id, aggregate_id, payload, occurred_on, sequence_number, version
             FROM ledger_events
             WHERE aggregate_id = $1
             ORDER BY sequence_number ASC",
        )
        .bind(aggregate_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(backend)?;

        tx.commit().await.map_err(backend)?;

        rows.into_iter()
            .map(|row| {
                let id: Uuid = row.try_get("id").map_err(backend)?;
                let raw: serde_json::Value = row.try_get("payload").map_err(backend)?;
                let payload: LedgerEvent =
                    serde_json::from_value(raw).map_err(|e| LedgerStoreError::CorruptEvent {
                        id,
                        detail: e.to_string(),
                    })?;
                Ok(StoreEvent {
                    id,
                    aggregate_id: row.try_get("aggregate_id").map_err(backend)?,
                    payload,
                    occurred_on: row.try_get("occurred_on").map_err(backend)?,
                    sequence_number: row.try_get("sequence_number").map_err(backend)?,
                    version: row.try_get("version").map_err(backend)?,
                })
            })
            .collect()
    }

    /// Appends events atomically: either all of them land or none do.
    ///
    /// Sequence numbers come from `AggregateState`, so two writers that both
    /// read at sequence N will both try to write N+1 and exactly one will win
    /// on the unique index — surfaced as
    /// [`LedgerStoreError::ConcurrencyConflict`] rather than a raw SQL error,
    /// because the caller's correct response (reload and retry) depends on
    /// being able to tell that case apart.
    async fn persist(
        &self,
        aggregate_state: &mut AggregateState<LedgerState>,
        events: Vec<LedgerEvent>,
    ) -> Result<Vec<StoreEvent<LedgerEvent>>, Self::Error> {
        let aggregate_id = *aggregate_state.id();
        let occurred_on = Utc::now();

        let mut tx = crate::db::begin_tenant_tx(&self.pool, self.tenant_id)
            .await
            .map_err(|e| LedgerStoreError::Backend(e.to_string()))?;

        let mut stored = Vec::with_capacity(events.len());
        for payload in events {
            let sequence_number = aggregate_state.next_sequence_number();
            let id = Uuid::now_v7();
            let json = serde_json::to_value(&payload)
                .map_err(|e| LedgerStoreError::Serialize(e.to_string()))?;

            let result = sqlx::query(
                "INSERT INTO ledger_events
                     (id, aggregate_id, tenant_id, payload, occurred_on, sequence_number, version)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(id)
            .bind(aggregate_id)
            .bind(self.tenant_id.as_uuid())
            .bind(&json)
            .bind(occurred_on)
            .bind(sequence_number)
            .bind(Option::<i32>::None)
            .execute(&mut *tx)
            .await;

            if let Err(e) = result {
                // 23505 = unique_violation. On this table that can only be the
                // (aggregate_id, sequence_number) index, i.e. a concurrent
                // append.
                let is_conflict = e
                    .as_database_error()
                    .and_then(sqlx::error::DatabaseError::code)
                    .is_some_and(|c| c == "23505");
                return Err(if is_conflict {
                    LedgerStoreError::ConcurrencyConflict {
                        aggregate_id,
                        sequence_number,
                    }
                } else {
                    backend(e)
                });
            }

            stored.push(StoreEvent {
                id,
                aggregate_id,
                payload,
                occurred_on,
                sequence_number,
                version: None,
            });
        }

        tx.commit().await.map_err(backend)?;
        Ok(stored)
    }

    async fn publish(&self, _store_events: &[StoreEvent<LedgerEvent>]) {
        // No event bus yet. A background job runner would subscribe here;
        // until then, publishing is a no-op rather than a silent buffer that
        // would need draining.
    }

    async fn delete(&self, _aggregate_id: Uuid) -> Result<(), Self::Error> {
        // `ledger_events` carries a trigger that rejects UPDATE and DELETE
        // outright. Refusing here as well means the caller gets a domain error
        // explaining what to do instead, rather than a raw trigger exception.
        Err(LedgerStoreError::AppendOnly)
    }
}
