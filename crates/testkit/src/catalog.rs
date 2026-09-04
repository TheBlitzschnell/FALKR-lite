//! The operation vocabulary, and the phase that delivers each operation.
//!
//! The vocabulary is deliberately the *full* accounting vocabulary, including
//! operations for subledgers this edition does not ship. It is a description of
//! what a scenario file may say, not a roadmap: keeping it complete is what lets
//! a scenario be moved between editions unchanged, and trimming it would mean a
//! synced scenario failing validation for a reason that has nothing to do with
//! its accounting.
//!
//! Two jobs, and the second is the interesting one.
//!
//! 1. **Typo protection.** A scenario using `kind = "conusme_credits"` fails
//!    loudly instead of being silently treated as an unimplemented feature and
//!    skipped forever. Anything not in this table is an error.
//! 2. **Making `meta.phase` machine-checked.** Each operation names the phase
//!    that delivers it; a scenario's declared phase must equal the highest phase
//!    among its operations. That turns "which phase unblocks this scenario"
//!    from a comment someone maintains by hand into something the test suite
//!    verifies, so the corpus cannot drift out of alignment with the build plan.
//!
//! Adding a feature therefore has a mechanical consequence here: when P04 lands
//! period close, `close_period` moves from *catalogued* to *registered* in
//! [`crate::runner`], and every scenario that uses it stops being deferred.
//! Nothing about the scenario file changes.

use core::fmt;
use core::str::FromStr;

/// A roadmap milestone.
///
/// Ordered, because "is this scenario runnable yet" is a comparison. The numbers
/// are shared with the commercial edition so that a corpus scenario is
/// byte-identical in both and can be synced rather than re-derived; what each
/// milestone delivers is in `corpus/README.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Phase(u8);

impl Phase {
    pub const P02: Self = Self(2);
    pub const P03: Self = Self(3);
    pub const P04: Self = Self(4);
    pub const P05: Self = Self(5);
    pub const P06: Self = Self(6);
    pub const P07: Self = Self(7);
    pub const P08: Self = Self(8);
    pub const P09: Self = Self(9);
    pub const P10: Self = Self(10);
    pub const P11: Self = Self(11);
    pub const P12: Self = Self(12);
    pub const P15: Self = Self(15);
    pub const P18: Self = Self(18);

    /// The last milestone whose features exist in this workspace.
    ///
    /// **This is the one line to change when a milestone lands.** Everything else —
    /// which scenarios execute, which are reported as deferred, which
    /// invariants are expected to pass — follows from it. Raising it without
    /// having done the work turns the corpus green by fiat, which is the single
    /// way P02 can fail while appearing to succeed.
    pub const DELIVERED_THROUGH: Self = Self::P02;

    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }

    /// Whether the workspace can execute operations delivered by this phase.
    #[must_use]
    pub const fn is_delivered(self) -> bool {
        self.0 <= Self::DELIVERED_THROUGH.0
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "P{:02}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a phase identifier; expected P02..P21")]
pub struct PhaseParseError(String);

impl FromStr for Phase {
    type Err = PhaseParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let n = s
            .strip_prefix('P')
            .and_then(|d| d.parse::<u8>().ok())
            .filter(|n| (2..=21).contains(n))
            .ok_or_else(|| PhaseParseError(s.to_owned()))?;
        Ok(Self(n))
    }
}

impl<'de> serde::Deserialize<'de> for Phase {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// One operation kind the corpus may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationSpec {
    /// The value of `kind` in a `[[operations]]` table.
    pub kind: &'static str,
    /// The phase that makes this operation executable.
    pub phase: Phase,
    /// What it does, in one line. Printed by `scripts/corpus-new.sh --list`.
    pub summary: &'static str,
}

const fn op(kind: &'static str, phase: Phase, summary: &'static str) -> OperationSpec {
    OperationSpec {
        kind,
        phase,
        summary,
    }
}

