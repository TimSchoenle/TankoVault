//! Rebuilds the crate whenever `migrations/` changes.
//!
//! `sqlx::migrate!` reads the directory at compile time but registers no cargo dependency on it.
//! Without this, a new migration leaves `MIGRATOR` on the previous set: `migrate` reports
//! success having applied nothing, and the first query against the missing table fails in
//! another service.

fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
