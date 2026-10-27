//! Rules over `.github/workflows`: the Postgres major the tests run, advisory ignores,
//! concurrency groups, the OIDC token the release signs with, and the order release-please tags
//! in.

mod advisories;
mod build_epoch;
mod concurrency;
mod oidc;
mod postgres;
mod registry;
mod release_please;
mod sentry;
mod yaml;

pub(super) use advisories::*;
pub(super) use build_epoch::*;
pub(super) use concurrency::*;
pub(super) use oidc::*;
pub(super) use postgres::*;
pub(super) use registry::*;
pub(super) use release_please::*;
pub(super) use sentry::*;

/// The workflow that mints the OIDC token and signs with it.
const PUBLISH_WORKFLOW: &str = ".github/workflows/release-please.yaml";
