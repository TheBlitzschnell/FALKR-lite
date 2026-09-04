//! AICPA **Audit Data Standards — General Ledger** extract.
//!
//! Five files, as the standard defines the GL package: GL detail, trial
//! balance, chart of accounts, source listing, business unit listing. Producing
//! them from any scenario and reconciling the extract back to the trial balance
//! catches an enormous class of completeness bugs in one assertion — an entry
//! that posts but does not export, an export that duplicates a line, a reversal
//! that appears once instead of twice — and it is precisely what an auditor does
//! to you in year one. It is also the cheapest available answer to the
//! "produce the complete journal entry population" demand in PCAOB AS 2401.
//!
//! # Two honesty notes, both load-bearing
//!
//! **1. The field roster here is a working reconstruction, not a transcription.**
//! The authoritative field list is the AICPA's own General Ledger Standard
//! spreadsheet, which is not machine-readable from a public URL. The columns
//! below are the ones the standard is built around and are enough to reconcile
//! and to hand to an audit tool, but the exact spelling and the optional columns
//! **must be diffed against the official workbook before anything here is sent
//! to a real auditor**. Columns prefixed `X_` are deliberate Fálki extensions
//! and are not part of the standard at all. Because the roster lives in the
//! `#[serde(rename)]` attributes on these structs and nowhere else, correcting
//! it later is a one-file change. See `docs/decisions/0005`.
//!
//! **2. Half the AS 2401 attribute set does not exist yet.** the AS 2401 entry-attribute rule
//! requires `entry_date`, `effective_date`, `created_at`, `posted_at`,
//! `created_by`, `approved_by`, `posted_by`, `source`, `description`,
//! `supporting_document_ref`, `reversal_of`, `reversed_by`,
//! `is_post_close_adjustment` and `is_topside` on every journal entry from
//! creation. Today's `LedgerEvent::JournalEntryPosted` carries `posted_at` and
//! the lines. The corpus supplies date, description and source from the scenario
//! file; everything else exports empty, and [`AdsExtract::missing_as2401_attributes`]
//! reports exactly which — asserted in `tests/corpus.rs`, so the list shrinks
//! visibly as P03 and P18 land and cannot silently grow.

use std::collections::BTreeMap;
use std::path::Path;

use rust_decimal::Decimal;

use crate::assertions::{Invariant, InvariantViolation};
use crate::chart::ChartOfAccounts;
use crate::runner::LedgerRun;

/// AS 2401 attributes that today's ledger cannot populate.
///
/// Pinned as a constant so that a phase landing one of them is a visible,
/// deliberate edit to this list rather than a silently changed CSV.
pub const MISSING_AS2401_ATTRIBUTES: &[&str] = &[
    "approved_by",
    "created_by",
    "entry_date",
    "is_post_close_adjustment",
    "is_topside",
    "posted_by",
    "reversed_by",
    "supporting_document_ref",
];