/// Every operation kind, with the phase that delivers it.
///
/// Kept sorted by phase then kind so a diff that adds one is readable.
pub const OPERATIONS: &[OperationSpec] = &[
    // ---- P02: the ledger primitives that exist today -------------------
    op(
        "post_entry",
        Phase::P02,
        "Post an explicit, hand-written journal entry",
    ),
    op(
        "reverse_entry",
        Phase::P02,
        "Reverse a previously posted entry by its `ref`",
    ),
    op(
        "set_capitalization",
        Phase::P02,
        "Change a run's capitalize-vs-expense status",
    ),
    // ---- P03: chart of accounts and the posting engine -----------------
    op(
        "open_balances",
        Phase::P03,
        "Seed opening balances for a period (posts against opening equity)",
    ),
    op(
        "post_by_rule",
        Phase::P03,
        "Post an entry derived from a business event by a posting rule",
    ),
    op(
        "void_entry",
        Phase::P03,
        "Void an entry before it is posted (distinct from reversing a posted one)",
    ),
    // ---- P04: period close and controls --------------------------------
    op(
        "advance_to",
        Phase::P04,
        "Move the scenario clock to a date",
    ),
    op("accrue", Phase::P04, "Post an accrual"),
    op(
        "reverse_accrual",
        Phase::P04,
        "Auto-reverse an accrual in the following period",
    ),
    op(
        "amortize_prepaid",
        Phase::P04,
        "Run one period of a prepaid amortization schedule",
    ),
    op("close_period", Phase::P04, "Close a period for a book"),
    op(
        "close_year",
        Phase::P04,
        "Close revenue and expense to retained earnings",
    ),
    op(
        "lock_subledger",
        Phase::P04,
        "Lock one subledger while leaving GL open",
    ),
    op(
        "post_close_adjustment",
        Phase::P04,
        "Post an adjustment into a closed period",
    ),
    op("reopen_period", Phase::P04, "Reopen a closed period"),
    op(
        "attempt_mutation",
        Phase::P04,
        "Attempt a mutation that must be refused, naming the entry point",
    ),
    op(
        "restate",
        Phase::P04,
        "Big-R restatement of a prior issued period",
    ),
    op(
        "revise",
        Phase::P04,
        "little-r revision recorded in the current period",
    ),
    // ---- P05: multi-currency and FX ------------------------------------
    op("set_fx_rate", Phase::P05, "Publish an FX rate for a date"),
    op(
        "revalue_monetary",
        Phase::P05,
        "ASC 830 period-end remeasurement of monetary balances",
    ),
    op(
        "translate_entity",
        Phase::P05,
        "Translate a foreign entity's balances, producing CTA",
    ),
    op(
        "settle_balance",
        Phase::P05,
        "Settle a foreign-currency balance, realizing FX",
    ),
    op(
        "flag_long_term_intercompany",
        Phase::P05,
        "Mark or unmark an intercompany balance as long-term-investment nature",
    ),
    op(
        "mark_currency_non_exchangeable",
        Phase::P05,
        "IAS 21 lack-of-exchangeability estimation",
    ),
    op(
        "release_cta",
        Phase::P05,
        "Release CTA to income on disposal of a foreign operation",
    ),
    // ---- P06: revenue recognition --------------------------------------
    op("create_contract", Phase::P06, "Open an ASC 606 contract"),
    op(
        "allocate_price",
        Phase::P06,
        "Allocate the transaction price across performance obligations",
    ),
    op("invoice", Phase::P06, "Raise an invoice"),
    op(
        "recognize_ratably",
        Phase::P06,
        "Recognize one period of a ratable schedule",
    ),
    op("record_usage", Phase::P06, "Record metered usage"),
    op(
        "sell_credits",
        Phase::P06,
        "Sell prepaid credits, possibly at a discount",
    ),
    op("consume_credits", Phase::P06, "Consume prepaid credits"),
    op("expire_credits", Phase::P06, "Expire unconsumed credits"),
    op(
        "true_up_commitment",
        Phase::P06,
        "True up a minimum commitment against actual usage",
    ),
    op(
        "modify_contract",
        Phase::P06,
        "Apply a contract modification (prospective or cumulative catch-up)",
    ),
    op(
        "issue_credit_memo",
        Phase::P06,
        "Issue a credit memo against an invoice",
    ),
    op(
        "record_agent_revenue",
        Phase::P06,
        "Record resold third-party inference gross or net",
    ),
    // ---- P07: accounts receivable --------------------------------------
    op("receive_payment", Phase::P07, "Apply cash to a receivable"),
    op(
        "record_cecl_allowance",
        Phase::P07,
        "Record or adjust the CECL allowance",
    ),
    op(
        "write_off_receivable",
        Phase::P07,
        "Write a receivable off against the allowance",
    ),
    // ---- P08: accounts payable -----------------------------------------
    op("receive_goods", Phase::P08, "Goods receipt into GR/IR"),
    op(
        "record_vendor_invoice",
        Phase::P08,
        "Record a vendor invoice",
    ),
    op("pay_vendor", Phase::P08, "Pay a vendor invoice"),
    // ---- P09: fixed assets, capitalization, leases ----------------------
    op("acquire_asset", Phase::P09, "Acquire a depreciable asset"),
    op(
        "place_in_service",
        Phase::P09,
        "Place an asset in service on a date",
    ),
    op("depreciate", Phase::P09, "Run one period of depreciation"),
    op(
        "change_useful_life",
        Phase::P09,
        "Change an asset's remaining life (prospective only)",
    ),
    op(
        "test_impairment",
        Phase::P09,
        "Run the two-step long-lived asset impairment test",
    ),
    op(
        "dispose_asset",
        Phase::P09,
        "Dispose of an asset, fully or partly",
    ),
    op(
        "commence_lease",
        Phase::P09,
        "Recognize a lease at commencement",
    ),
    op(
        "identify_embedded_lease",
        Phase::P09,
        "Identify a lease embedded in a services contract and separate its components",
    ),
    op(
        "lease_payment",
        Phase::P09,
        "Record one lease payment period",
    ),
    op(
        "remeasure_lease",
        Phase::P09,
        "Remeasure a lease on a change in term or payments",
    ),
    op(
        "record_commitment_disclosure",
        Phase::P09,
        "Record a non-lease purchase commitment for the ASC 440 table",
    ),
    // ---- P10: tax determination ----------------------------------------
    op(
        "determine_tax",
        Phase::P10,
        "Run tax determination on a sale",
    ),
    op(
        "apply_exemption_certificate",
        Phase::P10,
        "Apply (or fail to apply) an exemption certificate",
    ),
    op(
        "record_withholding",
        Phase::P10,
        "Record withholding tax on an inbound payment",
    ),
    // ---- P11: payroll and equity compensation --------------------------
    op("run_payroll", Phase::P11, "Run one payroll"),
    op("accrue_bonus", Phase::P11, "Accrue a bonus"),
    op("pay_bonus", Phase::P11, "Pay an accrued bonus"),
    op("grant_equity", Phase::P11, "Grant options or RSUs"),
    op("vest_tranche", Phase::P11, "Recognize one vesting tranche"),
    op("forfeit_grant", Phase::P11, "Forfeit an unvested grant"),
    op(
        "liquidity_event",
        Phase::P11,
        "Satisfy the second trigger on double-trigger RSUs",
    ),
    op(
        "modify_grant",
        Phase::P11,
        "Reprice or otherwise modify a grant",
    ),
    op(
        "early_exercise",
        Phase::P11,
        "Early-exercise unvested options",
    ),
    // ---- P12: consolidation and intercompany ---------------------------
    op(
        "eliminate_intercompany",
        Phase::P12,
        "Eliminate intercompany balances and profit",
    ),
    op(
        "attribute_to_nci",
        Phase::P12,
        "Attribute income and unrealized profit to non-controlling interests",
    ),
    op(
        "consolidate",
        Phase::P12,
        "Roll subsidiaries up to the parent",
    ),
    op(
        "record_equity_method",
        Phase::P12,
        "Record an equity-method investee's results",
    ),
    // ---- P15: reporting -------------------------------------------------
    op(
        "assert_export",
        Phase::P15,
        "Produce a statutory export and reconcile it to the ledger",
    ),
];

