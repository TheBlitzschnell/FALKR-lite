//! The ten invariants, as reusable functions.
//!
//! Every one of these is callable from any crate's tests, not just from the
//! corpus runner. That is the point: an invariant that only the corpus checks
//! is an invariant that a unit test in `billing` can quietly violate.
//!
//! ## Which of these can pass today
//!
//! Invariants 1, 3 and 5 hold against the aggregate that exists now. Invariants
//! 2, 4 and 6–10 describe behaviour that later phases deliver; they are written
//! and exercised against hand-built inputs so that the *assertion* is correct
//! before the feature exists, and the suites that would need the feature are
//! `#[ignore]`d with the phase number that unblocks them. Writing them first is
//! the whole point of the corpus — they are the acceptance criteria for P04, P05, P07
//! and P12, expressed as code rather than as a checklist.
//!
//! ## Why every sum here is `Decimal::checked_add`
//!
//! `Decimal` arithmetic with `+` panics on overflow in debug and can saturate in
//! release. Either way, a summation that silently stops being a sum is a wrong
//! answer that balances — the worst possible failure in a ledger, because it
//! looks correct. Every accumulation below is checked and reports
//! [`Invariant::Balanced`] with an overflow detail instead.

use std::collections::{BTreeMap, BTreeSet};

use falkr_core::{Currency, Money};
use falkr_events::{JournalLine, LedgerEvent, LedgerState};
use rust_decimal::Decimal;

use crate::chart::{Account, ChartOfAccounts, NormalBalance};

/// The ten invariants, numbered as in the invariant list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Invariant {
    /// 1. Every transaction's signed line amounts sum to exactly zero.
    Balanced,
    /// 2. Two posts with the same idempotency key produce exactly one event.
    Idempotent,
    /// 3. Sequence numbers increase; replay reproduces the live projection.
    DeterministicReplay,
    /// 4. Negative balances are prevented per account type.
    NegativeBalance,
    /// 5. A transaction is single-currency, and matches its accounts.
    CurrencyIsolation,
    /// 6. Trial balance is zero per currency, book, entity and period.
    TrialBalanceZero,
    /// 7. Each subledger total equals its GL control account.
    SubledgerTies,
    /// 8. Closing balance of period N equals opening balance of N+1.
    RollForward,
    /// 9. No code path mutates a posted line in a closed period.
    ClosedPeriodImmutable,
    /// 10. Intercompany accounts net to zero after elimination.
    ConsolidationComplete,
}

impl Invariant {
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::Balanced => 1,
            Self::Idempotent => 2,
            Self::DeterministicReplay => 3,
            Self::NegativeBalance => 4,
            Self::CurrencyIsolation => 5,
            Self::TrialBalanceZero => 6,
            Self::SubledgerTies => 7,
            Self::RollForward => 8,
            Self::ClosedPeriodImmutable => 9,
            Self::ConsolidationComplete => 10,
        }
    }
}

impl core::fmt::Display for Invariant {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invariant {}", self.number())
    }
}

/// A broken invariant, naming which one and why.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{invariant} violated: {detail}")]
pub struct InvariantViolation {
    pub invariant: Invariant,
    pub detail: String,
}

impl InvariantViolation {
    fn new(invariant: Invariant, detail: impl Into<String>) -> Self {
        Self {
            invariant,
            detail: detail.into(),
        }
    }
}

type Checked = Result<(), InvariantViolation>;

/// Sums exact decimals, refusing to saturate.
fn checked_sum<I: IntoIterator<Item = Decimal>>(
    values: I,
    invariant: Invariant,
) -> Result<Decimal, InvariantViolation> {
    let mut total = Decimal::ZERO;
    for value in values {
        total = total.checked_add(value).ok_or_else(|| {
            InvariantViolation::new(
                invariant,
                format!("summation overflowed Decimal at {total} + {value}"),
            )
        })?;
    }
    Ok(total)
}

// --------------------------------------------------------------------------
// 1. Balanced postings
// --------------------------------------------------------------------------

