//! AWS connector over FOCUS-format Data Exports (CUR 2.0).
//!
//! AWS emits FOCUS 1.x directly, so the mapping below is column-to-field. The
//! parts that are genuinely AWS-specific are the tag column convention
//! (`resourceTags/user:<key>`, flattened into a nested object in some export
//! variants) and the fact that AWS restates the open billing period repeatedly
//! until the invoice finalizes — which is why every row carries a content hash
//! and the store upserts rather than inserts.

use chrono::{DateTime, Utc};
use falkr_core::{Currency, Money};
use rust_decimal::Decimal;
use sha2::{Digest as _, Sha256};

use crate::attribution::tags::{AttributionContext, TagAttributionRules, attribute};
use crate::connector::{ConnectorError, Cursor, NormalizeError, RawCostRecord};
use crate::event::{ChargeCategory, CostEvent, ProviderKind, ServiceCategory, SourceRef};

/// Where FOCUS export files are read from.
///
/// Abstracted so the connector can be exercised against real export bytes
/// without AWS credentials. The S3-backed implementation lands with the
/// a background polling loop; it is a different `impl` of this trait,
/// not a change to the connector.
#[async_trait::async_trait]
pub trait FocusExportSource: Send + Sync {
    /// Lists export identifiers available at or after `cursor`, oldest first.
    async fn list_exports(&self, cursor: &Cursor) -> Result<Vec<ExportRef>, ConnectorError>;

    /// Reads one export's CSV bytes.
    async fn read_export(&self, export: &ExportRef) -> Result<Vec<u8>, ConnectorError>;
}

/// Identifies one delivered export file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportRef {
    /// e.g. an S3 key, or a path under a local export directory.
    pub key: String,
    /// e.g. `2026-08`.
    pub billing_period: String,
}

/// An in-memory export source, for tests and for replaying a downloaded export.
#[derive(Debug, Clone, Default)]
pub struct InMemoryExportSource {
    exports: Vec<(ExportRef, Vec<u8>)>,
}

impl InMemoryExportSource {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_export(mut self, key: &str, billing_period: &str, csv: &str) -> Self {
        self.exports.push((
            ExportRef {
                key: key.to_owned(),
                billing_period: billing_period.to_owned(),
            },
            csv.as_bytes().to_vec(),
        ));
        self
    }
}

#[async_trait::async_trait]
impl FocusExportSource for InMemoryExportSource {
    async fn list_exports(&self, cursor: &Cursor) -> Result<Vec<ExportRef>, ConnectorError> {
        // Resume *at* the cursor rather than after it: AWS restates an open
        // period in place, so the last export seen may have changed since.
        let start = cursor.last_export_ref.as_ref().map_or(0, |last| {
            self.exports
                .iter()
                .position(|(e, _)| &e.key == last)
                .unwrap_or(0)
        });
        Ok(self
            .exports
            .iter()
            .skip(start)
            .map(|(e, _)| e.clone())
            .collect())
    }

    async fn read_export(&self, export: &ExportRef) -> Result<Vec<u8>, ConnectorError> {
        self.exports
            .iter()
            .find(|(e, _)| e.key == export.key)
            .map(|(_, bytes)| bytes.clone())
            .ok_or_else(|| ConnectorError::MalformedExport {
                export_ref: export.key.clone(),
                detail: "export not found".to_owned(),
            })
    }
}

/// Reads AWS FOCUS exports and normalizes them onto the dimensional spine.
pub struct AwsFocusConnector<S: FocusExportSource> {
    source: S,
    rules: TagAttributionRules,
    ctx: AttributionContext,
    /// Currency the billing account invoices in. FOCUS carries `BillingCurrency`
    /// per row; this is the fallback when a row omits it.
    default_currency: Currency,
}

impl<S: FocusExportSource> AwsFocusConnector<S> {
    #[must_use]
    pub const fn new(
        source: S,
        rules: TagAttributionRules,
        ctx: AttributionContext,
        default_currency: Currency,
    ) -> Self {
        Self {
            source,
            rules,
            ctx,
            default_currency,
        }
    }
}

