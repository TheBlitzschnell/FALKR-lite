//! Executes the operation kinds that exist today, and defers the rest.
//!
//! ## The deferral rule
//!
//! A scenario whose operations are all delivered by
//! [`Phase::DELIVERED_THROUGH`] is **executed** and its expected trial balance
//! is asserted to the cent. Every other scenario is **deferred**, reported with
//! the phase that will unblock it, and still fully structurally validated.
//!
//! Deferred is not "skipped". A deferred scenario has had its accounts
//! resolved, its arithmetic checked, its prose derivation required and its
//! declared phase verified against its operations. What it has not had is its
//! numbers compared against a feature that does not exist. the corpus discipline: *do not
//! implement production features to make a scenario pass* — and equally, do not
//! delete a scenario because it does not.
//!
//! ## Deterministic account identifiers
//!
//! [`AccountId`] is a UUID, and the ledger stores balances against it, but the
//! corpus talks in account codes. The mapping is a SHA-256 of the code truncated
//! to sixteen bytes, so `"1000"` is the same `AccountId` in every run, in every
//! process, forever. `AccountId::new()` would be a UUIDv7 — different on every
//! run, which would make a failure message unreadable and a replay comparison
//! meaningless.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use esrs::Aggregate as _;
use falkr_core::{
    AccountId, CapitalizationStatus, Currency, Dimensions, EntryId, FunctionCode, Money, PolicyId,
    ProjectId, ProviderId, RunId, TeamId, TenantId, UsageEventId,
};
use falkr_events::{
    JournalLine, LedgerAggregate, LedgerCommand, LedgerError, LedgerEvent, LedgerState,
};
use rust_decimal::Decimal;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::catalog::{self, Phase};
use crate::chart::ChartOfAccounts;
use crate::scenario::{Operation, ParamError, Scenario, Status};

/// A deterministic identifier derived from a stable string.
///
/// SHA-256 truncated to 16 bytes. Not a UUID version anyone should store in
/// production — it has no version nibble and no time ordering — which is why it
/// lives in a dev crate and nowhere else.
fn stable_uuid(namespace: &str, name: &str) -> Uuid {
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

/// The `AccountId` the corpus uses for an account code.
#[must_use]
pub fn account_id(code: &str) -> AccountId {
    AccountId::from_uuid(stable_uuid("falkr.corpus.account", code))
}

/// One posted entry, with the header attributes the ledger does not carry yet.
///
/// `date`, `description` and `source` come from the scenario file rather than
/// from the event, because today's `LedgerEvent::JournalEntryPosted` carries
/// only `posted_at` and the lines. the AS 2401 entry-attribute rule requires the full PCAOB
/// AS 2401 attribute set on the entry from creation, and binds P03 to deliver
/// it. Until then this struct is where those attributes live, and the AICPA
/// export reads them from here — which makes the gap visible in exactly one
/// place instead of invisible everywhere.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryRecord {
    pub entry_id: EntryId,
    /// The scenario-local name an operation gave this entry, used by
    /// `reverse_entry` to point at it.
    pub entry_ref: Option<String>,
    pub journal_id: String,
    pub effective_date: NaiveDate,
    pub description: String,
    pub source: String,
    pub entity: String,
    pub lines: Vec<JournalLine>,
    /// Set on a reversing entry, naming the entry it reverses.
    pub reversal_of: Option<EntryId>,
}

/// The result of executing a scenario.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerRun {
    pub scenario_id: String,
    pub entity: String,
    pub book: String,
    pub period: String,
    pub currency: Currency,
    pub events: Vec<LedgerEvent>,
    pub state: LedgerState,
    pub entries: Vec<EntryRecord>,
    /// Account identifier back to the `"1000-Cash"` label, for readable
    /// failures and for the export.
    pub labels: BTreeMap<AccountId, String>,
    /// Commands the scenario expected to be rejected, and the error each
    /// produced. Adversarial scenarios assert on these.
    pub rejections: Vec<(String, LedgerError)>,
}

impl LedgerRun {
    /// The final balances, keyed by `"1000-Cash"` label.
    #[must_use]
    pub fn trial_balance(&self) -> BTreeMap<String, Decimal> {
        self.state
            .accounts()
            .map(|(id, money)| {
                let label = self
                    .labels
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| id.to_string());
                (label, money.amount())
            })
            .collect()
    }
}