/// Looks up an operation kind.
#[must_use]
pub fn spec(kind: &str) -> Option<&'static OperationSpec> {
    OPERATIONS.iter().find(|o| o.kind == kind)
}

/// The whole catalog, for tooling that wants to print it.
#[must_use]
pub const fn catalog() -> &'static [OperationSpec] {
    OPERATIONS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_round_trip() {
        assert_eq!("P06".parse::<Phase>(), Ok(Phase::P06));
        assert_eq!(Phase::P06.to_string(), "P06");
        assert!("P01".parse::<Phase>().is_err(), "P01 predates the corpus");
        assert!("P22".parse::<Phase>().is_err());
        assert!("6".parse::<Phase>().is_err());
    }

    #[test]
    fn phase_ordering_drives_deferral() {
        assert!(Phase::P02.is_delivered());
        assert!(!Phase::P06.is_delivered());
        assert!(Phase::P03 < Phase::P04);
    }

    #[test]
    fn catalog_kinds_are_unique() {
        let mut kinds: Vec<&str> = OPERATIONS.iter().map(|o| o.kind).collect();
        kinds.sort_unstable();
        let before = kinds.len();
        kinds.dedup();
        assert_eq!(
            before,
            kinds.len(),
            "duplicate operation kind in the catalog"
        );
    }

    #[test]
    fn catalog_entries_are_grouped_by_ascending_phase() {
        // Not cosmetic: the table is the readable statement of what each phase
        // unlocks, and a kind filed under the wrong phase would defer or run a
        // scenario at the wrong time.
        let phases: Vec<u8> = OPERATIONS.iter().map(|o| o.phase.number()).collect();
        let mut sorted = phases.clone();
        sorted.sort_unstable();
        assert_eq!(phases, sorted);
    }
}