/// FOCUS column names, spelled once.
mod col {
    pub const BILLING_ACCOUNT_ID: &str = "BillingAccountId";
    pub const BILLING_CURRENCY: &str = "BillingCurrency";
    pub const CHARGE_PERIOD_START: &str = "ChargePeriodStart";
    pub const CHARGE_PERIOD_END: &str = "ChargePeriodEnd";
    pub const BILLED_COST: &str = "BilledCost";
    pub const EFFECTIVE_COST: &str = "EffectiveCost";
    pub const LIST_COST: &str = "ListCost";
    pub const SERVICE_NAME: &str = "ServiceName";
    pub const SERVICE_CATEGORY: &str = "ServiceCategory";
    pub const CHARGE_CATEGORY: &str = "ChargeCategory";
    pub const RESOURCE_ID: &str = "ResourceId";
    pub const REGION_ID: &str = "RegionId";
    pub const TAGS: &str = "Tags";
}

/// AWS prefixes user-defined cost allocation tags in CUR exports. Strip it so
/// downstream rules match on the tag the user actually set.
const AWS_USER_TAG_PREFIX: &str = "resourceTags/user:";

fn field<'a>(row: &'a serde_json::Value, name: &'static str) -> Result<&'a str, NormalizeError> {
    row.get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or(NormalizeError::MissingColumn(name))
}

fn optional_field<'a>(row: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    row.get(name)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn parse_decimal(value: &str, column: &'static str) -> Result<Decimal, NormalizeError> {
    value
        .trim()
        .parse::<Decimal>()
        .map_err(|_| NormalizeError::UnparseableValue {
            column,
            value: value.to_owned(),
        })
}

fn parse_timestamp(value: &str, column: &'static str) -> Result<DateTime<Utc>, NormalizeError> {
    DateTime::parse_from_rfc3339(value.trim())
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| NormalizeError::UnparseableValue {
            column,
            value: value.to_owned(),
        })
}

fn parse_currency(code: Option<&str>, fallback: Currency) -> Currency {
    match code.map(str::trim).map(str::to_ascii_uppercase).as_deref() {
        Some("USD") => Currency::Usd,
        Some("EUR") => Currency::Eur,
        Some("GBP") => Currency::Gbp,
        Some("JPY") => Currency::Jpy,
        Some("CHF") => Currency::Chf,
        Some("CAD") => Currency::Cad,
        Some("AUD") => Currency::Aud,
        Some("SEK") => Currency::Sek,
        Some("NOK") => Currency::Nok,
        Some("DKK") => Currency::Dkk,
        Some("SGD") => Currency::Sgd,
        Some("INR") => Currency::Inr,
        _ => fallback,
    }
}

fn parse_charge_category(value: &str) -> Result<ChargeCategory, NormalizeError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "usage" => Ok(ChargeCategory::Usage),
        "purchase" => Ok(ChargeCategory::Purchase),
        "tax" => Ok(ChargeCategory::Tax),
        "credit" => Ok(ChargeCategory::Credit),
        "adjustment" => Ok(ChargeCategory::Adjustment),
        _ => Err(NormalizeError::UnparseableValue {
            column: col::CHARGE_CATEGORY,
            value: value.to_owned(),
        }),
    }
}

/// Collects tags from both shapes AWS emits: a `Tags` column holding a JSON
/// object, and flattened `resourceTags/user:<key>` columns.
fn collect_tags(row: &serde_json::Value) -> serde_json::Value {
    let mut out = serde_json::Map::new();

    if let Some(raw) = row.get(col::TAGS) {
        match raw {
            serde_json::Value::Object(map) => {
                out.extend(map.clone());
            }
            serde_json::Value::String(s) if !s.trim().is_empty() => {
                if let Ok(serde_json::Value::Object(map)) =
                    serde_json::from_str::<serde_json::Value>(s)
                {
                    out.extend(map);
                }
            }
            _ => {}
        }
    }

    if let Some(map) = row.as_object() {
        for (k, v) in map {
            if let Some(stripped) = k.strip_prefix(AWS_USER_TAG_PREFIX)
                && v.as_str().is_some_and(|s| !s.trim().is_empty())
            {
                out.insert(stripped.to_owned(), v.clone());
            }
        }
    }

    serde_json::Value::Object(out)
}

