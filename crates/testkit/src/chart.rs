//! The corpus chart of accounts, entity list and source list.
//!
//! All three are **data**, loaded from `corpus/_reference/`, not Rust
//! constants. That follows the policy-not-constants rule's reasoning one step further than
//! the rule itself requires: a chart of accounts is exactly the kind of thing a
//! customer, an auditor or a new jurisdiction changes, and a corpus whose
//! accounts are baked into a `match` arm cannot be pointed at a second chart.
//!
//! The chart also carries the metadata the invariants need and the ledger does
//! not yet model — normal balance, contra flag, subledger membership, currency
//! restriction. Once P03 lands a real chart of accounts in the database, this
//! file becomes the *expected* chart and the corpus gains a test that the two
//! agree. Until then it is the only chart there is.

use std::collections::BTreeMap;
use std::path::Path;

use falkr_core::Currency;

/// The five statement classifications. Deliberately closed: a sixth would be a
/// change to the accounting model, not a data entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Asset,
    Liability,
    Equity,
    Revenue,
    Expense,
}

impl AccountKind {
    /// The side an account of this kind normally carries, before any contra
    /// flag is applied.
    #[must_use]
    pub const fn natural_side(self) -> NormalBalance {
        match self {
            Self::Asset | Self::Expense => NormalBalance::Debit,
            Self::Liability | Self::Equity | Self::Revenue => NormalBalance::Credit,
        }
    }

    /// Whether a balance on the *wrong* side is a defect by default.
    ///
    /// Invariant 4: assets reject a credit balance (an overdrawn cash account is
    /// a bug, not a fact), liabilities and revenue permit either sign (a debit
    /// balance in deferred revenue is a real, if unhappy, state). Expenses
    /// reject by default too — a credit balance in an expense account is
    /// almost always a misposted reversal — and a scenario that legitimately
    /// produces one says so in `expected.unusual_balances`.
    #[must_use]
    pub const fn rejects_opposite_sign_by_default(self) -> bool {
        matches!(self, Self::Asset | Self::Expense)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Asset => "asset",
            Self::Liability => "liability",
            Self::Equity => "equity",
            Self::Revenue => "revenue",
            Self::Expense => "expense",
        }
    }
}

/// Which side of an account increases it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NormalBalance {
    Debit,
    Credit,
}

impl NormalBalance {
    #[must_use]
    pub const fn flip(self) -> Self {
        match self {
            Self::Debit => Self::Credit,
            Self::Credit => Self::Debit,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debit => "D",
            Self::Credit => "C",
        }
    }
}

/// One account in the corpus chart.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    /// The numeric code. Scenarios may refer to an account as `"1000"` or as
    /// `"1000-Cash"`; the second form is checked against `name`, so a typo in
    /// the name is a failure rather than a silently accepted alias.
    pub code: String,
    pub name: String,
    pub kind: AccountKind,
    /// A contra account carries the opposite side to its kind — accumulated
    /// depreciation is an asset with a credit normal balance.
    #[serde(default)]
    pub contra: bool,
    /// Overrides [`AccountKind::rejects_opposite_sign_by_default`].
    #[serde(default)]
    pub allow_opposite_sign: Option<bool>,
    /// Restricts the account to one currency (a EUR-denominated bank account).
    /// `None` means "the entity's functional currency", which is checked
    /// against the scenario's `functional_ccy` instead.
    #[serde(default)]
    pub currency: Option<String>,
    /// The subledger this account is the GL control account for, if any.
    /// Invariant 7 ties each named subledger to exactly one control account.
    #[serde(default)]
    pub subledger: Option<String>,
    /// An intercompany account. Invariant 10 requires every account carrying
    /// this flag to net to zero across the group after elimination.
    #[serde(default)]
    pub intercompany: bool,
    /// The financial-statement line this account rolls into. Carried into the
    /// AICPA ADS chart-of-accounts extract.
    pub fs_caption: String,
}

impl Account {
    #[must_use]
    pub const fn normal_balance(&self) -> NormalBalance {
        if self.contra {
            self.kind.natural_side().flip()
        } else {
            self.kind.natural_side()
        }
    }

    /// Whether a balance on the side opposite to [`Account::normal_balance`] is
    /// permitted without an explicit note in the scenario.
    #[must_use]
    pub const fn permits_opposite_sign(&self) -> bool {
        match self.allow_opposite_sign {
            Some(explicit) => explicit,
            None => !self.kind.rejects_opposite_sign_by_default(),
        }
    }

    /// `"1000-Cash"` — the form scenarios are written in and the form the
    /// failure messages print.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}-{}", self.code, self.name)
    }
}

/// A reporting entity (an AICPA ADS "business unit").
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    pub code: String,
    pub name: String,
    /// ISO 4217 code of the entity's functional currency.
    pub functional_ccy: String,
    /// Immediate parent in the consolidation tree; empty for the top entity.
    #[serde(default)]
    pub parent: Option<String>,
    /// The parent's ownership percentage, as a decimal string (`"0.80"`).
    /// Present only where it is not 1.
    #[serde(default)]
    pub ownership: Option<String>,
}

