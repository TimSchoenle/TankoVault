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
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $token:literal ),+ $(,)?
        }
        default = $default:ident,
        sql_type = $sql_type:literal
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
        #[serde(rename_all = "snake_case")]
        // `sqlx` on maps this to its native Postgres enum so `query!`/`query_as!` verify it at
        // compile time; off (the WASM frontend) keeps this crate free of the native sqlx stack.
        #[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
        #[cfg_attr(feature = "sqlx", sqlx(type_name = $sql_type))]
        $vis enum $name {
            $(
                $(#[$vmeta])*
                #[serde(rename = $token)]
                // utoipa needs telling separately: it does not read the `serde(rename)` above,
                // so without this it publishes the container's `rename_all` applied to the
                // *variant identifier* — a different string whenever the token is not that
                // identifier's snake_case (`MangaThemesia` → `manga_themesia`, while the
                // column, the wire and this enum all say `mangathemesia`). A client generated
                // from the document then cannot read the value the server actually sends.
                #[schema(rename = $token)]
                #[cfg_attr(feature = "sqlx", sqlx(rename = $token))]
                $variant
            ),+
        }

        impl $name {
            /// The token this variant serializes to, which is also its SQL enum label.
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
    /// The enum that refused the value, as its Rust type name.
    pub kind: &'static str,
    /// The string that matched no token, kept verbatim for the message.
    pub value: String,
}

str_enum! {
    /// The medium/origin classification of a work.
    pub enum ContentType {
        /// Japanese origin.
        Manga => "manga",
        /// Korean origin.
        Manhwa => "manhwa",
        /// Chinese origin.
        Manhua => "manhua",
        /// Vertical-scroll format, whatever its origin.
        Webtoon => "webtoon",
        /// No provider carrying the series has stated one.
        Unknown => "unknown",
    }
    default = Unknown,
    sql_type = "content_type"
}

str_enum! {
    /// Publication status of a canonical series.
    pub enum SeriesStatus {
        /// Still releasing.
        Ongoing => "ongoing",
        /// Finished, and no further chapter is expected.
        Completed => "completed",
        /// Paused by the author, with an ending still intended.
        Hiatus => "hiatus",
        /// Abandoned before an ending.
        Cancelled => "cancelled",
        /// No provider carrying the series has stated one.
        Unknown => "unknown",
    }
    default = Unknown,
    sql_type = "series_status"
}

str_enum! {
    /// Which adapter implementation drives a provider.
    ///
    /// `Madara`, `MangaThemesia`, `Manganato` and `Keyoapp` are *families*: each names a shared
    /// site theme or hosting platform whose default selector set ships in this workspace, so a
    /// site running one onboards as a single config row carrying only its deviations. `Custom`
    /// is the escape hatch, dispatched by slug.
    pub enum AdapterKind {
        /// The Madara `WordPress` theme.
        Madara => "madara",
        /// The `MangaThemesia` `WordPress` theme.
        MangaThemesia => "mangathemesia",
        /// The `Manganato` family of sites.
        Manganato => "manganato",
        /// The Keyoapp hosting platform.
        Keyoapp => "keyoapp",
        /// A site driven entirely by the selectors in `providers.config`, with no code of its
        /// own. What a new provider starts as.
        GenericConfig => "generic_config",
        /// A hand-written adapter, dispatched by the provider slug.
        Custom => "custom",
    }
    default = GenericConfig,
    sql_type = "adapter_kind"
}

str_enum! {
    /// Whether a provider gates a chapter behind payment or a timed early-access window.
    ///
    /// Only ever set from what the provider itself advertises. The distinction earns its own
    /// column because an early-access chapter is real and *will* become readable: counting it as
    /// unread by default tells a reader they are behind on something they cannot open, and
    /// dropping it at ingest loses the row that has to exist when the timer expires.
    pub enum ChapterAccess {
        /// Readable by anyone, which is what a provider stating nothing is taken to mean.
        Free => "free",
        /// Behind payment or a timer. The row exists so it can flip to `Free` when the window
        /// closes.
        EarlyAccess => "early_access",
    }
    default = Free,
    sql_type = "chapter_access"
}

str_enum! {
    /// Live health state of a provider (drives the circuit breaker + console tiles).
    pub enum ProviderState {
        /// Serving normally.
        Active => "active",
        /// Erroring often enough for the breaker to have noticed, but still answering.
        Degraded => "degraded",
        /// An interstitial was classified on the last fetch, so a solver tier is needed.
        Challenged => "challenged",
        /// A solve is in flight for this provider.
        Solving => "solving",
        /// The breaker opened. Nothing is dispatched until it closes.
        Blocked => "blocked",
        /// Turned off by an operator, which no health signal reverses.
        Disabled => "disabled",
    }
    default = Active,
    sql_type = "provider_state"
}

str_enum! {
    /// Scan cadence.
    pub enum ScanMode {
        /// Walks the provider archive and rebuilds everything it carries.
        Full => "full",
        /// Reads only the latest feed. The cadence that runs on a schedule.
        Fast => "fast",
    }
    default = Fast,
    sql_type = "scan_mode"
}

str_enum! {
    /// Lifecycle of a scan run.
    pub enum RunState {
        /// Dispatched, with no task claimed yet.
        Queued => "queued",
        /// At least one task has been claimed and the run has not settled.
        Running => "running",
        /// Every task settled, and not all of them failed.
        Completed => "completed",
        /// The run settled with nothing to show for it.
        Failed => "failed",
        /// Stopped by an operator. Queued tasks under it become unclaimable.
        Cancelled => "cancelled",
    }
    default = Queued,
    sql_type = "run_state"
}

str_enum! {
    /// Lifecycle of an individual scan task.
    pub enum TaskState {
        /// Waiting for a worker.
        Queued => "queued",
        /// A worker holds it. The claim is what increments `attempts`.
        Claimed => "claimed",
        /// The worker has begun fetching.
        Running => "running",
        /// Finished, and the rows it found are written.
        Done => "done",
        /// Gave up, with the reason on the row.
        Failed => "failed",
        /// Deliberately not run: its target was unchanged, or its run was cancelled first.
        Skipped => "skipped",
    }
    default = Queued,
    sql_type = "task_state"
}

str_enum! {
    /// A user's tracking status for a series.
    pub enum WatchStatus {
        /// Currently following. What an entry starts as.
        Reading => "reading",
        /// Saved for later, not started.
        Planned => "planned",
        /// Read to the end the user cares about.
        Completed => "completed",
        /// Abandoned, and not coming back.
        Dropped => "dropped",
        /// Set down with an intention to resume.
        Paused => "paused",
    }
    default = Reading,
    sql_type = "watch_status"
}

str_enum! {
    /// Whether an account may authenticate.
    ///
    /// Suspension is deliberately *not* a permission: permissions answer "what may this
    /// principal do", and a suspended account is one that may not act at all, including on
    /// its own data. Modelling it as the absence of every permission would still let the
    /// account read its watchlist and refresh its session, which is not what suspension
    /// means. It is therefore an identity-level state checked before authorization runs.
    pub enum AccountStatus {
        /// May sign in and act.
        Active => "active",
        /// May not act at all, its own data included. Checked before authorization runs.
        Suspended => "suspended",
    }
    default = Active,
    sql_type = "account_status"
}

impl AccountStatus {
    /// Whether an account in this state may sign in and hold a session.
    #[must_use]
    pub fn may_authenticate(self) -> bool {
        matches!(self, Self::Active)
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

    /// Every enum publishes exactly the vocabulary it serializes.
    ///
    /// The defect this pins: the schema derive ignores the per-variant `#[serde(rename)]` and
    /// fell back to the container's `rename_all`, so `AdapterKind::MangaThemesia` was published
    /// as `manga_themesia` while the database, the API and this enum all said `mangathemesia`.
    /// Nothing compared the two vocabularies, so the drift only surfaced as a client generated
    /// from the document refusing every response that carried such a provider — the console's
    /// provider and challenge screens both failed with "the server sent something this app
    /// couldn't read".
    #[test]
    fn the_published_vocabulary_is_the_one_serde_writes() {
        let mut covered: Vec<&str> = Vec::new();
        macro_rules! assert_published {
            ($($ty:ty),+ $(,)?) => {$({
                let schema = serde_json::to_value(<$ty as utoipa::PartialSchema>::schema())
                    .expect("a schema serializes");
                let published: Vec<&str> = schema["enum"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{} publishes a vocabulary", stringify!($ty)))
                    .iter()
                    .map(|token| token.as_str().expect("tokens are strings"))
                    .collect();
                let written: Vec<&str> = <$ty>::all().iter().map(|v| v.as_str()).collect();
                assert_eq!(
                    published, written,
                    "{} publishes tokens it does not serialize", stringify!($ty)
                );
                covered.push(stringify!($ty));
            })+};
        }
        assert_published!(
            ContentType,
            SeriesStatus,
            AdapterKind,
            ChapterAccess,
            ProviderState,
            ScanMode,
            RunState,
            TaskState,
            WatchStatus,
            AccountStatus,
        );

        // The list above is hand-written, so it is checked against the file rather than
        // trusted: a `str_enum!` nobody adds here would go unchecked, and going unchecked is
        // the whole failure mode.
        let declared: Vec<&str> = include_str!("enums.rs")
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("pub enum "))
            .filter_map(|rest| rest.split_whitespace().next())
            .collect();
        assert_eq!(
            declared, covered,
            "a `str_enum!` is missing from this test (declaration order)"
        );
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
    fn only_active_accounts_may_authenticate() {
        assert!(AccountStatus::Active.may_authenticate());
        assert!(!AccountStatus::Suspended.may_authenticate());
        // A fresh account must be usable, so the default cannot be the locked-out state.
        assert!(AccountStatus::default().may_authenticate());
    }
}
