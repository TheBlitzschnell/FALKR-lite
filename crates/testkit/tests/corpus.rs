//! Runs the whole corpus.
//!
//! Three layers, in increasing order of what they need from the codebase:
//!
//! 1. **Structural validation** — runs against every scenario today, including
//!    the ones whose features do not exist. Arithmetic, account references,
//!    phase alignment, the prose requirement.
//! 2. **Execution** — for scenarios whose operations are all delivered, posts
//!    them through the real aggregate and asserts the trial balance to the cent.
//! 3. **Export conformance** — for every executed scenario, builds the AICPA GL
//!    extract and reconciles it back to the trial balance.
//!
//! A scenario that cannot be executed yet is *deferred*, not skipped: layer 1
//! still runs, and [`deferred_scenarios_are_reported`] prints the backlog by
//! phase so it is visible rather than forgotten.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the no-unwrap rule scopes the ban to non-test code"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use falkr_testkit::ads_export::AdsExtract;
use falkr_testkit::assertions;
use falkr_testkit::catalog::Phase;
use falkr_testkit::chart::ChartOfAccounts;
use falkr_testkit::runner::{RunOutcome, Runner};
use falkr_testkit::scenario::{Scenario, Status};

/// A floor, so the corpus cannot quietly shrink. Raise it when you add
/// scenarios; never lower it to make a deletion pass.
const MINIMUM_SCENARIOS: usize = 65;

/// The categories this edition covers. A missing directory is a coverage
/// regression, and "we deleted the adversarial ones because they were tedious"
/// is exactly the failure the corpus discipline warns about.
///
/// The four here are the ledger-level ones: postings and calendars, FX, the
/// close, and the adversarial cases. Revenue recognition, fixed assets and
/// leases, tax determination, payroll and equity compensation, and
/// consolidation all belong to subledgers this edition does not ship, so
/// specifying them here would be specifying features that will never arrive.
const REQUIRED_CATEGORIES: &[&str] = &["adversarial", "close", "core", "currency"];

/// The corpus, parsed once per test process.
///
/// Every test below needs the whole corpus, and `cargo test` runs them in
/// threads inside one process. Without this, each test re-reads and re-parses
/// every scenario file — fine at today's size, and the cost grows with the
/// product of scenarios and tests, which is the direction both are meant to
/// grow in. Parsing once also guarantees every test sees identical input, so a
/// failure cannot depend on which test observed the directory.
static CORPUS: OnceLock<Vec<Scenario>> = OnceLock::new();
static CHART: OnceLock<ChartOfAccounts> = OnceLock::new();

fn chart() -> &'static ChartOfAccounts {
    CHART.get_or_init(|| ChartOfAccounts::corpus().expect("corpus/_reference must load"))
}

fn scenarios() -> &'static [Scenario] {
    CORPUS.get_or_init(|| falkr_testkit::load_all().expect("every corpus file must parse"))
}

// ---------------------------------------------------------------------------
// Layer 1: structure
// ---------------------------------------------------------------------------

