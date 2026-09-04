//! **Invariant 2**, against a real database and two genuinely concurrent
//! connections.
//!
//! the invariant list is specific about this one: *the database is the dedup source of
//! truth, not an in-memory map — test this with two concurrent connections, not
//! two sequential calls on one.* Two sequential calls on one connection pass
//! against an in-process `HashSet` that provides no protection whatsoever once
//! there are two API replicas, which there always are.
//!
//! ## What this can and cannot assert today
//!
//! There is **no idempotency key on the ledger entry yet** — P03 adds it with
//! the AS 2401 entry header. What exists is the mechanism the key will rely on:
//! the unique index on `(aggregate_id, sequence_number)` in
//! `migrations/0003_ledger_events.sql`. So the test below drives the mechanism:
//! two connections load the same aggregate at the same sequence, both attempt
//! the same append, and the assertion is that **exactly one event exists
//! afterwards** — which is [`assert_single_event_for_key`] with the aggregate
//! stream standing in for the key.
//!
//! When P03 lands the key, the setup changes and the assertion does not. That is
//! deliberate: the assertion is the specification.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the no-unwrap rule scopes the ban to non-test code"
)]

use esrs::store::EventStore as _;
use esrs::{Aggregate as _, AggregateState};
use falkr_core::{
    Currency, Dimensions, FunctionCode, Money, ProjectId, ProviderId, TeamId, TenantId,
};
use falkr_events::{JournalLine, LedgerAggregate, LedgerCommand, LedgerState};
use falkr_infra::test_support::setup;
use falkr_infra::{LedgerStoreError, PgLedgerEventStore};
use falkr_testkit::assertions::assert_single_event_for_key;
use falkr_testkit::runner::account_id;
use rust_decimal_macros::dec;
use uuid::Uuid;

fn dims(tenant: TenantId) -> Dimensions {
    Dimensions::new(
        tenant,
        ProjectId::new(),
        TeamId::new(),
        ProviderId::new(),
        FunctionCode::OpEx,
    )
}

fn entry() -> LedgerCommand {
    LedgerCommand::PostJournalEntry {
        lines: vec![
            JournalLine::debit(account_id("1000"), Money::new(dec!(1000.00), Currency::Usd)),
            JournalLine::credit(account_id("4000"), Money::new(dec!(1000.00), Currency::Usd)),
        ],
        dims: Dimensions::new(
            TenantId::new(),
            ProjectId::new(),
            TeamId::new(),
            ProviderId::new(),
            FunctionCode::OpEx,
        ),
    }
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn two_concurrent_appends_at_the_same_sequence_leave_exactly_one_event() {
    let db = setup().await;
    let tenant = TenantId::new();
    let aggregate_id = Uuid::now_v7();

    // Two independent stores over the same pool. Each loads the aggregate at
    // sequence 0 before either writes, which is the interleaving a single
    // sequential test can never produce.
    let left = PgLedgerEventStore::new(db.pool.clone(), tenant);
    let right = PgLedgerEventStore::new(db.pool.clone(), tenant);

    let mut left_state: AggregateState<LedgerState> = left.load(aggregate_id).await.unwrap();
    let mut right_state: AggregateState<LedgerState> = right.load(aggregate_id).await.unwrap();
    assert_eq!(
        left_state.next_sequence_number(),
        right_state.next_sequence_number(),
        "the two writers must start from the same sequence for this to test anything"
    );

    let left_events =
        LedgerAggregate::handle_command(left_state.inner(), entry()).expect("accepted");
    let right_events =
        LedgerAggregate::handle_command(right_state.inner(), entry()).expect("accepted");

    // Both attempts run concurrently on separate connections from the pool.
    let (left_result, right_result) = tokio::join!(
        left.persist(&mut left_state, left_events),
        right.persist(&mut right_state, right_events),
    );

    let winners = usize::from(left_result.is_ok()) + usize::from(right_result.is_ok());
    assert_eq!(
        winners, 1,
        "exactly one writer must win: left={left_result:?} right={right_result:?}"
    );

    for result in [left_result, right_result] {
        if let Err(error) = result {
            assert!(
                matches!(error, LedgerStoreError::ConcurrencyConflict { .. }),
                "the loser must be able to tell a conflict from a database failure, \
                 because the correct response (reload and retry) depends on it; got {error:?}"
            );
        }
    }

    let persisted = left.event_count(aggregate_id).await.unwrap();
    assert_single_event_for_key(&aggregate_id.to_string(), persisted)
        .expect("exactly one event must survive two concurrent appends");
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn the_loser_can_reload_and_retry_without_losing_the_write() {
    // The other half of the contract: a conflict is recoverable, not fatal.
    // Without this, a correct implementation and a livelock look identical.
    let db = setup().await;
    let tenant = TenantId::new();
    let aggregate_id = Uuid::now_v7();
    let store = PgLedgerEventStore::new(db.pool.clone(), tenant);

    let mut stale: AggregateState<LedgerState> = store.load(aggregate_id).await.unwrap();
    let stale_events = LedgerAggregate::handle_command(stale.inner(), entry()).expect("accepted");

    // Somebody else appends first.
    let mut fresh: AggregateState<LedgerState> = store.load(aggregate_id).await.unwrap();
    let fresh_events = LedgerAggregate::handle_command(fresh.inner(), entry()).expect("accepted");
    store.persist(&mut fresh, fresh_events).await.unwrap();

    let conflict = store.persist(&mut stale, stale_events).await;
    assert!(matches!(
        conflict,
        Err(LedgerStoreError::ConcurrencyConflict { .. })
    ));

    // Reload and retry: the write lands at the next sequence.
    let mut reloaded: AggregateState<LedgerState> = store.load(aggregate_id).await.unwrap();
    let retried = LedgerAggregate::handle_command(reloaded.inner(), entry()).expect("accepted");
    store.persist(&mut reloaded, retried).await.unwrap();

    assert_eq!(store.event_count(aggregate_id).await.unwrap(), 2);
}

/// **Invariant 2, pending.** Records the gap against a real database: the store
/// deduplicates on `(aggregate_id, sequence_number)` and on nothing else, so two
/// deliveries of the same external document land as two events.
///
/// TODO(P03): once the entry header carries an idempotency key, the second
/// persist must produce no event and `assert_single_event_for_key` must see 1.
#[tokio::test]
#[ignore = "P03: there is no idempotency key on a journal entry yet; this records \
            the gap. See the module docs"]
async fn two_posts_with_the_same_idempotency_key_produce_one_event() {
    let db = setup().await;
    let tenant = TenantId::new();
    let aggregate_id = Uuid::now_v7();
    let store = PgLedgerEventStore::new(db.pool.clone(), tenant);

    // The same external document delivered twice, sequentially — a webhook retry
    // after the nightly poll already ingested it.
    for _ in 0..2 {
        let mut state: AggregateState<LedgerState> = store.load(aggregate_id).await.unwrap();
        let events = LedgerAggregate::handle_command(state.inner(), entry()).expect("accepted");
        store.persist(&mut state, events).await.unwrap();
    }

    let persisted = store.event_count(aggregate_id).await.unwrap();
    assert_eq!(
        persisted, 2,
        "today both deliveries persist; there is no key to deduplicate on"
    );
    assert!(
        assert_single_event_for_key("external-invoice-9001", persisted).is_err(),
        "the assertion P03 must satisfy fails today, which is the point"
    );

    // `dims` is the spine every posting must carry; referenced here so the helper
    // does not rot while this test is a placeholder.
    let _ = dims(tenant);
}
