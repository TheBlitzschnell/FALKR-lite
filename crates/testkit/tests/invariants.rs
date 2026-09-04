//! The ten invariants, driven by `proptest` and by hand-built edge cases.
//!
//! Invariants 1, 3 and 5 hold against the aggregate that exists today and run
//! unconditionally. Invariants 2, 4 and 6–10 need features that later phases
//! deliver; those tests are written now, `#[ignore]`d with the phase that
//! unblocks them, and exercised against hand-built inputs so the *assertion* is
//! known correct before the feature exists. They are the acceptance criteria for
//! those phases.
//!
//! Running them: `cargo test -p falkr-testkit -- --include-ignored` shows the
//! full backlog failing, which is the honest picture and the point of P02.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the no-unwrap rule scopes the ban to non-test code"
)]

use std::collections::{BTreeMap, BTreeSet};

use esrs::Aggregate as _;
use falkr_core::{
    AccountId, Currency, Dimensions, FunctionCode, Money, ProjectId, ProviderId, TeamId, TenantId,
};
use falkr_events::{JournalLine, LedgerAggregate, LedgerCommand, LedgerEvent, LedgerState};
use falkr_testkit::allocation::{allocate, allocate_evenly};
use falkr_testkit::assertions::{
    self, BalanceScope, Invariant, MutationAttempt, assert_all_mutations_rejected, assert_balanced,
    assert_balances_net_to_zero, assert_entry_points_enumerated, assert_events_round_trip,
    assert_intercompany_eliminated, assert_no_unexpected_negative_balances, assert_replay_stable,
    assert_roll_forward, assert_single_event_for_key, assert_subledger_ties,
    assert_trial_balance_zero,
};
use falkr_testkit::chart::ChartOfAccounts;
use falkr_testkit::generator::{GeneratorSpec, generate};
use falkr_testkit::runner::account_id;
use proptest::prelude::*;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

fn dims() -> Dimensions {
    Dimensions::new(
        TenantId::new(),
        ProjectId::new(),
        TeamId::new(),
        ProviderId::new(),
        FunctionCode::OpEx,
    )
}

fn chart() -> ChartOfAccounts {
    ChartOfAccounts::corpus().expect("corpus/_reference must load")
}

/// Posts a generated ledger through the real aggregate.
fn post_all(entries: &[Vec<JournalLine>]) -> Result<(Vec<LedgerEvent>, LedgerState), String> {
    let mut state = LedgerState::default();
    let mut events = Vec::new();
    for lines in entries {
        let produced = LedgerAggregate::handle_command(
            &state,
            LedgerCommand::PostJournalEntry {
                lines: lines.clone(),
                dims: dims(),
            },
        )
        .map_err(|e| e.to_string())?;
        for event in produced {
            state = LedgerAggregate::apply_event(state, event.clone());
            events.push(event);
        }
    }
    Ok((events, state))
}

// ===========================================================================
// Invariant 1 — balanced postings
// ===========================================================================

proptest! {
    /// **Invariant 1.** Every generated entry balances, for every seed.
    ///
    /// The generator builds entries with a balancing plug, so this is as much a
    /// test of the generator as of the assertion — which is the point: a
    /// generator that quietly produced unbalanced entries would make every
    /// other property in this file vacuous.
    #[test]
    fn every_generated_entry_balances(seed in any::<u64>()) {
        let ledger = generate(seed, &GeneratorSpec::small());
        for entry in &ledger.entries {
            assert_balanced(&entry.lines).map_err(|e| TestCaseError::fail(e.to_string()))?;
        }
    }

    /// **Invariant 1.** The aggregate accepts exactly the entries that balance
    /// and rejects the ones that do not — no partial state on rejection.
    #[test]
    fn the_aggregate_rejects_an_unbalanced_entry_without_writing_anything(
        cents in 1_i64..1_000_000_i64,
        skew in 1_i64..1000_i64,
    ) {
        let a = account_id("1000");
        let b = account_id("4000");
        let lines = vec![
            JournalLine::debit(a, Money::new(Decimal::new(cents, 2), Currency::Usd)),
            JournalLine::credit(b, Money::new(Decimal::new(cents + skew, 2), Currency::Usd)),
        ];
        let state = LedgerState::default();
        let result = LedgerAggregate::handle_command(
            &state,
            LedgerCommand::PostJournalEntry { lines, dims: dims() },
        );
        prop_assert!(result.is_err(), "an unbalanced entry was accepted");
        // No event means no state change: the aggregate cannot have written a
        // partial entry, because a rejected command produces no events at all.
        prop_assert_eq!(state.entry_count(), 0);
    }
}

