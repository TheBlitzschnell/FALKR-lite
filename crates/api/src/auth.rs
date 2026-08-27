//! Tenant identity for incoming requests.
//!
//! # The rule this module exists to enforce
//!
//! **A request's tenant is never read from anything the client controls.** Not
//! a header, not a query parameter, not a field in the body. It is derived
//! server-side from a credential the server verified.
//!
//! This matters more here than in most systems. Row-level security scopes every
//! query to `falkr.tenant_id`, and [`crate::state::AppState`] sets that GUC from
//! whatever [`TenantContext`] the extractor produced. A tenant id taken from an
//! `X-Tenant-Id` header would therefore turn RLS from a security boundary into
//! an instruction the caller writes — the precise failure the multi-tenancy design
//! is built to prevent, arriving through the front door instead of the pooler.
//!
//! There is a test asserting a client-supplied tenant header is ignored.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use falkr_core::TenantId;

use crate::error::ApiError;

/// What a verified credential establishes about the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantContext {
    pub tenant_id: TenantId,
    pub scopes: Vec<Scope>,
}

impl TenantContext {
    #[must_use]
    pub fn has(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope)
    }

    /// Rejects the request unless the credential carries `scope`.
    pub fn require(&self, scope: Scope) -> Result<(), ApiError> {
        if self.has(scope) {
            Ok(())
        } else {
            Err(ApiError::Forbidden {
                missing_scope: scope,
            })
        }
    }
}

/// Coarse capabilities. Deliberately few — a permission model grows to fit
/// whatever it is asked to express, and starting broad is easier to narrow than
/// the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Scope {
    /// Read financial and operational data.
    Read,
    /// Ingest usage and cost events.
    Ingest,
    /// Post to the ledger.
    Post,
}

impl core::fmt::Display for Scope {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::Read => "read",
            Self::Ingest => "ingest",
            Self::Post => "post",
        };
        f.write_str(s)
    }
}

/// Verifies a bearer credential and resolves it to a tenant.
///
/// A trait rather than a concrete implementation because the credential format
/// is deployment-specific — an opaque API key today, an OIDC-issued JWT
/// elsewhere. What must not vary is that the tenant comes from *here*, having
/// been verified, rather than from the request.
#[async_trait::async_trait]
pub trait TenantResolver: Send + Sync {
    /// Returns the context a valid token establishes, or `None` if the token is
    /// unknown, expired, or malformed.
    ///
    /// Deliberately returns `None` rather than an error variant per failure
    /// mode: distinguishing "no such token" from "expired token" in a response
    /// tells an attacker which guesses were close.
    async fn resolve(&self, token: &str) -> Option<TenantContext>;
}

/// Resolver backed by an in-memory table of issued API keys.
///
/// Real, not a stub: opaque bearer keys mapped server-side to a tenant is the
/// standard shape for a B2B API, and it satisfies the rule above. A JWT-backed
/// resolver is a second implementation of the same trait, not a change here.
///
/// Tokens are compared in constant time — a naive `==` on a secret leaks its
/// prefix through timing, which is cheap to avoid and awkward to retrofit.
#[derive(Debug, Default)]
pub struct StaticTokenResolver {
    tokens: Vec<(String, TenantContext)>,
}

impl StaticTokenResolver {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_token(
        mut self,
        token: impl Into<String>,
        tenant_id: TenantId,
        scopes: Vec<Scope>,
    ) -> Self {
        self.tokens
            .push((token.into(), TenantContext { tenant_id, scopes }));
        self
    }
}

/// Constant-time comparison, so a mismatch reveals nothing through timing.
fn secret_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0_u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[async_trait::async_trait]
impl TenantResolver for StaticTokenResolver {
    async fn resolve(&self, token: &str) -> Option<TenantContext> {
        self.tokens
            .iter()
            .find(|(candidate, _)| secret_eq(candidate, token))
            .map(|(_, ctx)| ctx.clone())
    }
}

/// Extracts the `Authorization: Bearer <token>` value.
fn bearer_token(parts: &Parts) -> Option<&str> {
    parts
        .headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

impl<S> FromRequestParts<S> for TenantContext
where
    S: Send + Sync,
    crate::state::AppState: axum::extract::FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        use axum::extract::FromRef as _;
        let app = crate::state::AppState::from_ref(state);

        let token = bearer_token(parts).ok_or(ApiError::Unauthenticated)?;
        app.tenants
            .resolve(token)
            .await
            .ok_or(ApiError::Unauthenticated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_comparison_rejects_near_misses_and_length_differences() {
        assert!(secret_eq("abc123", "abc123"));
        assert!(!secret_eq("abc123", "abc124"));
        assert!(!secret_eq("abc123", "abc1234"));
        assert!(!secret_eq("", "a"));
    }

    #[tokio::test]
    async fn an_unknown_token_resolves_to_nothing() {
        let tenant = TenantId::new();
        let resolver = StaticTokenResolver::new().with_token("good", tenant, vec![Scope::Read]);
        assert!(resolver.resolve("good").await.is_some());
        assert!(resolver.resolve("bad").await.is_none());
        assert!(resolver.resolve("").await.is_none());
    }

    #[tokio::test]
    async fn scopes_gate_actions() {
        let ctx = TenantContext {
            tenant_id: TenantId::new(),
            scopes: vec![Scope::Read],
        };
        assert!(ctx.require(Scope::Read).is_ok());
        assert!(ctx.require(Scope::Post).is_err());
    }
}
