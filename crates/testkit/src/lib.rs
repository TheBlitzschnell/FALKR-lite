//! # falkr-testkit — the golden ledger corpus
//!
//! This crate is the executable half of the specification. The other half is
//! `corpus/`, a directory of declarative TOML scenarios whose expected numbers
//! were derived **by hand, from the accounting standards**, and whose derivation
//! is written out in prose next to them.
//!
//! ## The rule that gives the corpus its value
//!
//! **No expected number in `corpus/` was produced by running this codebase.**
//! A corpus built by recording current behaviour is a regression snapshot: it
//! locks in whatever the implementation does today, including the parts that are
//! wrong. A corpus derived from the standard is a specification: it can tell you
//! the implementation was wrong from the first commit. That matters here because
//! every other test in this repository was written by the same process that
//! wrote the code it tests: they encode the implementation's assumptions rather
//! than the domain's rules, and they cannot answer that question.
//!
//! Consequently, most of the corpus does not pass yet — and must not be made to
//! pass by weakening it. A scenario whose feature does not exist is *deferred*,
//! tagged with the milestone that will deliver it, and reported as such. Deferred is
//! the honest state; green-by-omission is not.
//!
//! ## Layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`chart`] | The corpus chart of accounts, loaded from `corpus/accounts.toml` |
//! | [`scenario`] | The TOML scenario format and its structural validation |
//! | [`catalog`] | Every operation kind the corpus may use, and the phase that delivers it |
//! | [`runner`] | Executes the operation kinds that exist today; defers the rest |
//! | [`assertions`] | The ten invariants, as reusable functions |
//! | [`allocation`] | The reference largest-remainder allocation rule (the precision requirements) |
//! | [`generator`] | Deterministic scenario generation from a seed |
//! | [`ads_export`] | AICPA Audit Data Standards General Ledger extract |
//! | [`fixtures`] | Seeded demo data, shared with the UI's `--demo` mode |
//!
//! ## Determinism
//!
//! Everything in this crate is a pure function of its inputs. There are no
//! wall-clock reads, no hash-map iteration, no randomness that is not seeded
//! from an explicit `u64`, and no floating point anywhere. That is not
//! stylistic: invariant 3 asserts that replaying the log reproduces the live
//! projection bit-for-bit, and a harness that is itself nondeterministic cannot
//! test for determinism.

#![deny(clippy::float_arithmetic)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]

pub mod ads_export;
pub mod allocation;
pub mod assertions;
pub mod catalog;
pub mod chart;
pub mod fixtures;
pub mod generator;
pub mod runner;
pub mod scenario;

use std::path::{Path, PathBuf};

pub use assertions::{Invariant, InvariantViolation};
pub use catalog::{Phase, catalog};
pub use chart::{Account, AccountKind, ChartOfAccounts, NormalBalance};
pub use runner::{EntryRecord, LedgerRun, RunOutcome, Runner};
pub use scenario::{Scenario, ScenarioError};

/// The repository's `corpus/` directory.
///
/// Resolved from `CARGO_MANIFEST_DIR` rather than the process working
/// directory: `cargo test -p falkr-testkit` and `cargo test --workspace` set
/// different working directories, and a corpus that loads under one and not the
/// other is a corpus that silently stops running.
#[must_use]
pub fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("corpus")
}

/// Every scenario in the corpus, ordered by path.
///
/// Ordered because a test that reports "3 failures" in a different order on
/// every run is a test nobody can bisect.
///
/// # Errors
///
/// Returns the first structural error encountered, naming the file. A corpus
/// file that does not parse is a hard failure, never a skip — a scenario that
/// quietly stops being loaded is worse than one that fails.
pub fn load_all() -> Result<Vec<Scenario>, ScenarioError> {
    load_from(&corpus_dir())
}

/// Every scenario under `root`, ordered by path.
///
/// # Errors
///
/// As [`load_all`].
pub fn load_from(root: &Path) -> Result<Vec<Scenario>, ScenarioError> {
    let mut paths = Vec::new();
    collect_toml(root, &mut paths)?;
    paths.sort();
    paths.iter().map(|p| Scenario::load(p)).collect()
}

/// Reference data (the chart of accounts, the entity list, the source list)
/// lives under `corpus/_reference/`, and is not a scenario.
///
/// One rule — a leading underscore on any path component means "not a
/// scenario" — rather than an exclusion list that someone has to remember to
/// extend when they add a second reference file.
fn is_reference(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with('_'))
}

fn collect_toml(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ScenarioError> {
    let entries = std::fs::read_dir(dir).map_err(|e| ScenarioError::Io {
        path: dir.to_path_buf(),
        detail: e.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| ScenarioError::Io {
            path: dir.to_path_buf(),
            detail: e.to_string(),
        })?;
        let path = entry.path();
        if path.is_dir() {
            if !is_reference(&path) {
                collect_toml(&path, out)?;
            }
        } else if path.extension().is_some_and(|e| e == "toml") && !is_reference(&path) {
            out.push(path);
        }
    }
    Ok(())
}
