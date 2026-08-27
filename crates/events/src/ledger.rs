//! The ledger aggregate: command → event → apply.
//!
//! Three rules hold here and are enforced by the type system where possible:
//!
//! 1. **Commands may be rejected; events may not.** All validation lives in
//!    [`LedgerAggregate::handle_command`]. [`LedgerAggregate::apply_event`]
//!    returns `State`, not `Result<State, _>` — it *cannot* reject, so a
//!    validation rule that leaked into replay would not compile.
//! 2. **Events are the only way a balance changes**. The
//!    balance map in [`LedgerState`] is a projection rebuilt from the log;
//!    nothing else writes to it.
//! 3. **Entries balance.** A journal entry whose debits and credits differ is
//!    rejected at the command boundary and therefore can never enter the log.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use falkr_core::{
    AccountId, CapitalizationStatus, Currency, CurrencyMismatch, Dimensions, EntryId, Money,
    PolicyId, RunId, UsageEventId,
};
use rust_decimal::Decimal;

/// One side of a journal entry.
///
/// Exactly one of `debit`/`credit` is populated. Modelling it as two `Option`s
/// rather than a signed amount matches how an accountant reads an entry and how
/// an accountant reads it; [`JournalLine::validate`] enforces the
/// "exactly one" part that the types alone cannot.
///
/// Not `Copy`, because of `source_usage_event_ids` — see its documentation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JournalLine {
    pub account: AccountId,
    pub debit: Option<Money>,
    pub credit: Option<Money>,
    /// The usage events that produced this line, for revenue lines.
    ///
    /// The bar this exists to clear: "can you trace this quarter's
    /// recognized revenue to the underlying token events in one query". That is
    /// the difference between an audit that takes an afternoon and one that
    /// takes a month, and it only works if the back-reference is written at
    /// posting time — reconstructing it afterwards from timestamps and amounts
    /// is guesswork.
    ///
    /// Empty for every line that is not recognized revenue.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_usage_event_ids: Vec<UsageEventId>,
}

impl JournalLine {
    #[must_use]
    pub const fn debit(account: AccountId, amount: Money) -> Self {
        Self {
            account,
            debit: Some(amount),
            credit: None,
            source_usage_event_ids: Vec::new(),
        }
    }

    #[must_use]
    pub const fn credit(account: AccountId, amount: Money) -> Self {
        Self {
            account,
            debit: None,
            credit: Some(amount),
            source_usage_event_ids: Vec::new(),
        }
    }

    /// Attaches the usage events this line was recognized from.
    #[must_use]
    pub fn with_sources(mut self, sources: Vec<UsageEventId>) -> Self {
        self.source_usage_event_ids = sources;
        self
    }

    /// The line's signed contribution under the debit-positive convention.
    ///
    /// A trial balance is the sum of these across every account; it is zero for
    /// a well-formed ledger, which is the invariant
    /// [`LedgerState::trial_balance`] checks.
    #[must_use]
    pub fn signed_amount(&self) -> Option<Money> {
        match (self.debit, self.credit) {
            (Some(d), None) => Some(d),
            (None, Some(c)) => Some(c.negate()),
            _ => None,
        }
    }

    fn validate(&self) -> Result<Money, LedgerError> {
        match (self.debit, self.credit) {
            (Some(d), None) => Ok(d),
            (None, Some(c)) => Ok(c),
            (Some(_), Some(_)) => Err(LedgerError::LineHasBothSides {
                account: self.account,
            }),
            (None, None) => Err(LedgerError::LineHasNoAmount {
                account: self.account,
            }),
        }
    }
}

/// Past-tense facts. Immutable once written, forever.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LedgerEvent {
    JournalEntryPosted {
        id: EntryId,
        lines: Vec<JournalLine>,
        dims: Dimensions,
        posted_at: DateTime<Utc>,
    },
    EntryReversed {
        original_id: EntryId,
        reversal_id: EntryId,
        reason: String,
    },
    CapitalizationStatusChanged {
        run_id: RunId,
        from: CapitalizationStatus,
        to: CapitalizationStatus,
        policy_ref: PolicyId,
    },
}