#[test]
fn every_scenario_is_structurally_valid() {
    let chart = chart();
    let mut defects = Vec::new();
    for scenario in scenarios() {
        defects.extend(scenario.validate(chart));
    }
    assert!(
        defects.is_empty(),
        "{} structural defect(s) in the corpus:\n{}",
        defects.len(),
        defects
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The check the CI requirements asks CI to run as its own step. Named so `cargo test
/// --test corpus every_scenario_documents_its_reasoning` reads as what it is in
/// the CI log.
///
/// Implemented here rather than in a separate script on purpose: the same rule
/// enforced in two places drifts, and the version that drifts is always the one
/// nobody runs locally.
#[test]
fn every_scenario_documents_its_reasoning() {
    let mut undocumented = Vec::new();
    for scenario in scenarios() {
        if scenario.expected.notes.reasoning.trim().len() < 120 {
            undocumented.push(scenario.meta.id.clone());
        }
    }
    assert!(
        undocumented.is_empty(),
        "these scenarios have no usable derivation in expected.notes.reasoning — \
         expected numbers that cannot be explained in prose are guesses:\n  {}",
        undocumented.join("\n  ")
    );
}

#[test]
fn scenario_ids_are_unique() {
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for scenario in scenarios() {
        seen.entry(scenario.meta.id.clone())
            .or_default()
            .push(scenario.path.display().to_string());
    }
    let duplicates: Vec<_> = seen.iter().filter(|(_, paths)| paths.len() > 1).collect();
    assert!(
        duplicates.is_empty(),
        "duplicate scenario ids: {duplicates:?}"
    );
}

#[test]
fn the_corpus_meets_its_size_and_coverage_floor() {
    let scenarios = scenarios();
    assert!(
        scenarios.len() >= MINIMUM_SCENARIOS,
        "the corpus has {} scenarios; the floor is {MINIMUM_SCENARIOS}",
        scenarios.len()
    );
    let categories: BTreeSet<String> = scenarios.iter().map(Scenario::category).collect();
    for required in REQUIRED_CATEGORIES {
        assert!(
            categories.contains(*required),
            "corpus category {required:?} is missing entirely"
        );
    }
}

#[test]
fn blocked_scenarios_say_what_they_need() {
    for scenario in scenarios() {
        if scenario.meta.status == Status::Blocked {
            let reason = scenario.meta.blocked_reason.clone().unwrap_or_default();
            assert!(
                reason.len() > 40,
                "{}: blocked_reason must say what is needed, not merely that \
                 something is",
                scenario.meta.id
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Layer 2: execution, one test per category so failures parallelise and read
// separately
// ---------------------------------------------------------------------------

fn run_category(category: &str) {
    let chart = chart();
    let runner = Runner::new(chart.clone());
    let mut failures = Vec::new();
    let mut executed = 0_usize;

    for scenario in scenarios().iter().filter(|s| s.category() == category) {
        let divergent = scenario.meta.status == Status::Divergent;
        let outcome = match runner.run(scenario) {
            Ok(outcome) => outcome,
            Err(e) => {
                if divergent {
                    // The documented divergence, showing up exactly where the
                    // scenario says it will.
                    continue;
                }
                failures.push(format!("{}: {e}", scenario.meta.id));
                continue;
            }
        };
        let RunOutcome::Executed(run) = outcome else {
            continue;
        };
        executed += 1;

        let actual = run.trial_balance();
        let expected = match scenario.expected_trial_balance() {
            Ok(expected) => expected,
            Err(defect) => {
                failures.push(defect.to_string());
                continue;
            }
        };
        // Compare on account code, since the run labels accounts as
        // "1000-Cash" and a scenario may have written "1000".
        let normalize = |m: &BTreeMap<String, rust_decimal::Decimal>| -> BTreeMap<String, rust_decimal::Decimal> {
            m.iter()
                .map(|(label, amount)| {
                    let code = label
                        .split_once('-')
                        .map_or_else(|| label.clone(), |(code, _)| code.to_owned());
                    (code, *amount)
                })
                .collect()
        };
        let actual = normalize(&actual);
        let expected = normalize(&expected);
        match (actual == expected, divergent) {
            (true, false) => {}
            (false, true) => continue,
            (false, false) => {
                failures.push(format!(
                    "{}: trial balance mismatch\n  expected: {expected:?}\n  actual:   {actual:?}",
                    scenario.meta.id
                ));
                continue;
            }
            (true, true) => {
                failures.push(format!(
                    "{}: marked status = \"divergent\" but it now produces the expected \
                     result. The defect it documents has been fixed \u{2014} promote it to \
                     status = \"ready\" and delete the `divergence` note.\n  divergence: {}",
                    scenario.meta.id,
                    scenario.meta.divergence.as_deref().unwrap_or("")
                ));
                continue;
            }
        }

        // The invariants the scenario claims for itself.
        if scenario.expected.invariants.balanced {
            for entry in &run.entries {
                if let Err(e) = assertions::assert_balanced(&entry.lines) {
                    failures.push(format!("{}: {e}", scenario.meta.id));
                }
            }
        }
        if scenario.expected.invariants.replay_stable {
            if let Err(e) = assertions::assert_replay_stable(&run.events, &run.state) {
                failures.push(format!("{}: {e}", scenario.meta.id));
            }
            if let Err(e) = assertions::assert_events_round_trip(&run.events) {
                failures.push(format!("{}: {e}", scenario.meta.id));
            }
        }
        if scenario.expected.invariants.intercompany_eliminated
            && let Err(e) = assertions::assert_intercompany_eliminated(&actual, chart)
        {
            failures.push(format!("{}: {e}", scenario.meta.id));
        }

        let declared: BTreeSet<String> = scenario
            .expected
            .unusual_balances
            .iter()
            .map(|reference| {
                reference
                    .split_once('-')
                    .map_or_else(|| reference.clone(), |(code, _)| code.to_owned())
            })
            .collect();
        if let Err(e) =
            assertions::assert_no_unexpected_negative_balances(&actual, chart, &declared)
        {
            failures.push(format!("{}: {e}", scenario.meta.id));
        }
    }

    assert!(
        failures.is_empty(),
        "{} failure(s) in corpus category {category:?} ({executed} scenario(s) executed):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

macro_rules! category_tests {
    ($($name:ident => $category:literal),+ $(,)?) => {
        $(
            #[test]
            fn $name() {
                run_category($category);
            }
        )+
    };
}

category_tests! {
    adversarial_scenarios => "adversarial",
    close_scenarios => "close",
    core_scenarios => "core",
    currency_scenarios => "currency",
}

// ---------------------------------------------------------------------------
// Layer 3: export conformance
// ---------------------------------------------------------------------------

#[test]
fn the_ads_extract_reconciles_for_every_executed_scenario() {
    let chart = chart();
    let runner = Runner::new(chart.clone());
    let mut failures = Vec::new();
    let mut reconciled = 0_usize;

    for scenario in scenarios() {
        if scenario.meta.status == Status::Divergent {
            // Reconciling an extract against a trial balance the code is known
            // to get wrong would assert the wrong thing.
            continue;
        }
        let Ok(RunOutcome::Executed(run)) = runner.run(scenario) else {
            continue;
        };
        let extract = AdsExtract::from_run(&run, chart);
        match extract.reconcile(&run.trial_balance()) {
            Ok(()) => reconciled += 1,
            Err(e) => failures.push(format!("{}: {e}", scenario.meta.id)),
        }
    }

    assert!(
        failures.is_empty(),
        "ADS extract failed to reconcile for {} scenario(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        reconciled > 0,
        "no scenario reconciled, which means this test proves nothing"
    );
}

/// Pins the AS 2401 gap so it shrinks visibly and cannot grow silently.
///
/// the AS 2401 entry-attribute rule lists fourteen attributes every journal entry must carry
/// from creation. Today's `LedgerEvent::JournalEntryPosted` carries `posted_at`
/// and the lines. This test asserts exactly which attributes are still missing;
/// when P03 lands the entry header, the list here shrinks in the same commit.
#[test]
fn the_as2401_attribute_gap_is_exactly_as_documented() {
    let chart = chart();
    let runner = Runner::new(chart.clone());
    let scenario = scenarios()
        .iter()
        .find(|s| {
            s.meta.status == Status::Ready && matches!(runner.run(s), Ok(RunOutcome::Executed(_)))
        })
        .expect("at least one scenario must be executable");
    let RunOutcome::Executed(run) = runner.run(scenario).unwrap() else {
        unreachable!("filtered above")
    };
    let extract = AdsExtract::from_run(&run, chart);
    assert_eq!(
        extract.missing_as2401_attributes(),
        falkr_testkit::ads_export::MISSING_AS2401_ATTRIBUTES,
        "the AS 2401 gap has changed. If a phase filled one of these in, remove \
         it from MISSING_AS2401_ATTRIBUTES in the same commit; if one has \
         appeared, an attribute regressed"
    );
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

/// Prints the deferred backlog by phase.
///
/// Not an assertion about how much is deferred — that number is expected to be
/// large today and to fall as phases land. It is here so `cargo test -p
/// falkr-testkit -- --nocapture` answers "what does P06 unblock?" without
/// anyone maintaining a list by hand.
#[test]
fn deferred_scenarios_are_reported() {
    let runner = Runner::new(chart().clone());
    let mut by_phase: BTreeMap<Phase, Vec<String>> = BTreeMap::new();
    let mut executed = 0_usize;
    let mut blocked = Vec::new();
    let mut divergent = Vec::new();

    for scenario in scenarios() {
        match runner.run(scenario) {
            Ok(RunOutcome::Executed(_)) => executed += 1,
            Ok(RunOutcome::Deferred { phase, .. }) => {
                by_phase
                    .entry(phase)
                    .or_default()
                    .push(scenario.meta.id.clone());
            }
            Ok(RunOutcome::Blocked { .. }) => blocked.push(scenario.meta.id.clone()),
            Err(e) if scenario.meta.status == Status::Divergent => {
                divergent.push(format!("{}: {e}", scenario.meta.id));
            }
            Err(e) => panic!("{}: {e}", scenario.meta.id),
        }
    }

    println!(
        "\ncorpus status (delivered through {})",
        Phase::DELIVERED_THROUGH
    );
    println!("  executed: {executed}");
    for (phase, ids) in &by_phase {
        println!("  deferred to {phase}: {}", ids.len());
    }
    println!(
        "  blocked (expectations not yet derivable): {}",
        blocked.len()
    );
    for id in &blocked {
        println!("    {id}");
    }
    let recorded_divergences: Vec<String> = scenarios()
        .iter()
        .filter(|s| s.meta.status == Status::Divergent)
        .map(|s| {
            format!(
                "{}: {}",
                s.meta.id,
                s.meta.divergence.clone().unwrap_or_default()
            )
        })
        .collect();
    println!(
        "  divergent (the standard and this codebase disagree): {}",
        recorded_divergences.len()
    );
    for line in &recorded_divergences {
        println!("    {line}");
    }
    let _ = &divergent;

    assert!(
        executed > 0,
        "nothing in the corpus executes; the runner and the catalog have drifted"
    );
}
