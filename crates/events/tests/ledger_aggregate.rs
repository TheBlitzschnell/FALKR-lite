//! The ledger's core invariants, exercised without a database.
//!
//! Everything here runs against the pure `handle_command`/`apply_event` pair,
//! which is the point of keeping them pure: the rules that decide whether a
//! financial fact is admissible are testable in microseconds, with no runtime
//! and no Postgres.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the unwrap/expect ban targets production code, not tests"
)]

use esrs::Aggregate as _;
use falkr_core::{
    AccountId, CapitalizationStatus, Currency, Dimensions, FunctionCode, Money, PolicyId,
    ProjectId, ProviderId, RunId, TeamId, TenantId,
};
use falkr_events::{
    JournalLine, LedgerAggregate, LedgerCommand, LedgerError, LedgerEvent, LedgerState,
};
use proptest::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn dims() -> Dimensions {
    Dimensions::new(
        TenantId::new(),
        ProjectId::new(),
        TeamId::new(),
        ProviderId::new(),
        FunctionCode::RnD,
    )
}

fn usd(d: Decimal) -> Money {
    Money::new(d, Currency::Usd)
}

fn exec(state: LedgerState, cmd: LedgerCommand) -> (LedgerState, Vec<LedgerEvent>) {
    let events = LedgerAggregate::handle_command(&state, cmd).expect("command accepted");
    let next = events
        .iter()
        .cloned()
        .fold(state, LedgerAggregate::apply_event);
    (next, events)
}

#[test]
fn a_balanced_entry_posts_and_projects_correct_balances() {
    // $32.77 of GPU compute: debit R&D expense, credit accounts payable.
    let expense = AccountId::new();
    let payable = AccountId::new();
    let (state, events) = exec(
        LedgerState::default(),
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(expense, usd(dec!(32.77))),
                JournalLine::credit(payable, usd(dec!(32.77))),
            ],
            dims: dims(),
        },
    );

    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], LedgerEvent::JournalEntryPosted { .. }));
    assert_eq!(state.balance(expense), Some(usd(dec!(32.77))));
    assert_eq!(state.balance(payable), Some(usd(dec!(-32.77))));
    assert_eq!(
        state.trial_balance(Currency::Usd),
        Money::zero(Currency::Usd)
    );
}

#[test]
fn an_unbalanced_entry_is_rejected_at_the_command_boundary() {
    let err = LedgerAggregate::handle_command(
        &LedgerState::default(),
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(AccountId::new(), usd(dec!(100))),
                JournalLine::credit(AccountId::new(), usd(dec!(99.99))),
            ],
            dims: dims(),
        },
    )
    .unwrap_err();

    assert_eq!(
        err,
        LedgerError::Unbalanced {
            debits: dec!(100),
            credits: dec!(99.99)
        }
    );
}

#[test]
fn an_entry_mixing_currencies_is_rejected_rather_than_silently_summed() {
    let err = LedgerAggregate::handle_command(
        &LedgerState::default(),
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(AccountId::new(), usd(dec!(100))),
                JournalLine::credit(AccountId::new(), Money::new(dec!(100), Currency::Eur)),
            ],
            dims: dims(),
        },
    )
    .unwrap_err();
    assert!(matches!(err, LedgerError::Currency(_)));
}

#[test]
fn a_line_must_carry_exactly_one_side() {
    let account = AccountId::new();
    let both = JournalLine {
        account,
        debit: Some(usd(dec!(1))),
        credit: Some(usd(dec!(1))),
        source_usage_event_ids: Vec::new(),
    };
    assert_eq!(
        LedgerAggregate::handle_command(
            &LedgerState::default(),
            LedgerCommand::PostJournalEntry {
                lines: vec![both],
                dims: dims()
            }
        )
        .unwrap_err(),
        LedgerError::LineHasBothSides { account }
    );

    let neither = JournalLine {
        account,
        debit: None,
        credit: None,
        source_usage_event_ids: Vec::new(),
    };
    assert_eq!(
        LedgerAggregate::handle_command(
            &LedgerState::default(),
            LedgerCommand::PostJournalEntry {
                lines: vec![neither],
                dims: dims()
            }
        )
        .unwrap_err(),
        LedgerError::LineHasNoAmount { account }
    );
}

