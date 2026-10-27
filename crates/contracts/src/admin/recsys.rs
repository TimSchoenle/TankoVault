//! Recommendation index builds.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Which kind of recommendation-model build to run.
///
/// Lives here rather than in either service because both sides of the internal hop parse it: the
/// API validates the operator's request body and the control plane acts on it. A hand-mirrored
/// second copy is the drift this crate exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecsysBuildMode {
    /// Patch the live generation: the repair queue, then whatever it has not reached.
    Incremental,
    /// Re-solve the projection basis and re-embed the catalogue. What a `next_full_build` tuning
    /// change needs before it means anything.
    Full,
}

/// What one recommendation-model build request did.
///
/// Returned by `POST /v1/admin/recommendations/rebuild`, produced by the control plane that
/// actually runs the build.
///
/// The build runs detached, so this answers only whether one was *started*: a build takes
/// minutes to hours and no request may be held open for it. What it went on to do — stage,
/// progress, counts, and how it ended — is on `GET /v1/admin/recommendations/health`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct RecsysBuildView {
    /// `false` when another build already held the claim. The correct response is to wait for
    /// it, not to retry: the other build is doing this one's work, and `generation` is zero
    /// because this call started nothing.
    pub started: bool,
    /// The generation the build claimed and will write under.
    pub generation: i32,
}