/// **Invariant 1.** Every transaction's signed line amounts sum to exactly
/// `Decimal::ZERO`.
///
/// A saturating sum is treated as a violation rather than a result: a `Decimal`
/// sum that overflows produces a number that may well be zero, and "balanced by
/// overflow" is indistinguishable from "balanced" to every downstream report.
///
/// # Errors
///
/// If any line carries both or neither side, if the lines span more than one
/// currency, if the signed sum is non-zero, or if the summation overflows.
pub fn assert_balanced(lines: &[JournalLine]) -> Checked {
    if lines.is_empty() {
        return Err(InvariantViolation::new(
            Invariant::Balanced,
            "an entry with no lines cannot be balanced; it is malformed",
        ));
    }
    let mut signed = Vec::with_capacity(lines.len());
    let mut currencies = BTreeSet::new();
    for line in lines {
        let amount = line.signed_amount().ok_or_else(|| {
            InvariantViolation::new(
                Invariant::Balanced,
                format!(
                    "line on account {} carries both or neither of debit/credit",
                    line.account
                ),
            )
        })?;
        currencies.insert(amount.currency());
        signed.push(amount.amount());
    }
    if currencies.len() > 1 {
        return Err(InvariantViolation::new(
            Invariant::Balanced,
            format!(
                "entry spans {} currencies; see invariant 5",
                currencies.len()
            ),
        ));
    }
    let total = checked_sum(signed, Invariant::Balanced)?;
    if total.is_zero() {
        Ok(())
    } else {
        Err(InvariantViolation::new(
            Invariant::Balanced,
            format!("signed lines sum to {total}, not zero"),
        ))
    }
}

// --------------------------------------------------------------------------
// 2. Idempotency
// --------------------------------------------------------------------------

/// **Invariant 2.** A given idempotency key must correspond to exactly one
/// persisted event.
///
/// The interesting half of this invariant is not the assertion, it is where the
/// count comes from: **the database is the dedup source of truth, not an
/// in-memory map**, so the count must be read back from Postgres after two
/// *concurrent* connections have both attempted the write. Two sequential calls
/// on one connection would pass against an in-process `HashSet` that provides no
/// protection at all in production.
///
/// See `tests/idempotency_pg.rs` for the concurrent harness.
///
/// # Errors
///
/// If `persisted` is anything other than 1.
pub fn assert_single_event_for_key(key: &str, persisted: usize) -> Checked {
    match persisted {
        1 => Ok(()),
        0 => Err(InvariantViolation::new(
            Invariant::Idempotent,
            format!("idempotency key {key:?} persisted no event at all"),
        )),
        n => Err(InvariantViolation::new(
            Invariant::Idempotent,
            format!("idempotency key {key:?} persisted {n} events; expected exactly 1"),
        )),
    }
}

// --------------------------------------------------------------------------
// 3. Monotonic ordering and deterministic replay
// --------------------------------------------------------------------------

/// **Invariant 3a.** Sequence numbers strictly increase within an aggregate.
///
/// # Errors
///
/// If any sequence number is not strictly greater than its predecessor.
pub fn assert_monotonic_sequence(sequences: &[i32]) -> Checked {
    for pair in sequences.windows(2) {
        let [previous, next] = pair else { continue };
        if next <= previous {
            return Err(InvariantViolation::new(
                Invariant::DeterministicReplay,
                format!("sequence went {previous} -> {next}; must strictly increase"),
            ));
        }
    }
    Ok(())
}

/// **Invariant 3b.** Replaying the log into a fresh projection reproduces the
/// live projection bit-for-bit.
///
/// Replay is run **twice** and both runs are compared with each other as well
/// as with the live state. One replay proves the fold is correct today; two
/// prove it does not depend on anything outside the events — a wall-clock read,
/// a hash-map iteration order, an uninitialised accumulator. Those are precisely
/// the defects that make a ledger irreproducible, and they are invisible to a
/// single-run comparison.
///
/// # Errors
///
/// If either replay differs from the live projection or from the other.
pub fn assert_replay_stable(events: &[LedgerEvent], live: &LedgerState) -> Checked {
    let first = replay(events);
    let second = replay(events);
    if first != second {
        return Err(InvariantViolation::new(
            Invariant::DeterministicReplay,
            "two replays of the same log produced different projections; something \
             outside the event stream is being read during replay",
        ));
    }
    if &first != live {
        return Err(InvariantViolation::new(
            Invariant::DeterministicReplay,
            "replaying the log did not reproduce the live projection",
        ));
    }
    Ok(())
}