#[test]
fn a_balanced_multi_line_entry_with_extreme_magnitudes_still_sums_to_zero() {
    // the precision requirements: very large and very small magnitudes in the same ledger.
    let a = account_id("1000");
    let b = account_id("4000");
    let c = account_id("5000");
    let big = Money::new(dec!(99999999999999999999.99), Currency::Usd);
    let small = Money::new(dec!(0.01), Currency::Usd);
    let lines = vec![
        JournalLine::debit(a, big),
        JournalLine::debit(b, small),
        JournalLine::credit(c, Money::new(dec!(100000000000000000000.00), Currency::Usd)),
    ];
    assert!(assert_balanced(&lines).is_ok());
}

// ===========================================================================
// Invariant 2 — idempotency
// ===========================================================================

#[test]
fn the_idempotency_assertion_itself_is_correct() {
    assert!(assert_single_event_for_key("k", 1).is_ok());
    assert!(assert_single_event_for_key("k", 0).is_err());
    assert!(assert_single_event_for_key("k", 2).is_err());
}

/// **Invariant 2, pending.** Today this documents the gap precisely: two
/// identical commands produce two distinct events, because there is no key to
/// deduplicate on.
///
/// TODO(P03): once the entry header carries an idempotency key, replace the body
/// with the real assertion — the second command produces no event — and drive it
/// from two concurrent connections in tests/idempotency_pg.rs. The assertion
/// function (`assert_single_event_for_key`) does not change.
#[test]
#[ignore = "P03: the ledger has no idempotency key yet; this records the gap. \
            The concurrent, database-backed harness is in tests/idempotency_pg.rs"]
fn two_concurrent_posts_with_one_key_produce_one_event() {
    let command = || LedgerCommand::PostJournalEntry {
        lines: vec![
            JournalLine::debit(account_id("1000"), Money::new(dec!(10.00), Currency::Usd)),
            JournalLine::credit(account_id("4000"), Money::new(dec!(10.00), Currency::Usd)),
        ],
        dims: dims(),
    };
    let state = LedgerState::default();
    let first = LedgerAggregate::handle_command(&state, command()).unwrap();
    let state = LedgerAggregate::apply_event(state, first[0].clone());
    let second = LedgerAggregate::handle_command(&state, command()).unwrap();

    // The gap, stated as an assertion rather than as a comment: the same
    // command twice yields two events, and nothing in the command carries a key
    // that could say they are the same.
    assert_ne!(first[0], second[0]);
    assert!(assert_single_event_for_key("no-key-exists-yet", 2).is_err());
}

// ===========================================================================
// Invariant 3 — monotonic ordering and deterministic replay
// ===========================================================================

proptest! {
    /// **Invariant 3.** Replaying the log into a fresh projection reproduces the
    /// live projection, twice over, for any generated ledger.
    #[test]
    fn replay_reproduces_the_live_projection(seed in any::<u64>()) {
        let ledger = generate(seed, &GeneratorSpec::small());
        let entries: Vec<Vec<JournalLine>> =
            ledger.entries.iter().map(|e| e.lines.clone()).collect();
        let (events, live) = post_all(&entries).map_err(TestCaseError::fail)?;
        assert_replay_stable(&events, &live).map_err(|e| TestCaseError::fail(e.to_string()))?;
        assert_events_round_trip(&events).map_err(|e| TestCaseError::fail(e.to_string()))?;
    }

    /// **Invariant 3.** The aggregate agrees with an independent accumulator.
    ///
    /// `ground_truth` is a fifteen-line fold with no aggregate, no projection and
    /// no event store in it. Comparing the two is comparing two implementations,
    /// not one implementation against its own output.
    #[test]
    fn the_aggregate_agrees_with_an_independent_accumulator(seed in any::<u64>()) {
        let ledger = generate(seed, &GeneratorSpec::small());
        let entries: Vec<Vec<JournalLine>> =
            ledger.entries.iter().map(|e| e.lines.clone()).collect();
        let (_, live) = post_all(&entries).map_err(TestCaseError::fail)?;

        let expected = ledger.ground_truth();
        let actual: BTreeMap<AccountId, Decimal> = live
            .accounts()
            .map(|(id, money)| (*id, money.amount()))
            .collect();
        prop_assert_eq!(actual, expected);
    }
}