/// One line of the GL detail file. One row per journal line, header attributes
/// repeated — that repetition is the standard's shape, not an oversight.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GlDetailRow {
    #[serde(rename = "Journal_ID")]
    pub journal_id: String,
    #[serde(rename = "Journal_ID_Line_Number")]
    pub line_number: u32,
    #[serde(rename = "JE_Header_Description")]
    pub header_description: String,
    #[serde(rename = "JE_Line_Description")]
    pub line_description: String,
    #[serde(rename = "Source")]
    pub source: String,
    #[serde(rename = "Business_Unit_Code")]
    pub business_unit: String,
    #[serde(rename = "Effective_Date")]
    pub effective_date: String,
    #[serde(rename = "Fiscal_Year")]
    pub fiscal_year: String,
    #[serde(rename = "Period")]
    pub period: String,
    #[serde(rename = "GL_Account_Number")]
    pub account_number: String,
    /// Always the unsigned magnitude; the side is in the indicator column. That
    /// is the standard's convention, and mixing a signed amount with a D/C
    /// indicator is the classic way to double-negate a credit.
    #[serde(rename = "Amount")]
    pub amount: String,
    #[serde(rename = "Amount_Credit_Debit_Indicator")]
    pub indicator: &'static str,
    #[serde(rename = "Amount_Currency")]
    pub currency: String,
    #[serde(rename = "Entered_By")]
    pub entered_by: String,
    #[serde(rename = "Entered_Date")]
    pub entered_date: String,
    #[serde(rename = "Approved_By")]
    pub approved_by: String,
    #[serde(rename = "Approved_Date")]
    pub approved_date: String,
    #[serde(rename = "Reversal_Indicator")]
    pub reversal_indicator: &'static str,
    #[serde(rename = "Reversal_Journal_ID")]
    pub reversal_journal_id: String,
    /// Fálki extension: the usage events this revenue line was recognized from.
    /// Not in the standard; carried because the billing design's whole promise
    /// is that this trace exists in the export, not merely in the database.
    #[serde(rename = "X_Source_Usage_Event_IDs")]
    pub source_usage_event_ids: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TrialBalanceRow {
    #[serde(rename = "Business_Unit_Code")]
    pub business_unit: String,
    #[serde(rename = "GL_Account_Number")]
    pub account_number: String,
    #[serde(rename = "Fiscal_Year")]
    pub fiscal_year: String,
    #[serde(rename = "Period")]
    pub period: String,
    #[serde(rename = "Beginning_Balance")]
    pub beginning_balance: String,
    #[serde(rename = "Ending_Balance")]
    pub ending_balance: String,
    #[serde(rename = "Amount_Currency")]
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ChartRow {
    #[serde(rename = "GL_Account_Number")]
    pub account_number: String,
    #[serde(rename = "GL_Account_Name")]
    pub account_name: String,
    #[serde(rename = "Account_Type")]
    pub account_type: String,
    #[serde(rename = "FS_Caption")]
    pub fs_caption: String,
    #[serde(rename = "X_Normal_Balance")]
    pub normal_balance: &'static str,
    #[serde(rename = "X_Is_Contra")]
    pub is_contra: bool,
    #[serde(rename = "X_Subledger")]
    pub subledger: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SourceRow {
    #[serde(rename = "Source")]
    pub source: String,
    #[serde(rename = "Source_Description")]
    pub description: String,
    /// Fálki extension. AS 2401 asks which entries were manual; recording it on
    /// the source is how that question gets answered without re-deriving it per
    /// entry.
    #[serde(rename = "X_Automated")]
    pub automated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BusinessUnitRow {
    #[serde(rename = "Business_Unit_Code")]
    pub code: String,
    #[serde(rename = "Business_Unit_Name")]
    pub name: String,
    #[serde(rename = "Parent_Business_Unit_Code")]
    pub parent: String,
    #[serde(rename = "X_Functional_Currency")]
    pub functional_currency: String,
}

/// The debit/credit control totals an auditor foots the extract against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlTotal {
    pub debits: Decimal,
    pub credits: Decimal,
    pub line_count: usize,
    pub entry_count: usize,
}

/// A complete five-file GL extract.
#[derive(Debug, Clone, PartialEq)]
pub struct AdsExtract {
    pub gl_detail: Vec<GlDetailRow>,
    pub trial_balance: Vec<TrialBalanceRow>,
    pub chart_of_accounts: Vec<ChartRow>,
    pub source_listing: Vec<SourceRow>,
    pub business_unit_listing: Vec<BusinessUnitRow>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("could not write {path}: {detail}")]
    Io { path: String, detail: String },
    #[error("could not serialize {file}: {detail}")]
    Serialize { file: &'static str, detail: String },
}

impl AdsExtract {
    /// Builds the extract from an executed scenario.
    #[must_use]
    pub fn from_run(run: &LedgerRun, chart: &ChartOfAccounts) -> Self {
        let (fiscal_year, period) = split_period(&run.period);

        let mut gl_detail = Vec::new();
        for entry in &run.entries {
            for (index, line) in entry.lines.iter().enumerate() {
                let Some(signed) = line.signed_amount() else {
                    continue;
                };
                // The side is taken from the SIGN of the amount, not from
                // which field it was written in. A line written as a debit of
                // -5,000.00 — the convention several billing systems use for a
                // credit memo — is economically a credit of 5,000.00, and the
                // standard's unsigned-amount-plus-indicator format has no way
                // to express a negative debit. Reading `line.debit.is_some()`
                // here emits `D` against an absolute value and double-negates
                // the line, so the extract stops reconciling to the ledger.
                // Found by corpus/adversarial/adversarial-016.
                let is_debit = !signed.amount().is_sign_negative();
                let label = run
                    .labels
                    .get(&line.account)
                    .cloned()
                    .unwrap_or_else(|| line.account.to_string());
                let account_number = label
                    .split_once('-')
                    .map_or_else(|| label.clone(), |(code, _)| code.to_owned());
                gl_detail.push(GlDetailRow {
                    journal_id: entry.journal_id.clone(),
                    // 1-based: an auditor reads line 1, not line 0.
                    line_number: u32::try_from(index + 1).unwrap_or(u32::MAX),
                    header_description: entry.description.clone(),
                    line_description: entry.description.clone(),
                    source: entry.source.clone(),
                    business_unit: entry.entity.clone(),
                    effective_date: entry.effective_date.to_string(),
                    fiscal_year: fiscal_year.clone(),
                    period: period.clone(),
                    account_number,
                    amount: signed.amount().abs().to_string(),
                    indicator: if is_debit { "D" } else { "C" },
                    currency: signed.currency().code().to_owned(),
                    // Empty until P03/P18 land the AS 2401 attribute set; see
                    // MISSING_AS2401_ATTRIBUTES.
                    entered_by: String::new(),
                    entered_date: String::new(),
                    approved_by: String::new(),
                    approved_date: String::new(),
                    reversal_indicator: if entry.reversal_of.is_some() {
                        "Y"
                    } else {
                        "N"
                    },
                    reversal_journal_id: entry
                        .reversal_of
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    source_usage_event_ids: line
                        .source_usage_event_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("|"),
                });
            }
        }

        let trial_balance = run
            .trial_balance()
            .into_iter()
            .map(|(label, amount)| TrialBalanceRow {
                business_unit: run.entity.clone(),
                account_number: label
                    .split_once('-')
                    .map_or_else(|| label.clone(), |(code, _)| code.to_owned()),
                fiscal_year: fiscal_year.clone(),
                period: period.clone(),
                // Every corpus scenario starts from an empty ledger, so the
                // opening balance is zero by construction. When P04 lands
                // roll-forward this is read from the prior period instead, and
                // invariant 8 becomes the check that the two agree.
                beginning_balance: "0".to_owned(),
                ending_balance: amount.to_string(),
                currency: run.currency.code().to_owned(),
            })
            .collect();

        let chart_of_accounts = chart
            .accounts()
            .map(|account| ChartRow {
                account_number: account.code.clone(),
                account_name: account.name.clone(),
                account_type: account.kind.as_str().to_owned(),
                fs_caption: account.fs_caption.clone(),
                normal_balance: account.normal_balance().as_str(),
                is_contra: account.contra,
                subledger: account.subledger.clone().unwrap_or_default(),
            })
            .collect();

        let source_listing = chart
            .sources()
            .map(|source| SourceRow {
                source: source.code.clone(),
                description: source.description.clone(),
                automated: source.automated,
            })
            .collect();

        let business_unit_listing = chart
            .entities()
            .map(|entity| BusinessUnitRow {
                code: entity.code.clone(),
                name: entity.name.clone(),
                parent: entity.parent.clone().unwrap_or_default(),
                functional_currency: entity.functional_ccy.clone(),
            })
            .collect();

        Self {
            gl_detail,
            trial_balance,
            chart_of_accounts,
            source_listing,
            business_unit_listing,
        }
    }

    /// Footed debit and credit totals over the GL detail file.
    ///
    /// # Errors
    ///
    /// If an amount is not a decimal, or a total overflows.
    pub fn control_total(&self) -> Result<ControlTotal, InvariantViolation> {
        let mut debits = Decimal::ZERO;
        let mut credits = Decimal::ZERO;
        let mut journals = std::collections::BTreeSet::new();
        for row in &self.gl_detail {
            let amount = Decimal::from_str_exact(&row.amount).map_err(|e| InvariantViolation {
                invariant: Invariant::Balanced,
                detail: format!("GL detail amount {:?} is not a decimal: {e}", row.amount),
            })?;
            let slot = if row.indicator == "D" {
                &mut debits
            } else {
                &mut credits
            };
            *slot = slot.checked_add(amount).ok_or_else(|| InvariantViolation {
                invariant: Invariant::Balanced,
                detail: "GL detail control total overflowed Decimal".to_owned(),
            })?;
            journals.insert(row.journal_id.clone());
        }
        Ok(ControlTotal {
            debits,
            credits,
            line_count: self.gl_detail.len(),
            entry_count: journals.len(),
        })
    }

    /// Reconciles the extract against a trial balance.
    ///
    /// Three separate checks, because they fail for different reasons:
    /// debits equal credits (the extract is internally consistent), the balances
    /// re-derived from GL detail equal the trial balance file (the two files
    /// agree with each other), and both equal `expected` (they agree with the
    /// hand-computed answer).
    ///
    /// # Errors
    ///
    /// Naming which of the three failed and by how much.
    pub fn reconcile(
        &self,
        expected: &BTreeMap<String, Decimal>,
    ) -> Result<(), InvariantViolation> {
        let totals = self.control_total()?;
        if totals.debits != totals.credits {
            return Err(InvariantViolation {
                invariant: Invariant::Balanced,
                detail: format!(
                    "ADS GL detail does not foot: debits {} vs credits {}",
                    totals.debits, totals.credits
                ),
            });
        }

        let mut derived: BTreeMap<String, Decimal> = BTreeMap::new();
        for row in &self.gl_detail {
            let amount = Decimal::from_str_exact(&row.amount).map_err(|e| InvariantViolation {
                invariant: Invariant::TrialBalanceZero,
                detail: format!("GL detail amount {:?} is not a decimal: {e}", row.amount),
            })?;
            let signed = if row.indicator == "D" {
                amount
            } else {
                -amount
            };
            let slot = derived.entry(row.account_number.clone()).or_default();
            *slot = slot.checked_add(signed).ok_or_else(|| InvariantViolation {
                invariant: Invariant::TrialBalanceZero,
                detail: "re-derived balance overflowed Decimal".to_owned(),
            })?;
        }

        let from_tb_file: BTreeMap<String, Decimal> = self
            .trial_balance
            .iter()
            .filter_map(|row| {
                Decimal::from_str_exact(&row.ending_balance)
                    .ok()
                    .map(|amount| (row.account_number.clone(), amount))
            })
            .collect();

        compare("GL detail vs trial balance file", &derived, &from_tb_file)?;

        // `expected` is keyed by the corpus's `"1000-Cash"` labels; the extract
        // is keyed by bare account numbers, as the standard requires.
        let expected_by_number: BTreeMap<String, Decimal> = expected
            .iter()
            .map(|(label, amount)| {
                let code = label
                    .split_once('-')
                    .map_or_else(|| label.clone(), |(code, _)| code.to_owned());
                (code, *amount)
            })
            .collect();
        compare("ADS extract vs expected", &derived, &expected_by_number)
    }

    /// The AS 2401 attributes this extract cannot populate today.
    ///
    /// Returns [`MISSING_AS2401_ATTRIBUTES`] filtered to those that are still
    /// genuinely absent from the rows, so the list is derived from the data
    /// rather than merely asserted.
    #[must_use]
    pub fn missing_as2401_attributes(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        for attribute in MISSING_AS2401_ATTRIBUTES {
            let still_missing = match *attribute {
                "created_by" => self.gl_detail.iter().all(|r| r.entered_by.is_empty()),
                "approved_by" => self.gl_detail.iter().all(|r| r.approved_by.is_empty()),
                "entry_date" => self.gl_detail.iter().all(|r| r.entered_date.is_empty()),
                // The remainder have no column at all yet, which is itself the
                // finding: the export cannot carry what the entry does not hold.
                _ => true,
            };
            if still_missing {
                missing.push(*attribute);
            }
        }
        missing
    }

    /// Writes the five files into `dir`.
    ///
    /// # Errors
    ///
    /// If the directory cannot be created or any file cannot be written.
    pub fn write_csv(&self, dir: &Path) -> Result<(), ExportError> {
        std::fs::create_dir_all(dir).map_err(|e| ExportError::Io {
            path: dir.display().to_string(),
            detail: e.to_string(),
        })?;
        write_file(dir, "GL_Detail.csv", &self.gl_detail, "GL_Detail")?;
        write_file(
            dir,
            "Trial_Balance.csv",
            &self.trial_balance,
            "Trial_Balance",
        )?;
        write_file(
            dir,
            "Chart_Of_Accounts.csv",
            &self.chart_of_accounts,
            "Chart_Of_Accounts",
        )?;
        write_file(
            dir,
            "Source_Listing.csv",
            &self.source_listing,
            "Source_Listing",
        )?;
        write_file(
            dir,
            "Business_Unit_Listing.csv",
            &self.business_unit_listing,
            "Business_Unit_Listing",
        )
    }
}

fn write_file<T: serde::Serialize>(
    dir: &Path,
    name: &str,
    rows: &[T],
    file: &'static str,
) -> Result<(), ExportError> {
    let path = dir.join(name);
    let mut writer = csv::Writer::from_path(&path).map_err(|e| ExportError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    for row in rows {
        writer.serialize(row).map_err(|e| ExportError::Serialize {
            file,
            detail: e.to_string(),
        })?;
    }
    writer.flush().map_err(|e| ExportError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    })
}

fn compare(
    what: &str,
    left: &BTreeMap<String, Decimal>,
    right: &BTreeMap<String, Decimal>,
) -> Result<(), InvariantViolation> {
    let accounts: std::collections::BTreeSet<&String> = left.keys().chain(right.keys()).collect();
    for account in accounts {
        let l = left.get(account).copied().unwrap_or(Decimal::ZERO);
        let r = right.get(account).copied().unwrap_or(Decimal::ZERO);
        if l != r {
            return Err(InvariantViolation {
                invariant: Invariant::TrialBalanceZero,
                detail: format!(
                    "{what}: account {account} is {l} on one side and {r} on the other"
                ),
            });
        }
    }
    Ok(())
}

/// `"2026-01"` -> `("2026", "01")`; `"2026-P13"` -> `("2026", "P13")`.
fn split_period(period: &str) -> (String, String) {
    period.split_once('-').map_or_else(
        || (period.to_owned(), String::new()),
        |(year, rest)| (year.to_owned(), rest.to_owned()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_both_calendar_shapes() {
        assert_eq!(split_period("2026-01"), ("2026".into(), "01".into()));
        assert_eq!(split_period("2026-P13"), ("2026".into(), "P13".into()));
    }
}