/// Folds an event log into a fresh [`LedgerState`].
#[must_use]
pub fn replay(events: &[LedgerEvent]) -> LedgerState {
    use esrs::Aggregate as _;
    events.iter().fold(LedgerState::default(), |state, event| {
        falkr_events::LedgerAggregate::apply_event(state, event.clone())
    })
}

/// **Invariant 3c.** Events survive a serialization round trip unchanged.
///
/// The stored form is what actually gets replayed in production — `infra`'s
/// event store reads JSONB back out of Postgres — so a projection that only
/// replays correctly from in-memory events is not replayable at all.
///
/// # Errors
///
/// If any event fails to serialize, fails to deserialize, or does not compare
/// equal to itself after the round trip.
pub fn assert_events_round_trip(events: &[LedgerEvent]) -> Checked {
    for event in events {
        let json = serde_json::to_string(event).map_err(|e| {
            InvariantViolation::new(
                Invariant::DeterministicReplay,
                format!("event failed to serialize: {e}"),
            )
        })?;
        let back: LedgerEvent = serde_json::from_str(&json).map_err(|e| {
            InvariantViolation::new(
                Invariant::DeterministicReplay,
                format!("event failed to deserialize from {json}: {e}"),
            )
        })?;
        if &back != event {
            return Err(InvariantViolation::new(
                Invariant::DeterministicReplay,
                format!("event changed across a serialization round trip: {json}"),
            ));
        }
    }
    Ok(())
}

// --------------------------------------------------------------------------
// 4. Negative-balance prevention
// --------------------------------------------------------------------------

/// **Invariant 4.** No account ends on the side opposite its normal balance
/// unless its type permits it or the caller has declared the exception.
///
/// Configurable per account type rather than globally: an overdrawn cash account
/// is a bug, a debit balance in deferred revenue is a real state that a genuine
/// over-recognition produces, and a policy that forbids both would be wrong half
/// the time. The per-type default lives in
/// [`crate::chart::AccountKind::rejects_opposite_sign_by_default`]; the per-account
/// override lives in `corpus/_reference/accounts.toml`.
///
/// # Errors
///
/// If any account not in `declared` sits on the wrong side.
pub fn assert_no_unexpected_negative_balances(
    balances: &BTreeMap<String, Decimal>,
    chart: &ChartOfAccounts,
    declared: &BTreeSet<String>,
) -> Checked {
    for (reference, amount) in balances {
        let Some(account) = chart.resolve(reference) else {
            return Err(InvariantViolation::new(
                Invariant::NegativeBalance,
                format!("account {reference:?} is not in the chart"),
            ));
        };
        if on_normal_side(account, *amount)
            || account.permits_opposite_sign()
            || declared.contains(reference)
        {
            continue;
        }
        return Err(InvariantViolation::new(
            Invariant::NegativeBalance,
            format!(
                "{reference} ends at {amount}, opposite its normal balance ({})",
                account.normal_balance().as_str()
            ),
        ));
    }
    Ok(())
}

fn on_normal_side(account: &Account, amount: Decimal) -> bool {
    match account.normal_balance() {
        NormalBalance::Debit => !amount.is_sign_negative(),
        NormalBalance::Credit => amount.is_sign_negative() || amount.is_zero(),
    }
}

// --------------------------------------------------------------------------
// 5. Currency isolation
// --------------------------------------------------------------------------

