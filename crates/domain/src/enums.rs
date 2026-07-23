//! Domain enumerations mirrored 1:1 with the `PostgreSQL` enum types (see `migrations`).
//!
//! Each enum serializes to the exact lowercase token used by the SQL `CREATE TYPE`
//! declarations so that `serde` payloads, API DTOs, and DB values stay identical.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use utoipa::ToSchema;

/// Generates a string-backed enum with lowercase serde tokens plus `as_str` /
/// `FromStr` implementations that agree byte-for-byte with the SQL enum.
macro_rules! str_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident { $( $variant:ident => $token:literal ),+ $(,)? }
        default = $default:ident
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
        #[serde(rename_all = "snake_case")]
        $vis enum $name {
            $( #[serde(rename = $token)] $variant ),+
        }

        impl $name {
            #[must_use]
            $vis fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $token ),+ }
            }

            /// Every variant, in declaration order.
            #[must_use]
            $vis fn all() -> &'static [Self] {
                &[ $( Self::$variant ),+ ]
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseEnumError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( $token => Ok(Self::$variant), )+
                    other => Err(ParseEnumError { kind: stringify!($name), value: other.to_owned() }),
                }
            }
        }

        impl Default for $name {
            fn default() -> Self { Self::$default }
        }
    };
}

/// Error raised when a string cannot be parsed into a domain enum.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid {kind} value: {value:?}")]
pub struct ParseEnumError {
    pub kind: &'static str,
    pub value: String,
}

str_enum! {
    /// The medium/origin classification of a work.
    pub enum ContentType {
        Manga => "manga",
        Manhwa => "manhwa",
        Manhua => "manhua",
        Webtoon => "webtoon",
        Unknown => "unknown",
    }
    default = Unknown
}

str_enum! {
    /// Publication status of a canonical series.
    pub enum SeriesStatus {
        Ongoing => "ongoing",
        Completed => "completed",
        Hiatus => "hiatus",
        Cancelled => "cancelled",
        Unknown => "unknown",
    }
    default = Unknown
}

str_enum! {
    /// Which adapter implementation drives a provider.
    pub enum AdapterKind {
        Madara => "madara",
        GenericConfig => "generic_config",
        Custom => "custom",
    }
    default = GenericConfig
}

str_enum! {
    /// Live health state of a provider (drives the circuit breaker + console tiles).
    pub enum ProviderState {
        Active => "active",
        Degraded => "degraded",
        Challenged => "challenged",
        Solving => "solving",
        Blocked => "blocked",
        Disabled => "disabled",
    }
    default = Active
}

str_enum! {
    /// Scan cadence.
    pub enum ScanMode {
        Full => "full",
        Fast => "fast",
    }
    default = Fast
}

str_enum! {
    /// Lifecycle of a scan run.
    pub enum RunState {
        Queued => "queued",
        Running => "running",
        Completed => "completed",
        Failed => "failed",
        Cancelled => "cancelled",
    }
    default = Queued
}

str_enum! {
    /// Lifecycle of an individual scan task.
    pub enum TaskState {
        Queued => "queued",
        Claimed => "claimed",
        Running => "running",
        Done => "done",
        Failed => "failed",
        Skipped => "skipped",
    }
    default = Queued
}

str_enum! {
    /// A user's tracking status for a series.
    pub enum WatchStatus {
        Reading => "reading",
        Planned => "planned",
        Completed => "completed",
        Dropped => "dropped",
        Paused => "paused",
    }
    default = Reading
}

str_enum! {
    /// RBAC role. Ordering matters for privilege comparison (see [`UserRole::at_least`]).
    pub enum UserRole {
        User => "user",
        Operator => "operator",
        Admin => "admin",
    }
    default = User
}

impl UserRole {
    /// Numeric privilege rank; higher is more privileged.
    #[must_use]
    fn rank(self) -> u8 {
        match self {
            Self::User => 0,
            Self::Operator => 1,
            Self::Admin => 2,
        }
    }

    /// True when `self` satisfies a requirement of `required` or higher.
    #[must_use]
    pub fn at_least(self, required: Self) -> bool {
        self.rank() >= required.rank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_via_str() {
        for &s in ProviderState::all() {
            assert_eq!(ProviderState::from_str(s.as_str()).unwrap(), s);
        }
    }

    #[test]
    fn serde_uses_sql_token() {
        let json = serde_json::to_string(&AdapterKind::GenericConfig).unwrap();
        assert_eq!(json, "\"generic_config\"");
    }

    #[test]
    fn rejects_unknown_token() {
        assert!(ScanMode::from_str("weekly").is_err());
    }

    #[test]
    fn role_privilege_ordering() {
        assert!(UserRole::Admin.at_least(UserRole::Operator));
        assert!(UserRole::Operator.at_least(UserRole::User));
        assert!(!UserRole::User.at_least(UserRole::Admin));
        assert!(UserRole::User.at_least(UserRole::User));
    }
}
