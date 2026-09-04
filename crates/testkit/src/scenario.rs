//! The scenario file format, and everything about a scenario that can be
//! checked without running it.
//!
//! A scenario is three things in one file: the **setup**, the **operations**,
//! and the **hand-computed expected result** with the derivation written out in
//! prose. The prose is not decoration —
//! *a scenario whose expected numbers cannot be explained in prose is a
//! scenario whose expected numbers are guesses* — and
//! [`Scenario::validate`] refuses a file without it.
//!
//! ## Sign convention
//!
//! `[expected.trial_balance]` is **debit-positive**, matching
//! [`falkr_events::JournalLine::signed_amount`]. Cash of 100,000 is
//! `"100000.00"`; revenue of 25,000 is `"-25000.00"`. One convention, stated
//! once, because a corpus that mixes conventions is a corpus that encodes sign
//! errors as facts.
//!
//! ## What validation catches before anything is executed
//!
//! Every check below runs against every scenario today, including the ones
//! whose features do not exist yet — which is most of the value of the corpus
//! in the near term:
//!
//! - the expected trial balance sums to exactly zero (a hand-arithmetic slip);
//! - every account reference resolves against the corpus chart, name included;
//! - every operation kind is in the catalog (a typo, not a missing feature);
//! - the declared phase equals the highest phase among the operations;
//! - a balance on the wrong side of an account is either impossible-by-policy or
//!   declared in `expected.unusual_balances`;
//! - an `unusual_balances` entry that is no longer unusual is stale, and fails.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use falkr_core::Currency;
use rust_decimal::Decimal;

use crate::catalog::{self, Phase};
use crate::chart::ChartOfAccounts;

#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("could not read {path}: {detail}")]
    Io { path: PathBuf, detail: String },
    #[error("{path}: {detail}")]
    Parse { path: PathBuf, detail: String },
}

/// Whether a scenario's expected numbers have been derived yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Expected numbers derived from the standard and written down.
    #[default]
    Ready,
    /// The derivation is not possible yet and `blocked_reason` says what is
    /// needed. the corpus discipline: mark it blocked rather than filling it in from a test
    /// run.
    Blocked,
    /// The expected numbers are derived and correct, **and this codebase
    /// currently produces something else**.
    ///
    /// This state is the reason a specification corpus is worth more than a
    /// regression snapshot. A snapshot has nowhere to record "the standard says
    /// X and we do Y" — the only options are to delete the scenario or to
    /// change the expectation to match the code, and both destroy the evidence.
    /// A divergent scenario is asserted to *fail*, so it stays visible, stays
    /// green in CI, and turns into a test failure the moment somebody fixes the
    /// underlying defect and forgets to promote it to `ready`.
    Divergent,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub id: String,
    pub title: String,
    /// The authoritative paragraphs this scenario's expected numbers come from.
    pub standards: Vec<String>,
    /// The phase that makes this scenario executable. Checked against the
    /// operations, never trusted on its own.
    pub phase: Phase,
    /// 1 (a two-line entry) to 5 (multi-entity, multi-currency, multi-period).
    pub difficulty: u8,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub blocked_reason: Option<String>,
    /// Required when `status = "divergent"`: what the standard requires, what
    /// this codebase does instead, and which phase is expected to close the gap.
    #[serde(default)]
    pub divergence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    pub entity: String,
    /// `US_GAAP`, `IFRS`, `TAX`, `MGMT`. Books are separate ledgers over the
    /// same events; the trial balance is zero in each of them independently.
    pub book: String,
    pub functional_ccy: String,
    pub period: String,
    /// `gregorian` (default), `4-4-5`, or `53-week`. Only affects which dates
    /// belong to which period, which is why it is setup rather than metadata.
    #[serde(default = "default_calendar")]
    pub calendar: String,
}

fn default_calendar() -> String {
    "gregorian".to_owned()
}

/// One step in a scenario.
///
/// Deliberately half-typed: `kind` is checked against the catalog, and the rest
/// is left as a TOML table that the operation's own handler interprets. The
/// alternative — one enum variant per operation — would mean a compile error in
/// this crate every time a future phase adds an operation kind, for no
/// correctness benefit, since the parameters of an unimplemented operation are
/// not interpreted by anything yet.
#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    pub kind: String,
    pub params: toml::Table,
}