/// Intent. May be rejected.
#[derive(Debug, Clone, PartialEq)]
pub enum LedgerCommand {
    PostJournalEntry {
        lines: Vec<JournalLine>,
        dims: Dimensions,
    },
    ReverseEntry {
        original_id: EntryId,
        reason: String,
    },
    ChangeCapitalizationStatus {
        run_id: RunId,
        to: CapitalizationStatus,
        policy_ref: PolicyId,
    },
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LedgerError {
    #[error("journal entry does not balance: {debits} debits vs {credits} credits")]
    Unbalanced { debits: Decimal, credits: Decimal },
    #[error("journal entry has no lines")]
    EmptyEntry,
    #[error("line on account {account} carries both a debit and a credit")]
    LineHasBothSides { account: AccountId },
    #[error("line on account {account} carries neither a debit nor a credit")]
    LineHasNoAmount { account: AccountId },
    #[error("entry {0} has already been reversed")]
    AlreadyReversed(EntryId),
    #[error("entry {0} is not in this ledger")]
    UnknownEntry(EntryId),
    #[error("run {run_id} is already {status:?}")]
    CapitalizationUnchanged {
        run_id: RunId,
        status: CapitalizationStatus,
    },
    #[error(transparent)]
    Currency(#[from] CurrencyMismatch),
}

/// The projected state of the ledger, rebuilt by replaying the event log.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LedgerState {
    /// Account balances under the debit-positive convention.
    balances: BTreeMap<AccountId, Money>,
    /// Entries posted, by id, retained so a reversal can be validated and
    /// mirrored without re-reading the log.
    entries: BTreeMap<EntryId, Vec<JournalLine>>,
    reversed: BTreeSet<EntryId>,
    capitalization: BTreeMap<RunId, CapitalizationStatus>,
}

impl LedgerState {
    /// The balance of one account, or `None` if it has never been posted to.
    #[must_use]
    pub fn balance(&self, account: AccountId) -> Option<Money> {
        self.balances.get(&account).copied()
    }

    /// Every account with a non-empty history, in account order.
    pub fn accounts(&self) -> impl Iterator<Item = (&AccountId, &Money)> {
        self.balances.iter()
    }

    #[must_use]
    pub fn is_reversed(&self, entry: EntryId) -> bool {
        self.reversed.contains(&entry)
    }

    #[must_use]
    pub fn capitalization_status(&self, run: RunId) -> Option<CapitalizationStatus> {
        self.capitalization.get(&run).copied()
    }

    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// The trial balance: the sum of every account balance in `currency`.
    ///
    /// **This is always zero for a well-formed ledger.** Every posted entry
    /// contributes equal debits and credits, so they cancel across accounts. A
    /// non-zero result means an unbalanced entry reached the log, which the
    /// command handler is supposed to make impossible.
    #[must_use]
    pub fn trial_balance(&self, currency: Currency) -> Money {
        self.balances
            .values()
            .filter(|m| m.currency() == currency)
            .fold(Money::zero(currency), |acc, m| {
                // Infallible: the filter above guarantees a matching currency.
                acc.checked_add(m).unwrap_or(acc)
            })
    }

