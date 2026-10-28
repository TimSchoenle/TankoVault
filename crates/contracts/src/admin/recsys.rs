//! Recommendation index builds.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Which kind of recommendation-model build to run.
///
/// Both sides of the internal hop parse it: the API validates the operator's request body and the
/// control plane acts on it. One shared type keeps the two from drifting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecsysBuildMode {
    /// Patch the live generation: the repair queue, then whatever it has not reached.
    Incremental,
    /// Re-solve the projection basis and re-embed the catalogue. A `next_full_build` tuning
    /// change takes effect only after one.
    Full,
}

/// What one recommendation-model build request did.
///
/// Returned by `POST /v1/admin/recommendations/rebuild`, produced by the control plane that runs
/// the build.
///
/// The build runs detached, so this says only whether one started: a build takes minutes to
/// hours, and no request may be held open that long. Its stage, progress, counts and outcome are
/// on `GET /v1/admin/recommendations/health`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct RecsysBuildView {
    /// `false` when another build already held the claim. Wait for it instead of retrying: it is
    /// doing this one's work, and `generation` is zero because this call started nothing.
    pub started: bool,
    /// The generation the build claimed and will write under.
    pub generation: i32,
}
