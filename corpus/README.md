# The golden ledger corpus

A specification of what this ledger is supposed to compute, written **from the
accounting standards, by hand, before the code that implements it**.

Every expected number in this directory was derived from the paragraph cited next
to it and explained in prose underneath it. None of them was produced by running
the code.

---

## Why that rule is the whole point

Every other test in this repository was written by the same process that wrote
the code it tests, which means they encode the implementation's assumptions, not
the domain's rules. They will happily tell you nothing has regressed while the
answer has been wrong since the first commit.

A corpus written from the standards is the only artifact that can catch an
assumption that was wrong from the start. The moment you compute an expected
value by running the code, you have converted the corpus from a specification
into a regression snapshot, and you have destroyed the one property that made it
worth building.

It has already earned this twice. `corpus/adversarial/adversarial-016` found a
double-negation bug in the AICPA export in this crate — a debit written as a
negative amount was emitted as `D` against its absolute value, so the extract
stopped reconciling. `corpus/currency/currency-014` found that
`falkr_core::Currency` has no three-decimal member at all, which the corpus is
required to cover and nothing else had noticed.

**If you cannot derive a number, mark the scenario `blocked` and say what you
need. Do not fill it in from a test run.**

---

## Layout

```
corpus/
  _reference/            data every scenario resolves against
    accounts.toml        the chart of accounts, with normal balance,
                         contra/intercompany flags and subledger control links
    entities.toml        reporting entities and the consolidation tree
    sources.toml         journal sources, and which of them are automated
  core/                  postings, reversals, calendars, fiscal-year close
  currency/              ASC 830 remeasurement, translation, CTA, IAS 21
  close/                 accruals, locks, restatement, roll-forward
  adversarial/           the ones that are supposed to fail, and the ones that
                         pass while the business is wrong
```

A path component starting with `_` is reference data, not a scenario. That is the
whole rule — there is no exclusion list to keep in sync.

---

## The format

```toml
[meta]
id             = "revenue-004-prepaid-credits-partial-consumption-breakage"
title          = "Prepaid credits, partial consumption, proportional breakage"
standards      = ["ASC 606-10-55-46", "ASC 606-10-55-48"]
phase          = "P06"        # checked against the operations, never trusted alone
difficulty     = 4            # 1..5
# status       = "ready" (default) | "blocked" | "divergent"

[setup]
entity         = "US_PARENT"  # must exist in _reference/entities.toml
book           = "US_GAAP"    # US_GAAP | IFRS | TAX | MGMT
functional_ccy = "USD"
period         = "2026-01"    # YYYY-MM, or YYYY-PNN under a fiscal calendar
# calendar     = "gregorian" (default) | "4-4-5" | "53-week"

[[operations]]
kind        = "post_entry"
date        = "2026-01-05"
description = "..."
source      = "REV"           # must exist in _reference/sources.toml
lines = [
    { account = "1000-Cash", debit = "100000.00" },
    { account = "4000-Revenue", credit = "100000.00" },
]

[expected.trial_balance]      # debit-positive, exact, must sum to zero
"1000-Cash"    = "100000.00"
"4000-Revenue" = "-100000.00"

[expected.invariants]
balanced = true
replay_stable = true

[expected.notes]
reasoning = """
The derivation, in prose, by a human.
"""
```

### Conventions that are not negotiable

- **Debit-positive throughout.** Cash of 100,000 is `"100000.00"`; revenue of
  25,000 is `"-25000.00"`. One convention, stated once, because a corpus that
  mixes them encodes sign errors as facts.
- **Amounts are strings.** TOML has no decimal type, so a bare `1000.10` is an
  IEEE-754 double before this crate ever sees it — the no-floats-for-money rule
  broken in the data file rather than in the code. Every amount is parsed with
  `Decimal::from_str_exact`, which also refuses a literal that would lose
  precision.
- **Accounts are written `"1000-Cash"` or `"1000"`.** If the name is given it is
  checked against the chart, so `"4000-Reveneu"` fails rather than resolving.