#[test]
fn replaying_the_log_reproduces_the_projection_exactly() {
    // The property the whole design rests on: balances are a function of the
    // event log and nothing else.
    let expense = AccountId::new();
    let payable = AccountId::new();
    let cash = AccountId::new();

    let mut state = LedgerState::default();
    let mut log: Vec<LedgerEvent> = Vec::new();
    for amount in [dec!(32.77), dec!(4.10), dec!(1199.99)] {
        let (next, events) = exec(
            state,
            LedgerCommand::PostJournalEntry {
                lines: vec![
                    JournalLine::debit(expense, usd(amount)),
                    JournalLine::credit(payable, usd(amount)),
                ],
                dims: dims(),
            },
        );
        state = next;
        log.extend(events);
    }
    let (state, events) = exec(
        state,
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(payable, usd(dec!(500))),
                JournalLine::credit(cash, usd(dec!(500))),
            ],
            dims: dims(),
        },
    );
    log.extend(events);

    let replayed = log
        .iter()
        .cloned()
        .fold(LedgerState::default(), LedgerAggregate::apply_event);

    assert_eq!(replayed, state);
    assert_eq!(replayed.balance(expense), Some(usd(dec!(1236.86))));
    assert_eq!(replayed.balance(payable), Some(usd(dec!(-736.86))));
    assert_eq!(replayed.balance(cash), Some(usd(dec!(-500))));
    assert_eq!(
        replayed.trial_balance(Currency::Usd),
        Money::zero(Currency::Usd)
    );
}

#[test]
fn a_reversal_restores_the_prior_balances_without_erasing_history() {
    let expense = AccountId::new();
    let payable = AccountId::new();

    let (posted, events) = exec(
        LedgerState::default(),
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(expense, usd(dec!(250))),
                JournalLine::credit(payable, usd(dec!(250))),
            ],
            dims: dims(),
        },
    );
    let LedgerEvent::JournalEntryPosted { id, .. } = &events[0] else {
        panic!("expected a posting");
    };

    let (reversed, _) = exec(
        posted,
        LedgerCommand::ReverseEntry {
            original_id: *id,
            reason: "duplicate provider invoice".to_owned(),
        },
    );

    assert_eq!(reversed.balance(expense), Some(usd(dec!(0))));
    assert_eq!(reversed.balance(payable), Some(usd(dec!(0))));
    assert!(reversed.is_reversed(*id));
    // The original entry is still there — a reversal appends, never deletes.
    assert_eq!(reversed.entry_count(), 2);
}

#[test]
fn an_entry_cannot_be_reversed_twice() {
    let (posted, events) = exec(
        LedgerState::default(),
        LedgerCommand::PostJournalEntry {
            lines: vec![
                JournalLine::debit(AccountId::new(), usd(dec!(10))),
                JournalLine::credit(AccountId::new(), usd(dec!(10))),
            ],
            dims: dims(),
        },
    );
    let LedgerEvent::JournalEntryPosted { id, .. } = &events[0] else {
        panic!("expected a posting");
    };
    let (once, _) = exec(
        posted,
        LedgerCommand::ReverseEntry {
            original_id: *id,
            reason: "first".to_owned(),
        },
    );
    assert_eq!(
        LedgerAggregate::handle_command(
            &once,
            LedgerCommand::ReverseEntry {
                original_id: *id,
                reason: "second".to_owned()
            }
        )
        .unwrap_err(),
        LedgerError::AlreadyReversed(*id)
    );
}

#[test]
fn capitalization_status_moves_through_the_event_log_like_everything_else() {
    let run = RunId::new();
    let policy = PolicyId::new();
    let (state, events) = exec(
        LedgerState::default(),
        LedgerCommand::ChangeCapitalizationStatus {
            run_id: run,
            to: CapitalizationStatus::Capitalized,
            policy_ref: policy,
        },
    );

    assert!(matches!(
        events[0],
        LedgerEvent::CapitalizationStatusChanged {
            from: CapitalizationStatus::PendingReview,
            to: CapitalizationStatus::Capitalized,
            ..
        }
    ));
    assert_eq!(
        state.capitalization_status(run),
        Some(CapitalizationStatus::Capitalized)
    );

    // A no-op transition is rejected rather than recorded as a fact.
    assert!(matches!(
        LedgerAggregate::handle_command(
            &state,
            LedgerCommand::ChangeCapitalizationStatus {
                run_id: run,
                to: CapitalizationStatus::Capitalized,
                policy_ref: policy,
            }
        ),
        Err(LedgerError::CapitalizationUnchanged { .. })
    ));
}