impl<'de> serde::Deserialize<'de> for Operation {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let mut params = toml::Table::deserialize(d)?;
        let kind = params
            .remove("kind")
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(|| D::Error::missing_field("kind"))?;
        Ok(Self { kind, params })
    }
}

impl Operation {
    /// A required string parameter.
    ///
    /// # Errors
    ///
    /// If the parameter is absent or is not a string.
    pub fn str_param(&self, name: &str) -> Result<&str, ParamError> {
        self.params
            .get(name)
            .and_then(toml::Value::as_str)
            .ok_or_else(|| ParamError::Missing {
                kind: self.kind.clone(),
                param: name.to_owned(),
                wanted: "string",
            })
    }

    #[must_use]
    pub fn opt_str_param(&self, name: &str) -> Option<&str> {
        self.params.get(name).and_then(toml::Value::as_str)
    }

    /// A required exact decimal parameter, written as a **string** in TOML.
    ///
    /// Amounts are strings, never TOML floats. TOML has no decimal type, so a
    /// bare `1000.10` is an IEEE-754 double before it ever reaches this crate —
    /// which is the no-floats-for-money rule being broken in the data file rather than in
    /// the code. `from_str_exact` additionally rejects a literal that would lose
    /// precision, so a 30-digit amount fails rather than silently truncating.
    ///
    /// # Errors
    ///
    /// If the parameter is absent, is not a string, or is not an exact decimal.
    pub fn decimal_param(&self, name: &str) -> Result<Decimal, ParamError> {
        let raw = self.str_param(name)?;
        Decimal::from_str_exact(raw).map_err(|e| ParamError::NotDecimal {
            kind: self.kind.clone(),
            param: name.to_owned(),
            raw: raw.to_owned(),
            detail: e.to_string(),
        })
    }

    /// The `lines = [...]` array of a `post_entry`.
    ///
    /// # Errors
    ///
    /// If `lines` is absent, is not an array, or contains a malformed line.
    pub fn lines(&self) -> Result<Vec<ScenarioLine>, ParamError> {
        let array = self
            .params
            .get("lines")
            .and_then(toml::Value::as_array)
            .ok_or_else(|| ParamError::Missing {
                kind: self.kind.clone(),
                param: "lines".to_owned(),
                wanted: "array of tables",
            })?;
        array
            .iter()
            .map(|v| {
                v.clone()
                    .try_into::<ScenarioLine>()
                    .map_err(|e| ParamError::BadLine {
                        kind: self.kind.clone(),
                        detail: e.to_string(),
                    })
            })
            .collect()
    }
}