/// **Invariant 5.** A single transaction is single-currency, every line matches
/// the transaction currency, and every line matches its account's currency.
///
/// The third clause is the one that does the work. The first two are satisfied
/// by any entry built from one `Money` currency; the third catches a USD line
/// posted to a EUR-denominated bank account, which is how an FX conversion hides
/// inside a posting instead of being an explicit, rate-stamped step.
///
/// # Errors
///
/// If any line's currency differs from `transaction_currency`, or from the
/// currency its account is restricted to.
pub fn assert_currency_isolation(
    lines: &[JournalLine],
    transaction_currency: Currency,
    chart: &ChartOfAccounts,
    account_label: impl Fn(&JournalLine) -> String,
) -> Checked {
    for line in lines {
        let Some(amount) = line.signed_amount() else {
            return Err(InvariantViolation::new(
                Invariant::CurrencyIsolation,
                "line carries both or neither of debit/credit",
            ));
        };
        if amount.currency() != transaction_currency {
            return Err(InvariantViolation::new(
                Invariant::CurrencyIsolation,
                format!(
                    "line is in {} but the transaction is in {transaction_currency}",
                    amount.currency()
                ),
            ));
        }
        let label = account_label(line);
        let Some(account) = chart.resolve(&label) else {
            continue;
        };
        if let Some(restricted) = &account.currency {
            let restricted = Currency::from_code(restricted).ok_or_else(|| {
                InvariantViolation::new(
                    Invariant::CurrencyIsolation,
                    format!("account {label} is restricted to unknown currency {restricted}"),
                )
            })?;
            if restricted != amount.currency() {
                return Err(InvariantViolation::new(
                    Invariant::CurrencyIsolation,
                    format!(
                        "account {label} is denominated in {restricted} but the line is \
                         in {}; an FX conversion is hiding inside this posting",
                        amount.currency()
                    ),
                ));
            }
        }
    }
    Ok(())
}

// --------------------------------------------------------------------------
// 6. Trial balance is zero
// --------------------------------------------------------------------------

/// The scope a trial balance is taken over. Zero holds within each scope
/// **separately** — a group whose entities' trial balances only net to zero in
/// aggregate has an entity posting into another entity's books.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BalanceScope {
    pub currency: Currency,
    pub book: String,
    pub entity: String,
    pub period: String,
}

/// **Invariant 6.** The trial balance is exactly zero in every currency, book,
/// entity and period, taken one scope at a time.
///
/// # Errors
///
/// Naming the first scope that does not balance, and by how much.
pub fn assert_trial_balance_zero(balances: &BTreeMap<BalanceScope, Vec<Decimal>>) -> Checked {
    for (scope, amounts) in balances {
        let total = checked_sum(amounts.iter().copied(), Invariant::TrialBalanceZero)?;
        if !total.is_zero() {
            return Err(InvariantViolation::new(
                Invariant::TrialBalanceZero,
                format!(
                    "{}/{}/{}/{} is out of balance by {total}",
                    scope.entity, scope.book, scope.currency, scope.period
                ),
            ));
        }
    }
    Ok(())
}

/// The single-scope form, for a scenario that lives in one book, entity, period
/// and currency — which is most of them.
///
/// # Errors
///
/// If the amounts do not sum to zero.
pub fn assert_balances_net_to_zero(amounts: &BTreeMap<String, Decimal>) -> Checked {
    let total = checked_sum(amounts.values().copied(), Invariant::TrialBalanceZero)?;
    if total.is_zero() {
        Ok(())
    } else {
        Err(InvariantViolation::new(
            Invariant::TrialBalanceZero,
            format!("trial balance is out by {total}"),
        ))
    }
}

// --------------------------------------------------------------------------
// 7. Subledger ties to GL
// --------------------------------------------------------------------------

/// **Invariant 7.** A subledger total equals its GL control account, exactly.
///
/// One generic function rather than four near-identical ones. AR aging vs the AR
/// control, AP aging vs the AP control, the deferred-revenue waterfall vs the
/// deferred-revenue balance, and asset-register NBV vs PP&E net of accumulated
/// depreciation are the same assertion with different inputs, and writing them
/// four times is how three of them end up with slightly different tolerance
/// behaviour.
///
/// There is no tolerance. A one-cent difference between a subledger and its
/// control account is a real difference, and the usual cause is a rounding rule
/// applied in one place and not the other.
///
/// # Errors
///
/// If the two differ at all, or if they are in different currencies.
pub fn assert_subledger_ties(
    subledger: &str,
    subledger_total: Money,
    control_account_balance: Money,
) -> Checked {
    let difference = subledger_total
        .checked_sub(&control_account_balance)
        .map_err(|e| {
            InvariantViolation::new(
                Invariant::SubledgerTies,
                format!("{subledger} and its control account are in different currencies: {e}"),
            )
        })?;
    if difference.is_zero() {
        Ok(())
    } else {
        Err(InvariantViolation::new(
            Invariant::SubledgerTies,
            format!(
                "{subledger} totals {subledger_total} but its control account is \
                 {control_account_balance}; out by {difference}"
            ),
        ))
    }
}