// --- properties -----------------------------------------------------------

prop_compose! {
    /// A randomly generated *balanced* entry: n debit lines whose total is
    /// mirrored by credit lines split by different weights, so the two sides
    /// agree without the individual amounts pairing up.
    fn arb_balanced_entry()(
        debits in prop::collection::vec(1i64..1_000_000i64, 1..8),
        credit_splits in prop::collection::vec(1i64..100i64, 1..8),
    ) -> Vec<JournalLine> {
        let total: i64 = debits.iter().sum();
        let mut lines: Vec<JournalLine> = debits
            .iter()
            .map(|cents| JournalLine::debit(AccountId::new(), usd(Decimal::new(*cents, 2))))
            .collect();

        let weight_sum: i64 = credit_splits.iter().sum();
        let mut allocated = 0i64;
        for (i, w) in credit_splits.iter().enumerate() {
            let share = if i + 1 == credit_splits.len() {
                total - allocated
            } else {
                total * w / weight_sum
            };
            allocated += share;
            if share > 0 {
                lines.push(JournalLine::credit(
                    AccountId::new(),
                    usd(Decimal::new(share, 2)),
                ));
            }
        }
        lines
    }
}

proptest! {
    /// The ledger's defining property: the sum of all debits across a
    /// posted entry always exactly equals the sum of credits, for any randomly
    /// generated valid entry.
    #[test]
    fn posted_entries_always_balance(lines in arb_balanced_entry()) {
        let events = LedgerAggregate::handle_command(
            &LedgerState::default(),
            LedgerCommand::PostJournalEntry { lines, dims: dims() },
        )?;

        let LedgerEvent::JournalEntryPosted { lines, .. } = &events[0] else {
            return Err(TestCaseError::fail("expected a posting"));
        };
        let debits: Decimal = lines.iter().filter_map(|l| l.debit).map(Money::amount).sum();
        let credits: Decimal = lines.iter().filter_map(|l| l.credit).map(Money::amount).sum();
        prop_assert_eq!(debits, credits);
    }

    /// The ledger's fundamental invariant: after any sequence of postings the
    /// trial balance is exactly zero. Not approximately — exactly, which is
    /// only true because `Money` is decimal.
    #[test]
    fn the_trial_balance_is_always_exactly_zero(
        entries in prop::collection::vec(arb_balanced_entry(), 1..12)
    ) {
        let mut state = LedgerState::default();
        for lines in entries {
            let events = LedgerAggregate::handle_command(
                &state,
                LedgerCommand::PostJournalEntry { lines, dims: dims() },
            )?;
            state = events.into_iter().fold(state, LedgerAggregate::apply_event);
        }
        prop_assert_eq!(state.trial_balance(Currency::Usd), Money::zero(Currency::Usd));
    }

    /// Replay is deterministic: folding the log from scratch reproduces the
    /// same projection, which is what makes "rebuild balances from events" a
    /// safe operation rather than a leap of faith.
    #[test]
    fn replay_is_deterministic(
        entries in prop::collection::vec(arb_balanced_entry(), 1..10)
    ) {
        let mut state = LedgerState::default();
        let mut log = Vec::new();
        for lines in entries {
            let events = LedgerAggregate::handle_command(
                &state,
                LedgerCommand::PostJournalEntry { lines, dims: dims() },
            )?;
            log.extend(events.iter().cloned());
            state = events.into_iter().fold(state, LedgerAggregate::apply_event);
        }
        let replayed = log.into_iter().fold(LedgerState::default(), LedgerAggregate::apply_event);
        prop_assert_eq!(replayed, state);
    }
}