    fn post(&mut self, id: EntryId, lines: &[JournalLine]) {
        for line in lines {
            let Some(delta) = line.signed_amount() else {
                continue; // Unreachable for a validated entry.
            };
            let entry = self
                .balances
                .entry(line.account)
                .or_insert_with(|| Money::zero(delta.currency()));
            if let Ok(updated) = entry.checked_add(&delta) {
                *entry = updated;
            }
        }
        self.entries.insert(id, lines.to_vec());
    }
}

/// The event-sourced ledger.
#[derive(Debug, Clone, Copy, Default)]
pub struct LedgerAggregate;

impl LedgerAggregate {
    /// Mirrors an entry's lines, swapping debits and credits.
    ///
    /// A reversal is a new posting rather than a deletion or an edit — the
    /// original event stays in the log untouched, which is what makes the trail
    /// auditable rather than merely accurate.
    fn mirror(lines: &[JournalLine]) -> Vec<JournalLine> {
        lines
            .iter()
            .map(|l| JournalLine {
                account: l.account,
                debit: l.credit,
                credit: l.debit,
                // Deliberately not carried across. A reversal is its own fact,
                // and duplicating the back-references would make a traced
                // revenue query return each source event twice.
                source_usage_event_ids: Vec::new(),
            })
            .collect()
    }
}

impl esrs::Aggregate for LedgerAggregate {
    const NAME: &'static str = "ledger";

    type State = LedgerState;
    type Command = LedgerCommand;
    type Event = LedgerEvent;
    type Error = LedgerError;

    /// The only place business rules about "is this allowed" live.
    fn handle_command(
        state: &Self::State,
        command: Self::Command,
    ) -> Result<Vec<Self::Event>, Self::Error> {
        match command {
            LedgerCommand::PostJournalEntry { lines, dims } => {
                if lines.is_empty() {
                    return Err(LedgerError::EmptyEntry);
                }

                // Every line must carry exactly one side, and all lines must
                // share a currency — summing across currencies is meaningless,
                // so `Money` refuses it rather than producing a wrong total.
                let mut debits = Money::zero(lines[0].validate()?.currency());
                let mut credits = debits;
                for line in &lines {
                    let amount = line.validate()?;
                    if line.debit.is_some() {
                        debits = debits.checked_add(&amount)?;
                    } else {
                        credits = credits.checked_add(&amount)?;
                    }
                }

                if debits != credits {
                    return Err(LedgerError::Unbalanced {
                        debits: debits.amount(),
                        credits: credits.amount(),
                    });
                }

                Ok(vec![LedgerEvent::JournalEntryPosted {
                    id: EntryId::new(),
                    lines,
                    dims,
                    posted_at: Utc::now(),
                }])
            }

            LedgerCommand::ReverseEntry {
                original_id,
                reason,
            } => {
                if !state.entries.contains_key(&original_id) {
                    return Err(LedgerError::UnknownEntry(original_id));
                }
                if state.is_reversed(original_id) {
                    return Err(LedgerError::AlreadyReversed(original_id));
                }
                Ok(vec![LedgerEvent::EntryReversed {
                    original_id,
                    reversal_id: EntryId::new(),
                    reason,
                }])
            }

            LedgerCommand::ChangeCapitalizationStatus {
                run_id,
                to,
                policy_ref,
            } => {
                let from = state
                    .capitalization_status(run_id)
                    .unwrap_or(CapitalizationStatus::PendingReview);
                if from == to {
                    return Err(LedgerError::CapitalizationUnchanged { run_id, status: to });
                }
                Ok(vec![LedgerEvent::CapitalizationStatusChanged {
                    run_id,
                    from,
                    to,
                    policy_ref,
                }])
            }
        }
    }

    /// Infallible by construction — it can only replay history, never reject.
    fn apply_event(mut state: Self::State, payload: Self::Event) -> Self::State {
        match payload {
            LedgerEvent::JournalEntryPosted { id, lines, .. } => {
                state.post(id, &lines);
            }
            LedgerEvent::EntryReversed {
                original_id,
                reversal_id,
                ..
            } => {
                if let Some(original) = state.entries.get(&original_id).cloned() {
                    state.post(reversal_id, &LedgerAggregate::mirror(&original));
                }
                state.reversed.insert(original_id);
            }
            LedgerEvent::CapitalizationStatusChanged { run_id, to, .. } => {
                state.capitalization.insert(run_id, to);
            }
        }
        state
    }
}