/// What happened when the runner was pointed at a scenario.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    Executed(Box<LedgerRun>),
    /// Every operation is catalogued, but at least one belongs to a phase that
    /// has not landed.
    Deferred {
        phase: Phase,
        kinds: Vec<String>,
    },
    /// The scenario's own expectations are not derived yet.
    Blocked {
        reason: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("{scenario}: {source}")]
    Param {
        scenario: String,
        #[source]
        source: ParamError,
    },
    #[error("{scenario}: account {reference:?} is not in the corpus chart")]
    UnknownAccount { scenario: String, reference: String },
    #[error("{scenario}: {detail}")]
    BadOperation { scenario: String, detail: String },
    #[error("{scenario}: the ledger rejected an operation that was expected to succeed: {source}")]
    Rejected {
        scenario: String,
        #[source]
        source: LedgerError,
    },
    #[error("{scenario}: operation {index} declared expect_rejection but the ledger accepted it")]
    ExpectedRejection { scenario: String, index: usize },
}

/// Executes corpus scenarios against the ledger aggregate.
#[derive(Debug, Clone)]
pub struct Runner {
    chart: ChartOfAccounts,
}

impl Runner {
    #[must_use]
    pub const fn new(chart: ChartOfAccounts) -> Self {
        Self { chart }
    }

    #[must_use]
    pub const fn chart(&self) -> &ChartOfAccounts {
        &self.chart
    }

    /// Runs a scenario, or explains why it cannot be run yet.
    ///
    /// # Errors
    ///
    /// If an operation is malformed, references an unknown account, or is
    /// rejected by the ledger when the scenario did not say it would be.
    pub fn run(&self, scenario: &Scenario) -> Result<RunOutcome, RunError> {
        if scenario.meta.status == Status::Blocked {
            return Ok(RunOutcome::Blocked {
                reason: scenario
                    .meta
                    .blocked_reason
                    .clone()
                    .unwrap_or_else(|| "no reason given".to_owned()),
            });
        }

        let undeliverable: Vec<String> = scenario
            .operations
            .iter()
            .filter(|op| catalog::spec(&op.kind).is_none_or(|spec| !spec.phase.is_delivered()))
            .map(|op| op.kind.clone())
            .collect();
        if !undeliverable.is_empty() {
            return Ok(RunOutcome::Deferred {
                phase: scenario.required_phase(),
                kinds: undeliverable,
            });
        }

        self.execute(scenario)
            .map(|run| RunOutcome::Executed(Box::new(run)))
    }