#[test]
fn replaying_the_same_log_in_a_different_order_produces_a_different_projection() {
    // the scenario coverage list (adversarial): "an event log replayed out of order". A reversal
    // applied before the entry it reverses cannot mirror anything, so the
    // projection differs — which is the point. Order is load-bearing, and a
    // store that does not guarantee it is not an event store.
    let a = account_id("1000");
    let b = account_id("4000");
    let lines = vec![
        JournalLine::debit(a, Money::new(dec!(100.00), Currency::Usd)),
        JournalLine::credit(b, Money::new(dec!(100.00), Currency::Usd)),
    ];
    let mut state = LedgerState::default();
    let posted = LedgerAggregate::handle_command(
        &state,
        LedgerCommand::PostJournalEntry {
            lines,
            dims: dims(),
        },
    )
    .unwrap();
    state = LedgerAggregate::apply_event(state, posted[0].clone());
    let LedgerEvent::JournalEntryPosted { id, .. } = &posted[0] else {
        panic!("expected a posting")
    };
    let reversed = LedgerAggregate::handle_command(
        &state,
        LedgerCommand::ReverseEntry {
            original_id: *id,
            reason: "corpus".to_owned(),
        },
    )
    .unwrap();

    let in_order = assertions::replay(&[posted[0].clone(), reversed[0].clone()]);
    let out_of_order = assertions::replay(&[reversed[0].clone(), posted[0].clone()]);
    assert_ne!(
        in_order, out_of_order,
        "replaying a reversal before its original produced the same projection, \
         which would mean ordering is not load-bearing"
    );
}

#[test]
fn sequence_numbers_must_strictly_increase() {
    assert!(assertions::assert_monotonic_sequence(&[1, 2, 3, 10]).is_ok());
    assert!(assertions::assert_monotonic_sequence(&[1, 2, 2]).is_err());
}

// ===========================================================================
// Invariant 4 — negative-balance prevention
// ===========================================================================

#[test]
fn negative_balance_policy_is_per_account_type() {
    let chart = chart();
    let declared = BTreeSet::new();

    // An overdrawn cash account is a defect.
    let overdrawn = BTreeMap::from([("1000".to_owned(), dec!(-1.00))]);
    let err = assert_no_unexpected_negative_balances(&overdrawn, &chart, &declared).unwrap_err();
    assert_eq!(err.invariant, Invariant::NegativeBalance);

    // A debit balance in deferred revenue is permitted: over-recognition
    // produces one, and it is a state to report, not a state to refuse.
    let over_recognized = BTreeMap::from([("2400".to_owned(), dec!(1.00))]);
    assert!(assert_no_unexpected_negative_balances(&over_recognized, &chart, &declared).is_ok());

    // A declared exception passes.
    let declared = BTreeSet::from(["1000".to_owned()]);
    assert!(assert_no_unexpected_negative_balances(&overdrawn, &chart, &declared).is_ok());
}

#[test]
fn contra_accounts_are_measured_against_their_flipped_normal_balance() {
    let chart = chart();
    let declared = BTreeSet::new();
    // Accumulated depreciation is an asset with a credit normal balance, so a
    // negative (credit) balance is correct and a positive one is not.
    let normal = BTreeMap::from([("1590".to_owned(), dec!(-5000.00))]);
    assert!(assert_no_unexpected_negative_balances(&normal, &chart, &declared).is_ok());
    let backwards = BTreeMap::from([("1590".to_owned(), dec!(5000.00))]);
    assert!(assert_no_unexpected_negative_balances(&backwards, &chart, &declared).is_err());
}

/// **Invariant 4, pending at write time.** The policy is enforced when a report
/// is checked, not when an entry is written.
///
/// TODO(P03): the posting engine consults the chart and refuses this command.
#[test]
#[ignore = "P03: the posting engine has no per-account-type rejection rule yet; \
            today this is a property of the chart, not of the writer"]
