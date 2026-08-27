//! # api
//!
//! Axum HTTP layer. Together with `worker`, one of only two crates permitted to
//! wire concrete `infra` implementations to the traits the domain crates define
//!.
//!
//! ## The property this layer is responsible for
//!
//! **A request's tenant comes from a verified credential, never from the
//! request.** Row-level security scopes every query to `falkr.tenant_id`, so a
//! tenant id read from a header would convert RLS from a security boundary into
//! an instruction the caller writes. See [`auth`].
//!
//! Exposed as a library as well as a binary so integration tests can drive
//! [`routes::router`] directly, without a socket.

pub mod auth;
pub mod error;
pub mod routes;
pub mod state;

pub use error::ApiError;
pub use routes::router;
pub use state::AppState;