    fn execute(&self, scenario: &Scenario) -> Result<LedgerRun, RunError> {
        let currency = Currency::from_code(&scenario.setup.functional_ccy).ok_or_else(|| {
            RunError::BadOperation {
                scenario: scenario.meta.id.clone(),
                detail: format!("unknown currency {}", scenario.setup.functional_ccy),
            }
        })?;
        let dims = self.dimensions(scenario);

        let mut state = LedgerState::default();
        let mut events = Vec::new();
        let mut entries: Vec<EntryRecord> = Vec::new();
        let mut labels = BTreeMap::new();
        let mut rejections = Vec::new();

        for (index, op) in scenario.operations.iter().enumerate() {
            let expect_rejection = op
                .params
                .get("expect_rejection")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);

            let command = match op.kind.as_str() {
                "post_entry" => {
                    let lines = self.journal_lines(scenario, op, currency, &mut labels)?;
                    LedgerCommand::PostJournalEntry { lines, dims }
                }
                "reverse_entry" => {
                    let target = op.str_param("ref").map_err(|source| RunError::Param {
                        scenario: scenario.meta.id.clone(),
                        source,
                    })?;
                    let original = entries
                        .iter()
                        .find(|e| e.entry_ref.as_deref() == Some(target))
                        .ok_or_else(|| RunError::BadOperation {
                            scenario: scenario.meta.id.clone(),
                            detail: format!("reverse_entry points at unknown ref {target:?}"),
                        })?;
                    LedgerCommand::ReverseEntry {
                        original_id: original.entry_id,
                        reason: op.opt_str_param("reason").unwrap_or("corpus").to_owned(),
                    }
                }
                "set_capitalization" => {
                    let run = op.str_param("run").map_err(|source| RunError::Param {
                        scenario: scenario.meta.id.clone(),
                        source,
                    })?;
                    let to = op.str_param("to").map_err(|source| RunError::Param {
                        scenario: scenario.meta.id.clone(),
                        source,
                    })?;
                    let to = CapitalizationStatus::from_stored(to).ok_or_else(|| {
                        RunError::BadOperation {
                            scenario: scenario.meta.id.clone(),
                            detail: format!("unknown capitalization status {to:?}"),
                        }
                    })?;
                    LedgerCommand::ChangeCapitalizationStatus {
                        run_id: RunId::from_uuid(stable_uuid("falkr.corpus.run", run)),
                        to,
                        policy_ref: PolicyId::from_uuid(stable_uuid(
                            "falkr.corpus.policy",
                            op.opt_str_param("policy").unwrap_or("default"),
                        )),
                    }
                }
                other => {
                    return Err(RunError::BadOperation {
                        scenario: scenario.meta.id.clone(),
                        detail: format!(
                            "{other:?} is catalogued as delivered but the runner has no \
                             handler for it; catalog::OPERATIONS and Runner::execute \
                             have drifted apart"
                        ),
                    });
                }
            };

            match LedgerAggregate::handle_command(&state, command) {
                Ok(produced) => {
                    if expect_rejection {
                        return Err(RunError::ExpectedRejection {
                            scenario: scenario.meta.id.clone(),
                            index,
                        });
                    }
                    for event in produced {
                        record_entry(&mut entries, &event, op, scenario, index);
                        state = LedgerAggregate::apply_event(state, event.clone());
                        events.push(event);
                    }
                }
                Err(error) => {
                    if !expect_rejection {
                        return Err(RunError::Rejected {
                            scenario: scenario.meta.id.clone(),
                            source: error,
                        });
                    }
                    rejections.push((op.kind.clone(), error));
                }
            }
        }

        Ok(LedgerRun {
            scenario_id: scenario.meta.id.clone(),
            entity: scenario.setup.entity.clone(),
            book: scenario.setup.book.clone(),
            period: scenario.setup.period.clone(),
            currency,
            events,
            state,
            entries,
            labels,
            rejections,
        })
    }

    /// The dimensional spine for a scenario.
    ///
    /// Derived from the scenario id so two scenarios never share a tenant, and
    /// so a given scenario's dimensions are the same on every run. The spine is
    /// mandatory at write time (the dimensional-spine rule) — there is no path through
    /// this runner that posts without one.
    fn dimensions(&self, scenario: &Scenario) -> Dimensions {
        let id = &scenario.meta.id;
        Dimensions::new(
            TenantId::from_uuid(stable_uuid("falkr.corpus.tenant", &scenario.setup.entity)),
            ProjectId::from_uuid(stable_uuid("falkr.corpus.project", id)),
            TeamId::from_uuid(stable_uuid("falkr.corpus.team", id)),
            ProviderId::from_uuid(stable_uuid("falkr.corpus.provider", id)),
            FunctionCode::OpEx,
        )
    }

    fn journal_lines(
        &self,
        scenario: &Scenario,
        op: &Operation,
        default_currency: Currency,
        labels: &mut BTreeMap<AccountId, String>,
    ) -> Result<Vec<JournalLine>, RunError> {
        let raw = op.lines().map_err(|source| RunError::Param {
            scenario: scenario.meta.id.clone(),
            source,
        })?;
        let mut lines = Vec::with_capacity(raw.len());
        for line in raw {
            let account =
                self.chart
                    .resolve(&line.account)
                    .ok_or_else(|| RunError::UnknownAccount {
                        scenario: scenario.meta.id.clone(),
                        reference: line.account.clone(),
                    })?;
            let id = account_id(&account.code);
            labels.insert(id, account.label());

            let currency = match &line.currency {
                Some(code) => Currency::from_code(code).ok_or_else(|| RunError::BadOperation {
                    scenario: scenario.meta.id.clone(),
                    detail: format!("line names unknown currency {code:?}"),
                })?,
                None => default_currency,
            };

            let amount = |raw: &str| -> Result<Money, RunError> {
                let value = Decimal::from_str_exact(raw).map_err(|e| RunError::BadOperation {
                    scenario: scenario.meta.id.clone(),
                    detail: format!("{raw:?} is not an exact decimal: {e}"),
                })?;
                Ok(Money::new(value, currency))
            };

            // A line carrying both sides or neither is built as written rather
            // than refused here. The scenarios that contain one are asserting
            // that the *aggregate* refuses it, and pre-validating in the runner
            // would test the runner instead. `Scenario::validate` already
            // rejects a malformed line in any operation that has not declared
            // `expect_rejection`, so this cannot mask a typo.
            let mut journal_line = match (&line.debit, &line.credit) {
                (Some(d), None) => JournalLine::debit(id, amount(d)?),
                (None, Some(c)) => JournalLine::credit(id, amount(c)?),
                (Some(d), Some(c)) => JournalLine {
                    account: id,
                    debit: Some(amount(d)?),
                    credit: Some(amount(c)?),
                    source_usage_event_ids: Vec::new(),
                },
                (None, None) => JournalLine {
                    account: id,
                    debit: None,
                    credit: None,
                    source_usage_event_ids: Vec::new(),
                },
            };
            if !line.sources.is_empty() {
                journal_line = journal_line.with_sources(
                    line.sources
                        .iter()
                        .map(|s| UsageEventId::from_uuid(stable_uuid("falkr.corpus.usage", s)))
                        .collect(),
                );
            }
            lines.push(journal_line);
        }
        Ok(lines)
    }
}

