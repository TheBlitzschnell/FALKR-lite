//! Tracker connectors.
//!
//! [`wandb`] is the real one — W&B is the more common choice in AI labs, and
//! its run object is the richer of the two, so building against it first
//! exercises more of the mapping. [`mlflow`] is a deliberate mock until a
//! later phase; it exists so the trait has two implementations and cannot
//! accidentally grow W&B-shaped assumptions.

pub mod mlflow;
pub mod wandb;
