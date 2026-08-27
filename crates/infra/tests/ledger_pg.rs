//! Post a balanced `JournalEntryPosted`,
//! replay the event log, get a correct projected balance.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the unwrap/expect ban targets production code, not tests"
)]

use esrs::Aggregate as _;
use esrs::AggregateState;
use esrs::store::EventStore as _;
use falkr_core::{
    AccountId, Currency, Dimensions, FunctionCode, Money, ProjectId, ProviderId, TeamId, TenantId,
};
use falkr_events::{JournalLine, LedgerAggregate, LedgerCommand, LedgerEvent, LedgerState};
use falkr_infra::{LedgerStoreError, PgLedgerEventStore};
use rust_decimal_macros::dec;
use uuid::Uuid;

mod common;
use common::setup;

fn dims(tenant: TenantId) -> Dimensions {
    Dimensions::new(
        tenant,
        ProjectId::new(),
        TeamId::new(),
        ProviderId::new(),
        FunctionCode::RnD,
    )
}

fn usd(d: rust_decimal::Decimal) -> Money {
    Money::new(d, Currency::Usd)
}

/// Runs a command against the persisted stream: load, handle, persist.
async fn post(
    store: &PgLedgerEventStore,
    aggregate_id: Uuid,
    cmd: LedgerCommand,
) -> Result<Vec<LedgerEvent>, LedgerStoreError> {
    let mut state = store.load(aggregate_id).await?;
    let events = LedgerAggregate::handle_command(state.inner(), cmd).expect("command accepted");
    store.persist(&mut state, events.clone()).await?;
    Ok(events)
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn a_posted_entry_replays_into_the_correct_balance() {
    let db = setup().await;
    let tenant = TenantId::new();
    let store = PgLedgerEventStore::new(db.pool.clone(), tenant);
    let ledger = Uuid::now_v7();

    let expense = AccountId::new();
    let payable = AccountId::new();

    post(
        &store,
        ledger,
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(expense, usd(dec!(32.77))),
                JournalLine::credit(payable, usd(dec!(32.77))),
            ],
            dims: dims(tenant),
        },
    )
    .await
    .expect("post");

    // Replay from the log — nothing is cached in process.
    let replayed = store.load(ledger).await.expect("replay");
    let state = replayed.inner();

    assert_eq!(state.balance(expense), Some(usd(dec!(32.77))));
    assert_eq!(state.balance(payable), Some(usd(dec!(-32.77))));
    assert_eq!(
        state.trial_balance(Currency::Usd),
        Money::zero(Currency::Usd)
    );
    assert_eq!(state.entry_count(), 1);
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn many_entries_replay_in_order_into_an_exact_balance() {
    let db = setup().await;
    let tenant = TenantId::new();
    let store = PgLedgerEventStore::new(db.pool.clone(), tenant);
    let ledger = Uuid::now_v7();

    let expense = AccountId::new();
    let payable = AccountId::new();
    let cash = AccountId::new();

    for amount in [dec!(32.77), dec!(4.10), dec!(1199.99), dec!(0.003)] {
        post(
            &store,
            ledger,
            LedgerCommand::PostJournalEntry {
                lines: vec![
                    JournalLine::debit(expense, usd(amount)),
                    JournalLine::credit(payable, usd(amount)),
                ],
                dims: dims(tenant),
            },
        )
        .await
        .expect("post");
    }
    post(
        &store,
        ledger,
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(payable, usd(dec!(500))),
                JournalLine::credit(cash, usd(dec!(500))),
            ],
            dims: dims(tenant),
        },
    )
    .await
    .expect("post");

    let state = store.load(ledger).await.expect("replay");
    let state = state.inner();

    // Exact to the sub-cent: 32.77 + 4.10 + 1199.99 + 0.003.
    assert_eq!(state.balance(expense), Some(usd(dec!(1236.863))));
    assert_eq!(state.balance(payable), Some(usd(dec!(-736.863))));
    assert_eq!(state.balance(cash), Some(usd(dec!(-500))));
    assert_eq!(
        state.trial_balance(Currency::Usd),
        Money::zero(Currency::Usd)
    );
    assert_eq!(state.entry_count(), 5);
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn events_are_stored_in_sequence_and_the_stream_is_ordered() {
    let db = setup().await;
    let tenant = TenantId::new();
    let store = PgLedgerEventStore::new(db.pool.clone(), tenant);
    let ledger = Uuid::now_v7();

    for _ in 0..3 {
        post(
            &store,
            ledger,
            LedgerCommand::PostJournalEntry {
                lines: vec![
                    JournalLine::debit(AccountId::new(), usd(dec!(1))),
                    JournalLine::credit(AccountId::new(), usd(dec!(1))),
                ],
                dims: dims(tenant),
            },
        )
        .await
        .expect("post");
    }

    let stream = store.by_aggregate_id(ledger).await.expect("stream");
    assert_eq!(stream.len(), 3);
    // esrs pre-increments: a fresh AggregateState sits at 0 and
    // `next_sequence_number()` returns 1 for the first event, so a stream is
    // 1-based. Pinned here because the unique index on
    // (aggregate_id, sequence_number) is the concurrency control, and an
    // off-by-one in numbering would weaken it silently.
    let sequences: Vec<i32> = stream.iter().map(|e| *e.sequence_number()).collect();
    assert_eq!(
        sequences,
        vec![1, 2, 3],
        "sequence numbers are dense and ordered"
    );
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn a_concurrent_append_at_the_same_sequence_is_rejected() {
    // Two writers both read at sequence N and both try to write N+1. Exactly
    // one wins on the unique index; the loser gets a typed error telling it to
    // reload and retry, not a raw SQL failure.
    let db = setup().await;
    let tenant = TenantId::new();
    let store = PgLedgerEventStore::new(db.pool.clone(), tenant);
    let ledger = Uuid::now_v7();

    let entry = |tenant| LedgerCommand::PostJournalEntry {
        lines: vec![
            JournalLine::debit(AccountId::new(), usd(dec!(5))),
            JournalLine::credit(AccountId::new(), usd(dec!(5))),
        ],
        dims: dims(tenant),
    };

    // Both writers load the same (empty) state.
    let mut writer_a = store.load(ledger).await.unwrap();
    let mut writer_b = store.load(ledger).await.unwrap();

    let events_a = LedgerAggregate::handle_command(writer_a.inner(), entry(tenant)).unwrap();
    let events_b = LedgerAggregate::handle_command(writer_b.inner(), entry(tenant)).unwrap();

    store
        .persist(&mut writer_a, events_a)
        .await
        .expect("A wins");
    let loser = store.persist(&mut writer_b, events_b).await;

    assert!(
        matches!(loser, Err(LedgerStoreError::ConcurrencyConflict { .. })),
        "expected a concurrency conflict, got {loser:?}"
    );

    // And only one entry actually landed.
    let state = store.load(ledger).await.unwrap();
    assert_eq!(state.inner().entry_count(), 1);
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn the_event_log_refuses_update_and_delete() {
    // Append-only, enforced by the database rather than by review: the
    // ledger is append-only, so a correction is a reversing entry, never an
    // edit. Without this, "the ledger is immutable" is a convention.
    let db = setup().await;
    let tenant = TenantId::new();
    let store = PgLedgerEventStore::new(db.pool.clone(), tenant);
    let ledger = Uuid::now_v7();

    post(
        &store,
        ledger,
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(AccountId::new(), usd(dec!(100))),
                JournalLine::credit(AccountId::new(), usd(dec!(100))),
            ],
            dims: dims(tenant),
        },
    )
    .await
    .expect("post");

    let mut tx = falkr_infra::begin_tenant_tx(&db.pool, tenant)
        .await
        .unwrap();
    let updated = sqlx::query("UPDATE ledger_events SET occurred_on = now()")
        .execute(&mut *tx)
        .await;
    assert!(updated.is_err(), "UPDATE against the event log must fail");

    let mut tx = falkr_infra::begin_tenant_tx(&db.pool, tenant)
        .await
        .unwrap();
    let deleted = sqlx::query("DELETE FROM ledger_events")
        .execute(&mut *tx)
        .await;
    assert!(deleted.is_err(), "DELETE against the event log must fail");

    // The store's own delete refuses too, with a domain error that says why.
    assert!(matches!(
        store.delete(ledger).await,
        Err(LedgerStoreError::AppendOnly)
    ));
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn a_reversal_zeroes_the_balance_while_the_original_event_remains() {
    let db = setup().await;
    let tenant = TenantId::new();
    let store = PgLedgerEventStore::new(db.pool.clone(), tenant);
    let ledger = Uuid::now_v7();

    let expense = AccountId::new();
    let payable = AccountId::new();

    let events = post(
        &store,
        ledger,
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(expense, usd(dec!(250))),
                JournalLine::credit(payable, usd(dec!(250))),
            ],
            dims: dims(tenant),
        },
    )
    .await
    .expect("post");
    let LedgerEvent::JournalEntryPosted { id, .. } = &events[0] else {
        panic!("expected a posting");
    };

    post(
        &store,
        ledger,
        LedgerCommand::ReverseEntry {
            original_id: *id,
            reason: "duplicate provider invoice".to_owned(),
        },
    )
    .await
    .expect("reverse");

    let state = store.load(ledger).await.expect("replay");
    let state = state.inner();
    assert_eq!(state.balance(expense), Some(usd(dec!(0))));
    assert_eq!(state.balance(payable), Some(usd(dec!(0))));
    assert!(state.is_reversed(*id));

    // Two events in the log — the reversal appended, it did not erase.
    assert_eq!(store.by_aggregate_id(ledger).await.unwrap().len(), 2);
}

#[tokio::test]
#[ignore = "requires a container runtime; CI runs with --include-ignored"]
async fn one_tenants_ledger_is_invisible_to_another() {
    let db = setup().await;
    let alice = TenantId::new();
    let bob = TenantId::new();
    let ledger = Uuid::now_v7();

    let alice_store = PgLedgerEventStore::new(db.pool.clone(), alice);
    post(
        &alice_store,
        ledger,
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(AccountId::new(), usd(dec!(1_000_000))),
                JournalLine::credit(AccountId::new(), usd(dec!(1_000_000))),
            ],
            dims: dims(alice),
        },
    )
    .await
    .expect("post");

    // Bob replays the very same aggregate id and gets an empty ledger.
    let bob_store = PgLedgerEventStore::new(db.pool.clone(), bob);
    let bob_state: AggregateState<LedgerState> = bob_store.load(ledger).await.expect("replay");
    assert_eq!(bob_state.inner().entry_count(), 0);
    assert_eq!(
        bob_state.inner().trial_balance(Currency::Usd),
        Money::zero(Currency::Usd)
    );
    assert!(bob_store.by_aggregate_id(ledger).await.unwrap().is_empty());
}