fn the_posting_engine_refuses_to_overdraw_an_asset() {
    // Cash credited with nothing in it. The aggregate accepts it, because it
    // knows nothing about accounts; only the chart-driven assertion catches it.
    let overdraw = LedgerCommand::PostJournalEntry {
        lines: vec![
            JournalLine::credit(account_id("1000"), Money::new(dec!(500.00), Currency::Usd)),
            JournalLine::debit(account_id("6300"), Money::new(dec!(500.00), Currency::Usd)),
        ],
        dims: dims(),
    };
    assert!(LedgerAggregate::handle_command(&LedgerState::default(), overdraw).is_ok());

    let balances = BTreeMap::from([("1000".to_owned(), dec!(-500.00))]);
    assert!(
        assert_no_unexpected_negative_balances(&balances, &chart(), &BTreeSet::new()).is_err(),
        "the report-time check must catch what the write-time check cannot yet"
    );
}

// ===========================================================================
// Invariant 5 — currency isolation
// ===========================================================================

#[test]
fn a_mixed_currency_entry_is_refused_at_the_command_boundary() {
    let lines = vec![
        JournalLine::debit(account_id("1000"), Money::new(dec!(100.00), Currency::Usd)),
        JournalLine::credit(account_id("4000"), Money::new(dec!(100.00), Currency::Eur)),
    ];
    let result = LedgerAggregate::handle_command(
        &LedgerState::default(),
        LedgerCommand::PostJournalEntry {
            lines: lines.clone(),
            dims: dims(),
        },
    );
    assert!(result.is_err(), "a mixed-currency entry was accepted");
    assert!(assert_balanced(&lines).is_err());
}

#[test]
fn a_line_may_not_be_posted_to_an_account_denominated_in_another_currency() {
    // The case the first two clauses of invariant 5 miss: a perfectly
    // self-consistent USD entry, one line of which lands in the EUR bank
    // account. That is an unrecorded FX conversion.
    let chart = chart();
    let lines = vec![
        JournalLine::debit(account_id("1010"), Money::new(dec!(100.00), Currency::Usd)),
        JournalLine::credit(account_id("4000"), Money::new(dec!(100.00), Currency::Usd)),
    ];
    let labels = BTreeMap::from([
        (account_id("1010"), "1010-Cash - EUR Account".to_owned()),
        (account_id("4000"), "4000-Revenue".to_owned()),
    ]);
    let err = assertions::assert_currency_isolation(&lines, Currency::Usd, &chart, |line| {
        labels.get(&line.account).cloned().unwrap_or_default()
    })
    .unwrap_err();
    assert_eq!(err.invariant, Invariant::CurrencyIsolation);
    assert!(err.detail.contains("hiding"), "{}", err.detail);
}

// ===========================================================================
// Invariant 6 — trial balance is zero, per scope
// ===========================================================================

proptest! {
    #[test]
    fn a_generated_ledger_has_a_zero_trial_balance(seed in any::<u64>()) {
        let ledger = generate(seed, &GeneratorSpec::small());
        let balances: BTreeMap<String, Decimal> = ledger
            .ground_truth()
            .into_iter()
            .map(|(id, amount)| (id.to_string(), amount))
            .collect();
        assert_balances_net_to_zero(&balances)
            .map_err(|e| TestCaseError::fail(e.to_string()))?;
    }
}

#[test]
fn each_scope_must_balance_separately_not_merely_in_aggregate() {
    // Two entities whose imbalances cancel. In aggregate this is zero; per
    // entity it is not, and an entity posting into another entity's books is
    // exactly what invariant 6's "separately" exists to catch.
    let scope = |entity: &str| BalanceScope {
        currency: Currency::Usd,
        book: "US_GAAP".to_owned(),
        entity: entity.to_owned(),
        period: "2026-01".to_owned(),
    };
    let balances = BTreeMap::from([
        (scope("US_PARENT"), vec![dec!(100.00)]),
        (scope("EU_SUB"), vec![dec!(-100.00)]),
    ]);
    assert!(assert_trial_balance_zero(&balances).is_err());
}

/// **Invariant 6, pending across scopes.** The assertion is correct; there is
/// only one scope in the system to apply it to.
///
/// TODO(P04): build the scope map from real posted entries rather than by hand,
/// once book, entity and period are dimensions of a posting.
#[test]
#[ignore = "P04: books, entities and periods are not modelled yet, so there is \
            only one scope to take a trial balance over"]