- **`reasoning` is required**, at least 120 characters, and it is the field that
  makes the rest trustworthy.

---

## Three states a scenario can be in

| `status` | Meaning | The test asserts |
|---|---|---|
| `ready` (default) | Numbers derived; this codebase agrees | the trial balance matches exactly |
| `blocked` | The numbers cannot be derived yet; `blocked_reason` says what is needed | structure only; nothing is executed |
| `divergent` | Numbers derived and correct; **this codebase produces something else** | that it still fails, and that `divergence` explains why |

`divergent` is the state that makes a specification corpus worth more than a
snapshot. A snapshot has nowhere to record "the standard says X and we do Y" —
the only options are to delete the scenario or to change the expectation to match
the code, and both destroy the evidence. A divergent scenario stays visible, keeps
CI green, and turns into a failure the moment somebody fixes the defect and
forgets to promote it.

There are three today, all with a milestone named in their `divergence` note.

---

## Deferred is not skipped

Most of the corpus describes features that do not exist yet. Those scenarios are
**deferred**, not skipped: they are still parsed, their arithmetic is still
checked, their accounts still have to resolve, their prose is still required, and
their declared phase is still verified against the operations they use.

What they do not get is a comparison against a feature that is not there.

`crates/testkit/src/catalog.rs` names every operation kind and the phase that
delivers it, and a scenario's `meta.phase` must equal the highest phase among its
operations — so "which phase unblocks this" is machine-checked rather than a
comment somebody maintains. `Phase::DELIVERED_THROUGH` is the single line that
decides what executes; raising it without doing the work turns the corpus green by
fiat, which is the one way P02 fails while appearing to succeed.

```
cargo test -p falkr-testkit -- --nocapture     # prints the backlog by phase
```

---

## Adding a scenario

```bash
scripts/corpus-new.sh <category> <slug> [phase]
scripts/corpus-new.sh --list-operations
```

The scaffold is full of `TODO`s and every one of them is a required field, so a
half-written scenario fails `cargo test -p falkr-testkit` rather than sitting in
the corpus looking finished.

Then:

1. Derive the expected numbers from the standard, by hand.
2. Write the derivation in `expected.notes.reasoning` — show the arithmetic, name
   the judgement, and say what the wrong answer would be and why somebody would
   reach it.
3. `cargo test -p falkr-testkit --test corpus`

The structural checks that run before anything is executed:

- the expected trial balance sums to exactly zero (catches a hand-arithmetic slip
  without running a line of production code — this is the single highest-value
  check in the suite);
- every account reference resolves against the chart, name included;
- every operation kind is in the catalog;
- the declared phase equals the highest phase among the operations;
- a balance on the wrong side of an account is either permitted by its type or
  declared in `expected.unusual_balances` — and a declaration that is no longer
  true is also a failure, so stale annotations cannot accumulate;
- `expected.notes.reasoning` is present and substantial.

---

## Known limitations

- **This edition carries four of the nine corpus categories.** Revenue
  recognition, fixed assets and leases, tax determination, payroll and equity
  compensation, and consolidation specify subledgers this edition does not ship.
  The four here are the ledger-level ones, and they are byte-identical to the
  commercial edition's copies.
- **The AICPA ADS field roster is a working reconstruction.** The authoritative
  field list is the AICPA's General Ledger Standard workbook, which is not
  machine-readable from a public URL. The columns in
  `crates/testkit/src/ads_export.rs` are enough to reconcile and to hand to an
  audit tool, and they **must be diffed against the official workbook before
  anything is sent to a real auditor**. Columns prefixed `X_` are deliberate
  extensions and are not part of the standard. The roster lives in the
  `#[serde(rename)]` attributes and nowhere else, so correcting it is a one-file
  change.
- **Eight of the fourteen PCAOB AS 2401 entry attributes do not exist yet.**
  `AdsExtract::missing_as2401_attributes` reports which, and a test pins the list
  so it shrinks visibly as P03 and P18 land and cannot silently grow.
- **No three-decimal currency.** See `currency/currency-014`.
