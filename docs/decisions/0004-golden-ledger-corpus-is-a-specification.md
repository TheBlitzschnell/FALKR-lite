# 0004 — The golden ledger corpus is a specification, not a regression snapshot

**Status:** accepted
**Date:** 2026-09-04

## Context

A golden-ledger test corpus was meant to exist before the first commit. It did
not, and the question "did I break anything" was answered by unit tests written
by the same process that wrote the code they test — tests that encode the
implementation's assumptions rather than the domain's rules.

There is no published corpus to download. The closest artifacts are FinBalance
(arXiv:2606.15949), whose value is its generator architecture rather than its
data, and the AICPA Audit Data Standards, which define an export schema rather
than test cases.

## Decision

`crates/testkit` plus `corpus/` hold 66 scenarios whose expected numbers were
derived by hand from the cited standard and explained in prose: the ledger-level
categories (postings and calendars, FX, the close, and the adversarial cases).
They are byte-identical to the commercial edition's copies, and
`scripts/sync-corpus-to-lite.sh` in that repository is what keeps them so — the
derivations are the expensive artifact, and two editions disagreeing about one
would be worse than not having it twice.

Four consequences follow from treating the corpus as a specification rather than
a snapshot, and each is enforced mechanically:

1. **No expected number may be produced by running the code.** A scenario whose
   numbers cannot be derived is marked `blocked` with a reason, never filled in
   from a test run.
2. **A scenario may assert that this codebase is wrong.** `status = "divergent"`
   records "the standard says X and we do Y", is asserted to *fail*, and becomes
   a test failure the moment the defect is fixed and the marker is not removed.
   Without this state the only options for a known divergence are deleting the
   scenario or changing the expectation to match the code, and both destroy the
   evidence.
3. **Deferred scenarios stay fully validated.** Arithmetic, account resolution,
   phase alignment and the prose requirement all run against scenarios whose
   features do not exist. `catalog.rs` names the phase that delivers each
   operation, and `meta.phase` is checked against it rather than trusted.
4. **Invariants that cannot pass yet are still written.** Eight are `#[ignore]`d
   with the milestone that unblocks them; each body asserts what *is* verifiable
   today — usually the shape of the gap itself — so `--include-ignored` stays
   green and the test is not a placeholder that passes by doing nothing.

`Phase::DELIVERED_THROUGH` is the single line that decides what executes.

## Consequences

**What it has already found.** The corpus caught a double-negation bug in this
crate's own AICPA export (a debit written as a negative amount was emitted as `D`
against its absolute value), and established that `falkr_core::Currency` has no
three-decimal member, which P02 §5 requires. It also holds three recorded
divergences in `LedgerAggregate`: no idempotency key on an entry, no account-level
currency restriction, and zero-amount lines accepted.

**What it costs.** Hand-derived scenarios are a real authoring investment, and
keeping the derivations honest is ongoing work that cannot be automated —
automating it is the failure mode.

**What is deliberately not covered.** A GL-level corpus cannot catch an error that
leaves the general ledger correct — `adversarial/adversarial-007` is a payment
applied to the wrong invoice, where every invariant passes and two customers have
wrong balances. That class needs per-counterparty subledger reconciliation, which this edition
does not ship. The scenario is in the corpus with that limitation written down.

## Alternatives considered

**Generate expected values from the implementation and freeze them.** This is
what most projects call a golden test suite. It catches regressions and cannot
catch an assumption that was wrong from the start, which is the failure mode this
codebase is most exposed to.

**Use an existing ERP's fixtures** (ERPNext, LedgerSMB, GnuCash, Beancount). Small,
uneven, smoke-test grade, and none of them covers ASC 606 usage-based revenue,
ASC 842 embedded leases in compute contracts, or multi-jurisdiction digital-
services tax — which are the areas where this system's exposure actually is.

**Skip the adversarial category.** It is the tedious one and it is where the bugs
are. Both bugs the corpus has found so far came from it or from the currency
category.

**Give this edition its own corpus.** Two independently authored sets of
derivations for the same double-entry rules is twice the work and half the
confidence. Sharing the files and scoping which categories ship is the cheaper
and more honest arrangement.