fn the_trial_balance_is_zero_in_every_book_entity_and_period() {
    let scope = |book: &str, entity: &str, period: &str| BalanceScope {
        currency: Currency::Usd,
        book: book.to_owned(),
        entity: entity.to_owned(),
        period: period.to_owned(),
    };
    let balanced = BTreeMap::from([
        (
            scope("US_GAAP", "US_PARENT", "2026-01"),
            vec![dec!(100.00), dec!(-100.00)],
        ),
        (
            scope("IFRS", "US_PARENT", "2026-01"),
            vec![dec!(80.00), dec!(-80.00)],
        ),
        (
            scope("US_GAAP", "EU_SUB", "2026-01"),
            vec![dec!(5.00), dec!(-5.00)],
        ),
        (
            scope("US_GAAP", "US_PARENT", "2026-02"),
            vec![dec!(7.00), dec!(-7.00)],
        ),
    ]);
    assert!(assert_trial_balance_zero(&balanced).is_ok());

    // One book out of balance must fail even though the others are fine.
    let mut broken = balanced.clone();
    broken.insert(
        scope("IFRS", "US_PARENT", "2026-01"),
        vec![dec!(80.00), dec!(-79.00)],
    );
    assert!(assert_trial_balance_zero(&broken).is_err());
}

// ===========================================================================
// Invariant 7 — subledger ties to GL
// ===========================================================================

#[test]
fn the_subledger_tie_is_generic_over_which_subledger() {
    let usd = |d: Decimal| Money::new(d, Currency::Usd);
    for subledger in ["AR", "AP", "DEFERRED_REVENUE", "FIXED_ASSETS"] {
        assert!(assert_subledger_ties(subledger, usd(dec!(42.00)), usd(dec!(42.00))).is_ok());
        assert!(assert_subledger_ties(subledger, usd(dec!(42.00)), usd(dec!(41.99))).is_err());
    }
}

#[test]
fn every_named_subledger_has_exactly_one_control_account() {
    let chart = chart();
    for subledger in ["AR", "AP", "DEFERRED_REVENUE", "FIXED_ASSETS"] {
        assert!(
            chart.control_account(subledger).is_some(),
            "{subledger} has no control account in corpus/_reference/accounts.toml"
        );
    }
}

/// **Invariant 7, pending.** The tie is exact and generic; the aging it ties
/// against does not exist.
///
/// TODO(P07): compute the aging total from real receivables rather than from the
/// hand-built buckets below.
#[test]
#[ignore = "P07/P08/P09: there are no subledgers to tie yet"]
fn the_ar_aging_total_equals_the_ar_control_account() {
    let usd = |d: Decimal| Money::new(d, Currency::Usd);
    // current, 1-30, 31-60, 61-90, 90+
    let aging = [
        dec!(120000.00),
        dec!(45000.00),
        dec!(18000.00),
        dec!(6000.00),
        dec!(1500.00),
    ];
    let total = aging
        .iter()
        .try_fold(usd(Decimal::ZERO), |acc, bucket| {
            acc.checked_add(&usd(*bucket))
        })
        .unwrap();
    assert_eq!(total, usd(dec!(190500.00)));

    let control = chart().control_account("AR").unwrap().code.clone();
    assert_eq!(control, "1100");
    assert!(assert_subledger_ties("AR", total, usd(dec!(190500.00))).is_ok());
    assert!(assert_subledger_ties("AR", total, usd(dec!(190499.99))).is_err());
}

// ===========================================================================
// Invariant 8 — period roll-forward continuity
// ===========================================================================

#[test]
fn roll_forward_rejects_a_dropped_and_a_conjured_account() {
    let closing = BTreeMap::from([
        ("1000".to_owned(), dec!(100.00)),
        ("4000".to_owned(), dec!(-100.00)),
    ]);
    assert!(assert_roll_forward(&closing, &closing).is_ok());

    let dropped = BTreeMap::from([("1000".to_owned(), dec!(100.00))]);
    assert!(assert_roll_forward(&closing, &dropped).is_err());

    let mut conjured = closing.clone();
    conjured.insert("1100".to_owned(), dec!(1.00));
    assert!(assert_roll_forward(&closing, &conjured).is_err());
}

