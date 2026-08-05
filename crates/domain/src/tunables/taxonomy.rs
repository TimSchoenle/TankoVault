//! How a tunable is classified: which console group it belongs to, what kind of number it
//! is, and when a change to it actually takes effect.

use serde::{Deserialize, Serialize};
use std::fmt;
use utoipa::ToSchema;

/// The console's grouping of tunables into sections.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TunableGroup {
    Affinity,
    Retrieval,
    Scoring,
    Diversity,
    Prior,
    Build,
    Cooccurrence,
    Serving,
}

impl TunableGroup {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Affinity => "affinity",
            Self::Retrieval => "retrieval",
            Self::Scoring => "scoring",
            Self::Diversity => "diversity",
            Self::Prior => "prior",
            Self::Build => "build",
            Self::Cooccurrence => "cooccurrence",
            Self::Serving => "serving",
        }
    }
}

impl fmt::Display for TunableGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a tunable's number *means*, so the console can render and validate it sensibly.
///
/// Purely presentational: every value is an `f64` on the wire and in the table regardless.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TunableKind {
    /// A fraction, normally `0…1`.
    Ratio,
    /// A blend coefficient whose scale is only meaningful against the other weights.
    Weight,
    /// A whole number of things; readers round it.
    Count,
    Days,
    Seconds,
}

impl TunableKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ratio => "ratio",
            Self::Weight => "weight",
            Self::Count => "count",
            Self::Days => "days",
            Self::Seconds => "seconds",
        }
    }
}

impl fmt::Display for TunableKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// When a change to a tunable actually reaches a reader.
///
/// The console shows this on every row, because it is the most likely way this surface fails a
/// user: someone raises a value baked into stored model data, sees no change, raises it again,
/// and concludes the page is broken — when the truth is that the old value is what is
/// physically stored.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Applies {
    /// The next request, once each replica's snapshot refreshes.
    Immediately,
    /// The next incremental build.
    NextBuild,
    /// The next full rebuild — the stored model was computed under the old value.
    NextFullBuild,
}

impl Applies {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Immediately => "immediately",
            Self::NextBuild => "next_build",
            Self::NextFullBuild => "next_full_build",
        }
    }
}

impl fmt::Display for Applies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