/// A journal source, in the AICPA ADS sense: the subsystem an entry came from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub code: String,
    pub description: String,
    /// `true` for sources that post without a human in the loop. AS 2401 asks
    /// which entries were manual; recording it as a property of the source is
    /// how you answer that without re-deriving it per entry.
    #[serde(default)]
    pub automated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ChartError {
    #[error("could not read {path}: {detail}")]
    Io { path: String, detail: String },
    #[error("could not parse {path}: {detail}")]
    Parse { path: String, detail: String },
    #[error("duplicate account code {0}")]
    DuplicateAccount(String),
    #[error("duplicate entity code {0}")]
    DuplicateEntity(String),
    #[error("duplicate source code {0}")]
    DuplicateSource(String),
    #[error("subledger {subledger} has more than one control account: {first} and {second}")]
    AmbiguousSubledger {
        subledger: String,
        first: String,
        second: String,
    },
    #[error("entity {entity} names {parent} as its parent, which is not in the entity list")]
    UnknownParent { entity: String, parent: String },
    #[error("{context} names currency {code}, which falkr_core::Currency does not know")]
    UnknownCurrency { context: String, code: String },
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountsFile {
    account: Vec<Account>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EntitiesFile {
    entity: Vec<Entity>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcesFile {
    source: Vec<Source>,
}

/// The loaded reference data, indexed for lookup.
#[derive(Debug, Clone)]
pub struct ChartOfAccounts {
    by_code: BTreeMap<String, Account>,
    entities: BTreeMap<String, Entity>,
    sources: BTreeMap<String, Source>,
    /// subledger name -> control account code
    controls: BTreeMap<String, String>,
}

impl ChartOfAccounts {
    /// Loads `accounts.toml`, `entities.toml` and `sources.toml` from
    /// `corpus/_reference/`.
    ///
    /// # Errors
    ///
    /// Any missing or malformed file, any duplicate code, any entity whose
    /// parent is not itself an entity, any currency `falkr_core` does not know,
    /// or any subledger claimed by two control accounts.
    pub fn load(reference_dir: &Path) -> Result<Self, ChartError> {
        let accounts: AccountsFile = read_toml(&reference_dir.join("accounts.toml"))?;
        let entities: EntitiesFile = read_toml(&reference_dir.join("entities.toml"))?;
        let sources: SourcesFile = read_toml(&reference_dir.join("sources.toml"))?;

        let mut by_code = BTreeMap::new();
        let mut controls: BTreeMap<String, String> = BTreeMap::new();
        for account in accounts.account {
            if let Some(currency) = &account.currency
                && Currency::from_code(currency).is_none()
            {
                return Err(ChartError::UnknownCurrency {
                    context: format!("account {}", account.code),
                    code: currency.clone(),
                });
            }
            if let Some(subledger) = &account.subledger
                && let Some(first) = controls.get(subledger)
            {
                return Err(ChartError::AmbiguousSubledger {
                    subledger: subledger.clone(),
                    first: first.clone(),
                    second: account.code.clone(),
                });
            }
            if by_code.contains_key(&account.code) {
                return Err(ChartError::DuplicateAccount(account.code));
            }
            if let Some(subledger) = &account.subledger {
                controls.insert(subledger.clone(), account.code.clone());
            }
            by_code.insert(account.code.clone(), account);
        }

        let mut entity_map = BTreeMap::new();
        for entity in entities.entity {
            if Currency::from_code(&entity.functional_ccy).is_none() {
                return Err(ChartError::UnknownCurrency {
                    context: format!("entity {}", entity.code),
                    code: entity.functional_ccy.clone(),
                });
            }
            if entity_map.contains_key(&entity.code) {
                return Err(ChartError::DuplicateEntity(entity.code));
            }
            entity_map.insert(entity.code.clone(), entity);
        }
        for entity in entity_map.values() {
            if let Some(parent) = entity.parent.as_ref().filter(|p| !p.is_empty())
                && !entity_map.contains_key(parent)
            {
                return Err(ChartError::UnknownParent {
                    entity: entity.code.clone(),
                    parent: parent.clone(),
                });
            }
        }

        let mut source_map = BTreeMap::new();
        for source in sources.source {
            if source_map.contains_key(&source.code) {
                return Err(ChartError::DuplicateSource(source.code));
            }
            source_map.insert(source.code.clone(), source);
        }

        Ok(Self {
            by_code,
            entities: entity_map,
            sources: source_map,
            controls,
        })
    }

    /// Resolves `"1000"` or `"1000-Cash"` to an account.
    ///
    /// Returns `None` for an unknown code *and* for a known code with the wrong
    /// name attached, which is the case that matters: `"4000-Reveneu"` should
    /// fail a corpus file, not quietly resolve.
    #[must_use]
    pub fn resolve(&self, reference: &str) -> Option<&Account> {
        let (code, name) = match reference.split_once('-') {
            Some((code, name)) => (code.trim(), Some(name.trim())),
            None => (reference.trim(), None),
        };
        let account = self.by_code.get(code)?;
        match name {
            Some(name) if name != account.name => None,
            _ => Some(account),
        }
    }

    pub fn accounts(&self) -> impl Iterator<Item = &Account> {
        self.by_code.values()
    }

    #[must_use]
    pub fn entity(&self, code: &str) -> Option<&Entity> {
        self.entities.get(code)
    }

    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values()
    }

    #[must_use]
    pub fn source(&self, code: &str) -> Option<&Source> {
        self.sources.get(code)
    }

    pub fn sources(&self) -> impl Iterator<Item = &Source> {
        self.sources.values()
    }

    /// The GL control account for a named subledger.
    #[must_use]
    pub fn control_account(&self, subledger: &str) -> Option<&Account> {
        self.controls
            .get(subledger)
            .and_then(|code| self.by_code.get(code))
    }

    /// The reference chart shipped in `corpus/_reference/`.
    ///
    /// # Errors
    ///
    /// As [`ChartOfAccounts::load`].
    pub fn corpus() -> Result<Self, ChartError> {
        Self::load(&crate::corpus_dir().join("_reference"))
    }
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ChartError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ChartError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    toml::from_str(&raw).map_err(|e| ChartError::Parse {
        path: path.display().to_string(),
        detail: e.to_string(),
    })
}