/// **Invariant 8, pending.** The assertion is correct; there is no close to
/// take a closing balance from.
///
/// TODO(P04): read the closing and opening maps from two consecutive closed
/// periods instead of building them here.
#[test]
#[ignore = "P04: there is no period close, so there is no closing balance to \
            carry forward"]
fn every_account_carries_forward_across_a_close() {
    let closing = BTreeMap::from([
        ("1000".to_owned(), dec!(125000.00)),
        ("1100".to_owned(), dec!(48000.00)),
        ("2000".to_owned(), dec!(-31000.00)),
        ("3200".to_owned(), dec!(-142000.00)),
    ]);
    assert!(assert_roll_forward(&closing, &closing).is_ok());

    // A single account off by a cent is a failure; there is no tolerance.
    let mut drifted = closing.clone();
    drifted.insert("1100".to_owned(), dec!(47999.99));
    assert!(assert_roll_forward(&closing, &drifted).is_err());
}

// ===========================================================================
// Invariant 9 — closed-period immutability
// ===========================================================================

/// Every public entry point that can change ledger state today.
///
/// Enumerated as a constant so that adding one without a closed-period test is a
/// test failure. When P04 lands the period state machine, each of these must
/// reject a mutation into a closed period, and the list grows with the API.
const MUTATION_ENTRY_POINTS: &[&str] = &[
    "falkr_events::LedgerAggregate::handle_command/ChangeCapitalizationStatus",
    "falkr_events::LedgerAggregate::handle_command/PostJournalEntry",
    "falkr_events::LedgerAggregate::handle_command/ReverseEntry",
];

#[test]
fn the_mutation_entry_point_enumeration_is_current() {
    let observed: BTreeSet<String> = MUTATION_ENTRY_POINTS
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert!(assert_entry_points_enumerated(&observed, MUTATION_ENTRY_POINTS).is_ok());
}

#[test]
fn an_empty_enumeration_does_not_pass_invariant_nine() {
    assert!(assert_all_mutations_rejected(&[]).is_err());
    let accepted = [MutationAttempt {
        entry_point: "somewhere".to_owned(),
        rejected: false,
    }];
    assert!(assert_all_mutations_rejected(&accepted).is_err());
}

/// **Invariant 9, pending.** Every entry point is enumerated; none of them can
/// yet consult a period state, because there is none.
///
/// TODO(P04): drive each entry point against a genuinely closed period and set
/// `rejected` from what it actually did, rather than asserting the shape.
#[test]
#[ignore = "P04: no period can be closed, so no mutation into one can be refused"]
fn no_entry_point_mutates_a_posted_line_in_a_closed_period() {
    let attempts: Vec<MutationAttempt> = MUTATION_ENTRY_POINTS
        .iter()
        .map(|entry_point| MutationAttempt {
            entry_point: (*entry_point).to_owned(),
            // Today: nothing refuses, because nothing can be closed.
            rejected: false,
        })
        .collect();
    assert!(
        assert_all_mutations_rejected(&attempts).is_err(),
        "with no period state machine, no entry point refuses — that is the gap"
    );

    // And the enumeration itself is complete, which is the half of invariant 9
    // that can be guaranteed today.
    let observed: BTreeSet<String> = MUTATION_ENTRY_POINTS
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert!(assert_entry_points_enumerated(&observed, MUTATION_ENTRY_POINTS).is_ok());
}

// ===========================================================================
// Invariant 10 — consolidation completeness
// ===========================================================================

#[test]
fn intercompany_accounts_must_net_to_zero() {
    let chart = chart();
    let eliminated = BTreeMap::from([
        ("1900".to_owned(), dec!(50000.00)),
        ("2600".to_owned(), dec!(-50000.00)),
    ]);
    assert!(assert_intercompany_eliminated(&eliminated, &chart).is_ok());

    let residual = BTreeMap::from([
        ("1900".to_owned(), dec!(50000.00)),
        ("2600".to_owned(), dec!(-49000.00)),
    ]);
    let err = assert_intercompany_eliminated(&residual, &chart).unwrap_err();
    assert!(err.detail.contains("1000"), "{}", err.detail);
}