/// Stable identity for a FOCUS row.
///
/// FOCUS has no single mandated line-item identifier, so identity is derived
/// from the tuple that makes a line unique within a billing period. Deriving it
/// rather than trusting a provider-supplied surrogate keeps re-delivery
/// idempotent even when AWS regenerates an export from scratch.
fn derive_external_id(row: &serde_json::Value, billing_period: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(billing_period.as_bytes());
    for name in [
        col::BILLING_ACCOUNT_ID,
        col::CHARGE_PERIOD_START,
        col::CHARGE_PERIOD_END,
        col::SERVICE_NAME,
        col::CHARGE_CATEGORY,
        col::RESOURCE_ID,
        col::REGION_ID,
    ] {
        hasher.update(b"\x1f");
        hasher.update(optional_field(row, name).unwrap_or_default().as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Hash of the row's content, used to tell a restatement from a duplicate.
fn content_hash(row: &serde_json::Value) -> String {
    // Serialize through a BTreeMap-backed value so key order is stable.
    let canonical = serde_json::to_string(row).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[async_trait::async_trait]
impl<S: FocusExportSource> crate::connector::CostConnector for AwsFocusConnector<S> {
    fn provider(&self) -> ProviderKind {
        ProviderKind::Aws
    }

    async fn fetch_since(&self, cursor: Cursor) -> Result<Vec<RawCostRecord>, ConnectorError> {
        let exports = self.source.list_exports(&cursor).await?;
        let mut records = Vec::new();

        for export in exports {
            let bytes = self.source.read_export(&export).await?;
            let mut reader = csv::Reader::from_reader(bytes.as_slice());
            let headers =
                reader
                    .headers()
                    .cloned()
                    .map_err(|e| ConnectorError::MalformedExport {
                        export_ref: export.key.clone(),
                        detail: e.to_string(),
                    })?;

            for result in reader.records() {
                let record = result.map_err(|e| ConnectorError::MalformedExport {
                    export_ref: export.key.clone(),
                    detail: e.to_string(),
                })?;
                let mut row = serde_json::Map::new();
                for (header, value) in headers.iter().zip(record.iter()) {
                    row.insert(
                        header.to_owned(),
                        serde_json::Value::String(value.to_owned()),
                    );
                }
                records.push(RawCostRecord {
                    provider: ProviderKind::Aws,
                    export_ref: export.key.clone(),
                    billing_period: export.billing_period.clone(),
                    row: serde_json::Value::Object(row),
                });
            }
        }

        Ok(records)
    }

    fn normalize(&self, raw: RawCostRecord) -> Result<CostEvent, NormalizeError> {
        let row = &raw.row;

        let currency = parse_currency(
            optional_field(row, col::BILLING_CURRENCY),
            self.default_currency,
        );
        let money = |name: &'static str| -> Result<Money, NormalizeError> {
            Ok(Money::new(
                parse_decimal(field(row, name)?, name)?,
                currency,
            ))
        };

        let tags = collect_tags(row);
        let dims = attribute(&tags, &self.rules, &self.ctx)?;

        let source_ref = SourceRef {
            provider: ProviderKind::Aws,
            external_id: derive_external_id(row, &raw.billing_period),
            export_ref: raw.export_ref.clone(),
            billing_period: raw.billing_period.clone(),
            content_hash: content_hash(row),
            ingested_at: Utc::now(),
        };

        CostEvent::new(
            field(row, col::BILLING_ACCOUNT_ID)?.to_owned(),
            parse_timestamp(
                field(row, col::CHARGE_PERIOD_START)?,
                col::CHARGE_PERIOD_START,
            )?,
            parse_timestamp(field(row, col::CHARGE_PERIOD_END)?, col::CHARGE_PERIOD_END)?,
            money(col::BILLED_COST)?,
            money(col::EFFECTIVE_COST)?,
            money(col::LIST_COST)?,
            field(row, col::SERVICE_NAME)?.to_owned(),
            ServiceCategory::from_focus_value(
                optional_field(row, col::SERVICE_CATEGORY).unwrap_or("Other"),
            ),
            parse_charge_category(field(row, col::CHARGE_CATEGORY)?)?,
            optional_field(row, col::RESOURCE_ID).map(str::to_owned),
            optional_field(row, col::REGION_ID).map(str::to_owned),
            tags,
            dims,
            source_ref,
        )
        .map_err(NormalizeError::from)
    }
}