// --------------------------------------------------------------------------
// 8. Period roll-forward continuity
// --------------------------------------------------------------------------

/// **Invariant 8.** Closing balance of period N equals opening balance of N+1,
/// for every account and every book.
///
/// Checks the union of both account sets, not the intersection: an account that
/// appears only in the closing set has been dropped from the roll-forward, and
/// an account that appears only in the opening set has been conjured. Iterating
/// one side alone silently accepts both.
///
/// # Errors
///
/// Naming the first account that does not carry forward.
pub fn assert_roll_forward(
    closing: &BTreeMap<String, Decimal>,
    opening: &BTreeMap<String, Decimal>,
) -> Checked {
    let accounts: BTreeSet<&String> = closing.keys().chain(opening.keys()).collect();
    for account in accounts {
        let closed = closing.get(account).copied().unwrap_or(Decimal::ZERO);
        let opened = opening.get(account).copied().unwrap_or(Decimal::ZERO);
        if closed != opened {
            return Err(InvariantViolation::new(
                Invariant::RollForward,
                format!("{account} closed at {closed} but opened the next period at {opened}"),
            ));
        }
    }
    Ok(())
}

// --------------------------------------------------------------------------
// 9. Closed-period immutability
// --------------------------------------------------------------------------

/// One public mutation entry point, and what it did when pointed at a closed
/// period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationAttempt {
    /// The fully-qualified path of the entry point, e.g.
    /// `falkr_events::LedgerAggregate::handle_command/PostJournalEntry`.
    pub entry_point: String,
    pub rejected: bool,
}

/// **Invariant 9.** No code path mutates a posted line in a closed period.
///
/// Takes the whole enumerated list of entry points rather than one at a time,
/// because the failure this invariant exists to catch is *an entry point nobody
/// enumerated*. The caller is responsible for that enumeration being complete;
/// [`assert_entry_points_enumerated`] is the guard that it has not silently
/// shrunk.
///
/// # Errors
///
/// If any entry point accepted the mutation.
pub fn assert_all_mutations_rejected(attempts: &[MutationAttempt]) -> Checked {
    if attempts.is_empty() {
        return Err(InvariantViolation::new(
            Invariant::ClosedPeriodImmutable,
            "no mutation entry points were attempted; an empty enumeration proves \
             nothing and passes trivially",
        ));
    }
    for attempt in attempts {
        if !attempt.rejected {
            return Err(InvariantViolation::new(
                Invariant::ClosedPeriodImmutable,
                format!(
                    "{} accepted a mutation into a closed period",
                    attempt.entry_point
                ),
            ));
        }
    }
    Ok(())
}

/// Guards the enumeration behind invariant 9 against shrinking silently.
///
/// A new public mutation entry point added without a matching closed-period
/// test would otherwise leave invariant 9 passing while the hole it describes is
/// wide open.
///
/// # Errors
///
/// If the observed set of entry points differs from the expected set.
pub fn assert_entry_points_enumerated(observed: &BTreeSet<String>, expected: &[&str]) -> Checked {
    let expected: BTreeSet<String> = expected.iter().map(|s| (*s).to_owned()).collect();
    if observed == &expected {
        return Ok(());
    }
    let added: Vec<&String> = observed.difference(&expected).collect();
    let removed: Vec<&String> = expected.difference(observed).collect();
    Err(InvariantViolation::new(
        Invariant::ClosedPeriodImmutable,
        format!(
            "the set of mutation entry points has changed; added {added:?}, removed \
             {removed:?}. Each new one needs a closed-period rejection test"
        ),
    ))
}

// --------------------------------------------------------------------------
// 10. Consolidation completeness
// --------------------------------------------------------------------------

