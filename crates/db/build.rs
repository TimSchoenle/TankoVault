//! Rebuild whenever `migrations/` changes.
//!
//! `sqlx::migrate!` reads the directory at compile time but registers no cargo dependency on it,
//! so adding a migration changes no input cargo tracks and `MIGRATOR` keeps the previous set.
//! Nothing fails at that point: `migrate` reports "migrations applied" having applied nothing,
//! and the first query against the missing table is where it surfaces — in a different service,
//! hours later. This is the dependency the macro does not declare.

fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