fn record_entry(
    entries: &mut Vec<EntryRecord>,
    event: &LedgerEvent,
    op: &Operation,
    scenario: &Scenario,
    index: usize,
) {
    let default_date = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap_or_default();
    match event {
        LedgerEvent::JournalEntryPosted { id, lines, .. } => {
            entries.push(EntryRecord {
                entry_id: *id,
                entry_ref: op.opt_str_param("ref").map(str::to_owned),
                journal_id: format!("{}-{index:03}", scenario.meta.id),
                effective_date: op
                    .opt_str_param("date")
                    .and_then(|d| d.parse::<NaiveDate>().ok())
                    .unwrap_or(default_date),
                description: op.opt_str_param("description").unwrap_or("").to_owned(),
                source: op.opt_str_param("source").unwrap_or("MANUAL").to_owned(),
                entity: op
                    .opt_str_param("entity")
                    .unwrap_or(&scenario.setup.entity)
                    .to_owned(),
                lines: lines.clone(),
                reversal_of: None,
            });
        }
        LedgerEvent::EntryReversed {
            original_id,
            reversal_id,
            reason,
        } => {
            let mirrored = entries
                .iter()
                .find(|e| e.entry_id == *original_id)
                .map(|e| mirror(&e.lines))
                .unwrap_or_default();
            entries.push(EntryRecord {
                entry_id: *reversal_id,
                entry_ref: op.opt_str_param("ref").map(str::to_owned),
                journal_id: format!("{}-{index:03}", scenario.meta.id),
                effective_date: op
                    .opt_str_param("date")
                    .and_then(|d| d.parse::<NaiveDate>().ok())
                    .unwrap_or(default_date),
                description: format!("Reversal: {reason}"),
                source: op.opt_str_param("source").unwrap_or("MANUAL").to_owned(),
                entity: scenario.setup.entity.clone(),
                lines: mirrored,
                reversal_of: Some(*original_id),
            });
        }
        LedgerEvent::CapitalizationStatusChanged { .. } => {}
    }
}

/// Mirrors lines for the export's view of a reversal.
///
/// Duplicated from the aggregate's private `mirror` deliberately: this is the
/// corpus's *independent* statement of what a reversal looks like. If the
/// aggregate's mirroring changes, the export stops reconciling and a test fails,
/// which is the intended alarm. Calling the aggregate's version would make the
/// check tautological.
fn mirror(lines: &[JournalLine]) -> Vec<JournalLine> {
    lines
        .iter()
        .map(|l| JournalLine {
            account: l.account,
            debit: l.credit,
            credit: l.debit,
            source_usage_event_ids: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_ids_are_stable_across_calls() {
        assert_eq!(account_id("1000"), account_id("1000"));
        assert_ne!(account_id("1000"), account_id("1001"));
    }

    #[test]
    fn namespaces_do_not_collide() {
        assert_ne!(
            stable_uuid("falkr.corpus.account", "1000"),
            stable_uuid("falkr.corpus.run", "1000")
        );
    }
}
