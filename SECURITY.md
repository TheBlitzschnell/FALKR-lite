# Security Policy

## Reporting a vulnerability

Please **do not** open a public issue for a security problem.

Report it privately through GitHub's
[security advisory](https://github.com/TheBlitzschnell/FALKR-lite/security/advisories/new)
form, which keeps the report confidential until a fix is available.

Please include what you did, what happened, and what you expected. A
proof-of-concept helps but isn't required.

## Scope

This project handles financial records for multiple tenants in a shared
database, so the areas most worth attention are:

- **Cross-tenant data access.** Isolation is enforced by PostgreSQL row-level
  security. Anything that reads or writes another tenant's rows is a serious
  finding, including through connection pooling, a missing `SET LOCAL`, or a
  table added without a policy.
- **Tenant identity.** A request's tenant is derived from a verified credential.
  Anything that lets a caller influence which tenant a request runs as is in
  scope.
- **Ledger integrity.** The event log is append-only, enforced by a database
  trigger. A path that mutates or deletes a posted event is in scope.
- **Information disclosure through errors.** Internal errors are logged in full
  and answered with a fixed message, because a storage error can carry query
  text or another tenant's data.

## Supported versions

This is early software with no release series yet. Fixes land on `main`.