/// One line of a hand-written journal entry in a scenario file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioLine {
    pub account: String,
    /// Exactly one of `debit`/`credit`, as an exact decimal string.
    #[serde(default)]
    pub debit: Option<String>,
    #[serde(default)]
    pub credit: Option<String>,
    /// Overrides the scenario's functional currency for this line. Present only
    /// in scenarios that are *testing* the currency-isolation invariant, since a
    /// well-formed entry is single-currency by rule.
    #[serde(default)]
    pub currency: Option<String>,
    /// Usage-event identifiers this revenue line was recognized from, carried
    /// through to `JournalLine::source_usage_event_ids`.
    #[serde(default)]
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParamError {
    #[error("operation `{kind}` is missing required {wanted} parameter `{param}`")]
    Missing {
        kind: String,
        param: String,
        wanted: &'static str,
    },
    #[error("operation `{kind}` parameter `{param}` = {raw:?} is not an exact decimal: {detail}")]
    NotDecimal {
        kind: String,
        param: String,
        raw: String,
        detail: String,
    },
    #[error("operation `{kind}` has a malformed line: {detail}")]
    BadLine { kind: String, detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedInvariants {
    #[serde(default)]
    pub balanced: bool,
    #[serde(default)]
    pub replay_stable: bool,
    #[serde(default)]
    pub subledger_ties: bool,
    #[serde(default)]
    pub roll_forward: bool,
    #[serde(default)]
    pub intercompany_eliminated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notes {
    /// The derivation, in prose, by a human. Required.
    pub reasoning: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    /// Account reference -> signed balance, debit-positive, as an exact decimal
    /// string.
    #[serde(default)]
    pub trial_balance: BTreeMap<String, String>,
    /// Per-entity trial balances, for consolidation scenarios. When present,
    /// `trial_balance` is the consolidated view.
    #[serde(default)]
    pub by_entity: BTreeMap<String, BTreeMap<String, String>>,
    /// Subledger name -> expected subledger total, tied to its control account
    /// by invariant 7.
    #[serde(default)]
    pub subledgers: BTreeMap<String, String>,
    /// Accounts whose expected balance sits on the side opposite to their
    /// normal balance, acknowledged deliberately.
    #[serde(default)]
    pub unusual_balances: Vec<String>,
    #[serde(default)]
    pub invariants: ExpectedInvariants,
    pub notes: Notes,
}

/// A parsed corpus scenario.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub path: PathBuf,
    pub meta: Meta,
    pub setup: Setup,
    pub operations: Vec<Operation>,
    pub expected: Expected,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioFile {
    meta: Meta,
    setup: Setup,
    #[serde(default)]
    operations: Vec<Operation>,
    expected: Expected,
}

/// One thing wrong with a scenario file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defect {
    pub scenario: String,
    pub detail: String,
}

impl core::fmt::Display for Defect {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.scenario, self.detail)
    }
}

/// The shortest prose derivation that could plausibly explain a number.
///
/// Chosen to reject "see above" and "obvious" without demanding an essay for a
/// two-line entry. It is a floor on effort, not a quality bar — the quality bar
/// is a human reading it.
const MIN_REASONING_CHARS: usize = 120;

/// Books the corpus recognizes.
const BOOKS: &[&str] = &["US_GAAP", "IFRS", "TAX", "MGMT"];

/// Fiscal calendars the corpus recognizes.
const CALENDARS: &[&str] = &["gregorian", "4-4-5", "53-week"];

