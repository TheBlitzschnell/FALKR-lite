//! Rebuild when a migration changes.
//!
//! `sqlx::migrate!` embeds the migration directory at **compile time**. Without
//! this, adding or editing a `.sql` file does not invalidate the build, so
//! `cargo test` happily runs the previous schema — and a test suite that
//! verifies schema properties (`tenancy_pg.rs` checks that every table carries
//! row-level security) would pass against a schema that no longer exists.
//!
//! That failure is silent and points the wrong way: the tests go green while
//! the thing they protect has regressed. CI is unaffected because it builds
//! clean, which makes it precisely the kind of bug that only ever bites locally.

fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
