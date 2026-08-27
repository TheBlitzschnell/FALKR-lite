//! Provider connectors.
//!
//! One real connector, not five stubs. AWS is first because AWS Data Exports
//! (CUR 2.0) emits FOCUS natively, so [`aws`]'s `normalize` is a column mapping
//! rather than an invented translation layer — which is the entire reason FOCUS
//! is the internal shape here.
//!
//! Adding GCP, Azure or CoreWeave means implementing
//! [`crate::connector::CostConnector`] again; nothing outside this module needs
//! to change.

pub mod aws;