impl Scenario {
    /// Parses one scenario file.
    ///
    /// # Errors
    ///
    /// If the file cannot be read or is not a well-formed scenario. Structural
    /// problems that need the chart of accounts are reported by
    /// [`Scenario::validate`] instead, so that one bad file does not stop the
    /// rest of the corpus from being checked.
    pub fn load(path: &Path) -> Result<Self, ScenarioError> {
        let raw = std::fs::read_to_string(path).map_err(|e| ScenarioError::Io {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
        let file: ScenarioFile = toml::from_str(&raw).map_err(|e| ScenarioError::Parse {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            meta: file.meta,
            setup: file.setup,
            operations: file.operations,
            expected: file.expected,
        })
    }

    /// The corpus category, taken from the containing directory.
    #[must_use]
    pub fn category(&self) -> String {
        self.path
            .parent()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The highest phase among this scenario's operations — the phase that
    /// actually unblocks it.
    #[must_use]
    pub fn required_phase(&self) -> Phase {
        self.operations
            .iter()
            .filter_map(|o| catalog::spec(&o.kind))
            .map(|s| s.phase)
            .max()
            .unwrap_or(Phase::P02)
    }

    /// Whether every operation this scenario uses exists today.
    #[must_use]
    pub fn is_executable(&self) -> bool {
        self.meta.status != Status::Blocked && self.required_phase().is_delivered()
    }

    /// The expected trial balance as exact decimals.
    ///
    /// # Errors
    ///
    /// If any amount is not an exact decimal literal.
    pub fn expected_trial_balance(&self) -> Result<BTreeMap<String, Decimal>, Defect> {
        parse_balances(&self.meta.id, &self.expected.trial_balance)
    }

    /// Everything wrong with this scenario, as a list rather than a first
    /// error: a corpus file with three problems should report three, so one
    /// pass over the file fixes it.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "a flat list of independent checks reads better than the same \
                  checks scattered across ten single-use helpers"
    )]
    pub fn validate(&self, chart: &ChartOfAccounts) -> Vec<Defect> {
        let mut defects = Vec::new();
        let mut fail = |detail: String| {
            defects.push(Defect {
                scenario: self.meta.id.clone(),
                detail,
            });
        };

        // --- identity -----------------------------------------------------
        let stem = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if stem != self.meta.id {
            fail(format!(
                "meta.id is {:?} but the file is named {stem:?}; they must match \
                 so a failure message names a file you can open",
                self.meta.id
            ));
        }
        let category = self.category();
        if !self.meta.id.starts_with(&format!("{category}-")) {
            fail(format!(
                "meta.id {:?} does not start with its category {category:?}",
                self.meta.id
            ));
        }
        if !(1..=5).contains(&self.meta.difficulty) {
            fail(format!(
                "difficulty {} is outside 1..=5",
                self.meta.difficulty
            ));
        }

        // --- documentation ------------------------------------------------
        let reasoning = self.expected.notes.reasoning.trim();
        if reasoning.len() < MIN_REASONING_CHARS {
            fail(format!(
                "expected.notes.reasoning is {} characters; at least \
                 {MIN_REASONING_CHARS} are required. Expected numbers that cannot \
                 be explained in prose are guesses",
                reasoning.len()
            ));
        }
        if self.meta.standards.is_empty() {
            fail(
                "meta.standards is empty; name the paragraph the expected numbers \
                 come from, even if it is the definition of double entry"
                    .to_owned(),
            );
        }

        // --- blocked scenarios --------------------------------------------
        match self.meta.status {
            Status::Blocked => {
                if self
                    .meta
                    .blocked_reason
                    .as_deref()
                    .is_none_or(|r| r.trim().is_empty())
                {
                    fail("status = \"blocked\" requires blocked_reason".to_owned());
                }
                if !self.expected.trial_balance.is_empty() {
                    fail(
                        "a blocked scenario must not carry an expected trial balance; \
                         the corpus discipline forbids filling one in from a test run"
                            .to_owned(),
                    );
                }
            }
            Status::Ready | Status::Divergent => {
                if self.meta.blocked_reason.is_some() {
                    fail(format!(
                        "blocked_reason is set but status is {:?}",
                        self.meta.status
                    ));
                }
                if self.expected.trial_balance.is_empty() {
                    fail("this scenario needs an expected trial balance".to_owned());
                }
            }
        }
        match (self.meta.status, self.meta.divergence.as_deref()) {
            (Status::Divergent, None | Some("")) => fail(
                "status = \"divergent\" requires `divergence`, saying what the standard \
                 requires, what this codebase does instead, and which phase closes the gap"
                    .to_owned(),
            ),
            (Status::Ready | Status::Blocked, Some(_)) => fail(format!(
                "`divergence` is set but status is {:?}",
                self.meta.status
            )),
            _ => {}
        }

        // --- setup --------------------------------------------------------
        if chart.entity(&self.setup.entity).is_none() {
            fail(format!(
                "setup.entity {:?} is not in corpus/_reference/entities.toml",
                self.setup.entity
            ));
        }
        if !BOOKS.contains(&self.setup.book.as_str()) {
            fail(format!(
                "setup.book {:?} is not one of {BOOKS:?}",
                self.setup.book
            ));
        }
        if Currency::from_code(&self.setup.functional_ccy).is_none() {
            fail(format!(
                "setup.functional_ccy {:?} is not a currency falkr_core knows",
                self.setup.functional_ccy
            ));
        }
        if !CALENDARS.contains(&self.setup.calendar.as_str()) {
            fail(format!(
                "setup.calendar {:?} is not one of {CALENDARS:?}",
                self.setup.calendar
            ));
        }
        if let Err(detail) = validate_period(&self.setup.period) {
            fail(detail);
        }

        // --- operations ---------------------------------------------------
        if self.operations.is_empty() && self.meta.status != Status::Blocked {
            fail("a ready scenario needs at least one operation".to_owned());
        }
        for op in &self.operations {
            if catalog::spec(&op.kind).is_none() {
                fail(format!(
                    "unknown operation kind {:?}; add it to catalog::OPERATIONS or \
                     fix the typo",
                    op.kind
                ));
            }
        }
        let required = self.required_phase();
        if self.meta.status != Status::Blocked && self.meta.phase != required {
            fail(format!(
                "meta.phase is {} but the operations require {required}",
                self.meta.phase
            ));
        }

        // --- accounts and amounts ------------------------------------------
        let mut referenced: BTreeSet<String> = BTreeSet::new();
        for op in &self.operations {
            if op.kind != "post_entry" {
                continue;
            }
            // An operation that declares `expect_rejection` is *supposed* to be
            // malformed — that is what it is testing. Validating its lines here
            // would make the adversarial category impossible to write.
            let deliberately_malformed = op
                .params
                .get("expect_rejection")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            match op.lines() {
                Ok(lines) => {
                    for line in lines {
                        referenced.insert(line.account.clone());
                        if deliberately_malformed {
                            continue;
                        }
                        match (&line.debit, &line.credit) {
                            (Some(_), Some(_)) => fail(format!(
                                "line on {} carries both a debit and a credit",
                                line.account
                            )),
                            (None, None) => fail(format!(
                                "line on {} carries neither a debit nor a credit",
                                line.account
                            )),
                            _ => {}
                        }
                    }
                }
                Err(e) => fail(e.to_string()),
            }
        }

        let balances = match self.expected_trial_balance() {
            Ok(b) => b,
            Err(defect) => {
                defects.push(defect);
                BTreeMap::new()
            }
        };
        for reference in referenced.iter().chain(balances.keys()) {
            if chart.resolve(reference).is_none() {
                defects.push(Defect {
                    scenario: self.meta.id.clone(),
                    detail: format!(
                        "account {reference:?} is not in the corpus chart (or its name \
                         does not match its code)"
                    ),
                });
            }
        }

        // --- the trial balance must actually balance ------------------------
        // The single highest-value check in this file. It catches an arithmetic
        // slip in a hand-computed expectation without running one line of
        // production code, which is exactly the failure mode of writing 130
        // scenarios by hand.
        let mut total = Decimal::ZERO;
        let mut overflowed = false;
        for amount in balances.values() {
            match total.checked_add(*amount) {
                Some(next) => total = next,
                None => overflowed = true,
            }
        }
        if overflowed {
            defects.push(Defect {
                scenario: self.meta.id.clone(),
                detail: "expected trial balance overflows Decimal while summing".to_owned(),
            });
        } else if !total.is_zero() && self.meta.status != Status::Blocked {
            defects.push(Defect {
                scenario: self.meta.id.clone(),
                detail: format!(
                    "expected trial balance does not sum to zero: {total} left over. \
                     Debits are positive and credits negative; check the derivation \
                     in expected.notes.reasoning against the numbers above it"
                ),
            });
        }

        // --- unusual balances must be declared, and declarations must be live
        let declared: BTreeSet<&str> = self
            .expected
            .unusual_balances
            .iter()
            .map(String::as_str)
            .collect();
        for (reference, amount) in &balances {
            let Some(account) = chart.resolve(reference) else {
                continue;
            };
            let on_normal_side = match account.normal_balance() {
                crate::chart::NormalBalance::Debit => !amount.is_sign_negative(),
                crate::chart::NormalBalance::Credit => {
                    amount.is_sign_negative() || amount.is_zero()
                }
            };
            let is_declared = declared.contains(reference.as_str());
            if !on_normal_side && !account.permits_opposite_sign() && !is_declared {
                defects.push(Defect {
                    scenario: self.meta.id.clone(),
                    detail: format!(
                        "{reference} is expected to end at {amount}, on the side \
                         opposite its normal balance ({}). If that is genuinely \
                         correct, list it in expected.unusual_balances and say why \
                         in the reasoning",
                        account.normal_balance().as_str()
                    ),
                });
            }
            if on_normal_side && is_declared {
                defects.push(Defect {
                    scenario: self.meta.id.clone(),
                    detail: format!(
                        "{reference} is listed in expected.unusual_balances but ends \
                         at {amount}, which is its normal side. Stale annotation"
                    ),
                });
            }
        }
        for reference in &declared {
            if !balances.contains_key(*reference) {
                defects.push(Defect {
                    scenario: self.meta.id.clone(),
                    detail: format!(
                        "expected.unusual_balances names {reference}, which is not in \
                         the expected trial balance"
                    ),
                });
            }
        }

        // --- subledger expectations must name a real subledger --------------
        for name in self.expected.subledgers.keys() {
            if chart.control_account(name).is_none() {
                defects.push(Defect {
                    scenario: self.meta.id.clone(),
                    detail: format!(
                        "expected.subledgers names {name:?}, which no account in the \
                         chart claims as its control account"
                    ),
                });
            }
        }

        // --- per-entity trial balances --------------------------------------
        for (entity, per_entity) in &self.expected.by_entity {
            if chart.entity(entity).is_none() {
                defects.push(Defect {
                    scenario: self.meta.id.clone(),
                    detail: format!("expected.by_entity names unknown entity {entity:?}"),
                });
            }
            match parse_balances(&self.meta.id, per_entity) {
                Ok(parsed) => {
                    let mut sum = Decimal::ZERO;
                    for amount in parsed.values() {
                        sum = sum.checked_add(*amount).unwrap_or(sum);
                    }
                    if !sum.is_zero() {
                        defects.push(Defect {
                            scenario: self.meta.id.clone(),
                            detail: format!(
                                "expected.by_entity.{entity} does not sum to zero: {sum}"
                            ),
                        });
                    }
                }
                Err(defect) => defects.push(defect),
            }
        }

        defects
    }
}

fn parse_balances(
    scenario: &str,
    raw: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, Decimal>, Defect> {
    let mut out = BTreeMap::new();
    for (account, amount) in raw {
        let parsed = Decimal::from_str_exact(amount).map_err(|e| Defect {
            scenario: scenario.to_owned(),
            detail: format!("{account} = {amount:?} is not an exact decimal: {e}"),
        })?;
        out.insert(account.clone(), parsed);
    }
    Ok(out)
}

/// `2026-01` (Gregorian month) or `2026-P01` (fiscal period under a 4-4-5 or
/// 53-week calendar).
fn validate_period(period: &str) -> Result<(), String> {
    let Some((year, rest)) = period.split_once('-') else {
        return Err(format!(
            "setup.period {period:?} is not YYYY-MM or YYYY-PNN"
        ));
    };
    if year.len() != 4 || year.parse::<u16>().is_err() {
        return Err(format!("setup.period {period:?} has a non-numeric year"));
    }
    let (digits, max) = match rest.strip_prefix('P') {
        // A 53-week year can carry a 14th fiscal period in some calendars; 13 is
        // the ceiling for the 4-4-5 variants the corpus uses.
        Some(d) => (d, 13),
        None => (rest, 12),
    };
    match digits.parse::<u8>() {
        Ok(n) if (1..=max).contains(&n) && digits.len() == 2 => Ok(()),
        _ => Err(format!(
            "setup.period {period:?} has an out-of-range or unpadded period number"
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "the no-unwrap rule permits unwrap in tests"
    )]

    use super::*;

    #[test]
    fn accepts_gregorian_and_fiscal_periods() {
        assert!(validate_period("2026-01").is_ok());
        assert!(validate_period("2026-12").is_ok());
        assert!(validate_period("2026-P13").is_ok());
        assert!(validate_period("2026-13").is_err());
        assert!(validate_period("2026-1").is_err());
        assert!(validate_period("26-01").is_err());
        assert!(validate_period("2026").is_err());
    }

    #[test]
    fn operation_keeps_its_parameters_and_drops_only_kind() {
        let op: Operation = toml::from_str(
            r#"
            kind = "post_entry"
            date = "2026-01-15"
            "#,
        )
        .unwrap();
        assert_eq!(op.kind, "post_entry");
        assert_eq!(op.str_param("date").unwrap(), "2026-01-15");
        assert!(op.params.get("kind").is_none());
    }

    #[test]
    fn decimal_parameters_reject_a_toml_float() {
        // The bug this prevents: `amount = 1000.10` in a corpus file is an
        // IEEE-754 double before this crate ever sees it.
        let op: Operation = toml::from_str(
            r#"
            kind = "post_entry"
            amount = 1000.10
            "#,
        )
        .unwrap();
        assert!(op.decimal_param("amount").is_err());
    }
}