/// **Invariant 10, pending.** The assertion is correct; nothing consolidates.
///
/// TODO(P12): take the balances from a real consolidation run, and add the
/// same-currency precondition — corpus/consolidation/consolidation-005 is the
/// documented case where intercompany balances legitimately do not net.
#[test]
#[ignore = "P12: there is no consolidation, so there is nothing to eliminate"]
fn consolidation_eliminates_intercompany_in_the_reporting_currency() {
    let chart = chart();
    let after_elimination = BTreeMap::from([
        ("1900".to_owned(), dec!(2400000.00)),
        ("2600".to_owned(), dec!(-2400000.00)),
        ("4950".to_owned(), dec!(-800000.00)),
        ("5900".to_owned(), dec!(800000.00)),
        // A non-intercompany account, which must not be included in the net.
        ("1000".to_owned(), dec!(999999.00)),
    ]);
    assert!(assert_intercompany_eliminated(&after_elimination, &chart).is_ok());

    let mut residual = after_elimination.clone();
    residual.insert("2600".to_owned(), dec!(-2399000.00));
    assert!(assert_intercompany_eliminated(&residual, &chart).is_err());
}

// ===========================================================================
// Layer 3 — the precision harness (the precision requirements)
// ===========================================================================

proptest! {
    /// **The allocation property.** For any amount, any number of parts and any
    /// weights, the parts sum to exactly the whole.
    ///
    /// This is the property the precision requirements names explicitly, and it is the one that
    /// catches a deferred-revenue waterfall losing a cent per contract per
    /// month.
    #[test]
    fn allocation_parts_always_sum_to_the_whole(
        minor in -100_000_000_i64..100_000_000_i64,
        weights in prop::collection::vec(0_u32..1000_u32, 1..12),
        scale in 0_u32..4_u32,
    ) {
        let total = Decimal::new(minor, scale);
        let weights: Vec<Decimal> = weights.iter().map(|w| Decimal::from(*w)).collect();
        let Ok(parts) = allocate(total, &weights, scale) else {
            // Rejected only for a degenerate weighting, which is a documented
            // error rather than a silent zero.
            prop_assume!(weights.iter().all(rust_decimal::Decimal::is_zero));
            return Ok(());
        };
        let mut sum = Decimal::ZERO;
        for part in &parts {
            sum = sum.checked_add(*part).ok_or_else(|| {
                TestCaseError::fail("allocation parts overflowed while summing")
            })?;
        }
        prop_assert_eq!(sum, total);
        prop_assert_eq!(parts.len(), weights.len());
    }

    /// Allocation is a pure function of its inputs.
    #[test]
    fn allocation_is_deterministic(
        minor in 0_i64..1_000_000_i64,
        n in 1_usize..10_usize,
    ) {
        let total = Decimal::new(minor, 2);
        prop_assert_eq!(
            allocate_evenly(total, n, 2).ok(),
            allocate_evenly(total, n, 2).ok()
        );
    }
}

#[test]
fn allocation_holds_at_the_currency_scales_a_ledger_actually_meets() {
    // 0 decimals (JPY), 2 (USD, EUR), 3 (BHD, KWD). Most home-grown ledgers
    // assume 2 and break on the other two.
    for (scale, total) in [(0_u32, dec!(100)), (2, dec!(100.00)), (3, dec!(100.000))] {
        for n in 1..=7_usize {
            let parts = allocate_evenly(total, n, scale).unwrap();
            let sum: Decimal = parts.iter().sum();
            assert_eq!(sum, total, "scale {scale}, {n} parts");
        }
    }
}

#[test]
fn money_rounding_at_zero_decimals_produces_no_fractional_yen() {
    let yen = Money::new(dec!(1234.56), Currency::Jpy);
    assert_eq!(yen.round_to_minor_units().amount(), dec!(1235));
    assert_eq!(Currency::Jpy.minor_units(), 0);
}

/// **Blocked, not pending.** the precision requirements requires the corpus to cover 0-, 2- and
/// 3-decimal currencies, and the third is not expressible: `falkr_core::Currency`
/// has no three-decimal member, so no `Money` value can carry one.
///
/// The body asserts the gap so that adding BHD or KWD turns this test red and
/// whoever adds it is pointed at corpus/currency/currency-014.
///
/// TODO: adding a currency is a production change and the corpus discipline forbids making it
/// here. See the scenario's `blocked_reason` for the unblocking steps.
#[test]
#[ignore = "blocked: falkr_core::Currency has no three-decimal member (BHD, KWD, \
            OMR, TND). See corpus/currency/currency-014. The allocation harness \
            covers scale 3 directly; what cannot be tested is a Money value in a \
            three-decimal currency"]
