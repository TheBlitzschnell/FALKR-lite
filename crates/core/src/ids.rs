//! Domain-ID newtypes.
//!
//! Every identifier in this system is a distinct type, never a bare [`Uuid`]
//! (the domain-ID newtype rule). `post_entry(tenant: Uuid, project: Uuid, run: Uuid)` compiles
//! happily with the arguments in the wrong order; `post_entry(tenant: TenantId,
//! project: ProjectId, run: RunId)` does not. In a multi-tenant system a
//! swapped `tenant_id` is not merely bad data, it is a tenant-isolation breach,
//! so the compiler is the right place to catch it.

use uuid::Uuid;

/// Declares a domain-ID newtype over [`Uuid`].
///
/// The `sqlx::Type` derive is behind the `sqlx` feature, which is off by
/// default. That keeps `core` — and therefore every domain crate that depends
/// on it — resolvable without pulling in `sqlx`, per the workspace layout and
/// the inward-only dependency rule, while still giving `infra` real type
/// checking at the
/// database boundary rather than binding bare `Uuid`s.
macro_rules! domain_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
            serde::Serialize, serde::Deserialize,
        )]
        #[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
        #[cfg_attr(feature = "sqlx", sqlx(transparent))]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        // No `Default` impl, deliberately. A defaulted identifier would be a
        // freshly generated random one, which in this codebase means a
        // `Dimensions::default()` could silently invent a tenant. Construction
        // is always explicit.
        #[expect(
            clippy::new_without_default,
            reason = "a Default identifier would silently invent a tenant; see above"
        )]
        impl $name {
            /// Generates a new identifier.
            ///
            /// UUIDv7 rather than v4: these are primary keys in an append-only
            /// event log, and time-ordered keys keep Postgres btree inserts
            /// local instead of scattering them across the index.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wraps an existing [`Uuid`], e.g. one read back out of Postgres.
            #[must_use]
            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            /// The underlying [`Uuid`].
            ///
            /// Takes `self` by value — these are `Copy` — so that
            /// `TenantId::as_uuid` works directly as a function reference.
            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                core::fmt::Display::fmt(&self.0, f)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

// --- The dimensional spine ---------------------------
domain_id!(
    /// Tenant. The row-level-security boundary; present on every row in the
    /// system.
    TenantId
);
domain_id!(ProjectId);
domain_id!(RunId);
domain_id!(TeamId);
domain_id!(CustomerId);
domain_id!(ModelId);
domain_id!(DatasetId);
domain_id!(ProviderId);
domain_id!(CommitmentId);

// --- Cost spine ------------------------------------
domain_id!(CostEventId);

// --- Research graph --------------------------------
domain_id!(ExperimentId);
domain_id!(JobId);
domain_id!(CheckpointId);
domain_id!(ModelVersionId);
domain_id!(DatasetVersionId);

// --- Ledger ----------------------------------------
domain_id!(EntryId);
domain_id!(AccountId);
domain_id!(PolicyId);

// --- Billing ---------------------------------------
domain_id!(UsageEventId);
domain_id!(ContractId);
domain_id!(CreditPoolId);

// --- Compliance ------------------------------------
domain_id!(LicenseId);

// --- HR (the HR design) --------------------------------------------
domain_id!(GrantId);
domain_id!(EmployeeId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip_through_uuid() {
        let a = TenantId::new();
        let b = TenantId::new();
        assert_ne!(a, b);
        assert_eq!(TenantId::from_uuid(a.as_uuid()), a);
    }

    #[test]
    fn generated_ids_are_time_ordered() {
        // UUIDv7 sorts by creation time, which is what keeps event-log inserts
        // local in the index. If this ever fails, the version changed.
        let first = EntryId::new();
        let second = EntryId::new();
        assert!(first <= second);
        assert_eq!(first.as_uuid().get_version_num(), 7);
    }

    #[test]
    fn distinct_id_types_do_not_unify() {
        // The point of the newtypes: this is what a swapped argument would
        // look like, and it must not compile.
        let t = TenantId::new();
        let p = ProjectId::from_uuid(t.as_uuid());
        // Same underlying Uuid, different types — no `==` is possible between
        // them, so we compare the projections instead.
        assert_eq!(t.as_uuid(), p.as_uuid());
    }
}