/// **Invariant 10.** Intercompany accounts net to zero after elimination, in the
/// reporting currency.
///
/// Note "in the reporting currency": intercompany balances denominated in
/// different currencies do *not* net to zero before translation, and a group
/// that eliminates them at transaction rates leaves a residue that has to land
/// somewhere. Whether that residue is a real P&L item or a bug depends on the
/// balance's designation, which is why the corpus has a scenario for
/// intercompany FX that deliberately does not eliminate.
///
/// # Errors
///
/// If the intercompany accounts do not net to exactly zero.
pub fn assert_intercompany_eliminated(
    balances: &BTreeMap<String, Decimal>,
    chart: &ChartOfAccounts,
) -> Checked {
    let mut intercompany = Vec::new();
    for (reference, amount) in balances {
        if chart.resolve(reference).is_some_and(|a| a.intercompany) {
            intercompany.push(*amount);
        }
    }
    if intercompany.is_empty() {
        return Ok(());
    }
    let total = checked_sum(intercompany, Invariant::ConsolidationComplete)?;
    if total.is_zero() {
        Ok(())
    } else {
        Err(InvariantViolation::new(
            Invariant::ConsolidationComplete,
            format!("intercompany accounts net to {total}, not zero, after elimination"),
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "the no-unwrap rule permits unwrap in tests"
    )]

    use falkr_core::{AccountId, Currency, Money};
    use rust_decimal_macros::dec;

    use super::*;

    fn usd(d: Decimal) -> Money {
        Money::new(d, Currency::Usd)
    }

    #[test]
    fn balanced_accepts_an_entry_that_nets_to_zero() {
        let a = AccountId::new();
        let b = AccountId::new();
        let lines = vec![
            JournalLine::debit(a, usd(dec!(100.00))),
            JournalLine::credit(b, usd(dec!(100.00))),
        ];
        assert!(assert_balanced(&lines).is_ok());
    }

    #[test]
    fn balanced_rejects_an_entry_that_does_not() {
        let a = AccountId::new();
        let b = AccountId::new();
        let lines = vec![
            JournalLine::debit(a, usd(dec!(100.00))),
            JournalLine::credit(b, usd(dec!(99.99))),
        ];
        let err = assert_balanced(&lines).unwrap_err();
        assert_eq!(err.invariant, Invariant::Balanced);
        assert!(err.detail.contains("0.01"), "{}", err.detail);
    }

    #[test]
    fn balanced_rejects_an_empty_entry() {
        assert!(assert_balanced(&[]).is_err());
    }

    #[test]
    fn balanced_reports_overflow_rather_than_saturating() {
        // Decimal::MAX + Decimal::MAX has no representation. A sum that quietly
        // saturates here could land on zero and report a balanced entry.
        let a = AccountId::new();
        let b = AccountId::new();
        let lines = vec![
            JournalLine::debit(a, usd(Decimal::MAX)),
            JournalLine::debit(b, usd(Decimal::MAX)),
        ];
        let err = assert_balanced(&lines).unwrap_err();
        assert!(err.detail.contains("overflow"), "{}", err.detail);
    }

    #[test]
    fn monotonic_sequence_rejects_a_repeat_and_a_gap_backwards() {
        assert!(assert_monotonic_sequence(&[1, 2, 3]).is_ok());
        // A gap forward is fine: a store may skip numbers.
        assert!(assert_monotonic_sequence(&[1, 4, 9]).is_ok());
        assert!(assert_monotonic_sequence(&[1, 1]).is_err());
        assert!(assert_monotonic_sequence(&[3, 2]).is_err());
    }

    #[test]
    fn subledger_ties_has_no_tolerance() {
        assert!(assert_subledger_ties("AR", usd(dec!(1000.00)), usd(dec!(1000.00))).is_ok());
        assert!(assert_subledger_ties("AR", usd(dec!(1000.01)), usd(dec!(1000.00))).is_err());
        // Different currencies are a violation, not a conversion.
        assert!(
            assert_subledger_ties(
                "AR",
                usd(dec!(1000.00)),
                Money::new(dec!(1000.00), Currency::Eur)
            )
            .is_err()
        );
    }

    #[test]
    fn roll_forward_catches_an_account_that_appears_from_nowhere() {
        let closing = BTreeMap::from([("1000-Cash".to_owned(), dec!(100))]);
        let opening = BTreeMap::from([
            ("1000-Cash".to_owned(), dec!(100)),
            ("1100-Accounts Receivable".to_owned(), dec!(50)),
        ]);
        let err = assert_roll_forward(&closing, &opening).unwrap_err();
        assert!(err.detail.contains("1100"), "{}", err.detail);
    }

    #[test]
    fn closed_period_immutability_fails_on_an_empty_enumeration() {
        // The bug: a test that enumerates nothing passes, and reads as coverage.
        assert!(assert_all_mutations_rejected(&[]).is_err());
    }
}