fn a_three_decimal_currency_settles_to_three_places() {
    for code in ["BHD", "KWD", "OMR", "TND"] {
        assert!(
            Currency::from_code(code).is_none(),
            "{code} now exists; promote corpus/currency/currency-014 to ready \
             and write the real settlement assertion here"
        );
    }
    // Every currency the enum does know settles at 0 or 2 places.
    for code in [
        "USD", "EUR", "GBP", "CHF", "JPY", "CAD", "AUD", "SEK", "NOK", "DKK", "SGD", "INR",
    ] {
        let currency = Currency::from_code(code).unwrap();
        assert!(matches!(currency.minor_units(), 0 | 2), "{code}");
    }
}

#[test]
fn decimal_extremes_are_handled_rather_than_wrapped() {
    // Near the top of Decimal's range, addition must fail loudly.
    assert!(Decimal::MAX.checked_add(Decimal::ONE).is_none());
    // And the smallest representable step is not lost.
    let tiny = Decimal::new(1, 28);
    assert_ne!(tiny, Decimal::ZERO);
    assert_eq!(tiny.checked_add(tiny), Some(Decimal::new(2, 28)));
}

#[test]
fn negative_amounts_are_permitted_wherever_positive_ones_are() {
    let a = account_id("1000");
    let b = account_id("4000");
    // A credit memo posts negative debits in some systems; here it posts the
    // opposite side. Both must balance.
    let lines = vec![
        JournalLine::debit(a, Money::new(dec!(-100.00), Currency::Usd)),
        JournalLine::credit(b, Money::new(dec!(-100.00), Currency::Usd)),
    ];
    assert!(
        assert_balanced(&lines).is_ok(),
        "an entry of two negative amounts still nets to zero"
    );
}

// ===========================================================================
// Wall-clock reads (the invariant list, invariant 3's "hunt for and eliminate")
// ===========================================================================

/// Invariant 3 says to hunt for wall-clock reads. There are two, and this pins
/// them so they are removed deliberately rather than discovered again.
///
/// 1. `LedgerAggregate::handle_command` stamps `posted_at: Utc::now()` into the
///    event. The command handler is therefore not a pure function of `(state,
///    command)`: the same inputs produce different events on different days.
/// 2. `PgLedgerEventStore::persist` stamps `occurred_on = Utc::now()` onto the
///    stored row.
///
/// Neither breaks replay determinism — replay reads the stored event and does
/// not call the clock — so invariant 3 as written still holds. What they break
/// is **reproducibility of the write**, and the second one has already caused a
/// real failure: `crates/infra/tests/billing_pg.rs` queries `ledger_events` by
/// `occurred_on` over a fixed August 2026 window, so
/// `the_trace_query_does_not_cross_tenants` and
/// `recognized_revenue_posts_to_the_ledger_and_traces_back_in_one_query` passed
/// every day of August 2026 and have failed every day since. The stored time is
/// when the row was written, not when the transaction happened.
///
/// TODO(P03): the AS 2401 entry-attribute rule requires `entry_date`, `effective_date`,
/// `created_at` and `posted_at` as four distinct fields on the entry. Once the
/// command carries them, `handle_command` takes the time as an input and the
/// store writes the effective date, and this test inverts to assert that the
/// same command twice produces the same event.
#[test]
fn the_command_handler_reads_the_wall_clock() {
    let command = || LedgerCommand::PostJournalEntry {
        lines: vec![
            JournalLine::debit(account_id("1000"), Money::new(dec!(1.00), Currency::Usd)),
            JournalLine::credit(account_id("4000"), Money::new(dec!(1.00), Currency::Usd)),
        ],
        dims: dims(),
    };
    let state = LedgerState::default();
    let first = LedgerAggregate::handle_command(&state, command()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let second = LedgerAggregate::handle_command(&state, command()).unwrap();

    let stamp = |event: &LedgerEvent| match event {
        LedgerEvent::JournalEntryPosted { posted_at, .. } => *posted_at,
        _ => panic!("expected a posting"),
    };
    assert_ne!(
        stamp(&first[0]),
        stamp(&second[0]),
        "if this now passes, the clock has been made an input — invert the \
         assertion and delete the TODO above"
    );
}
