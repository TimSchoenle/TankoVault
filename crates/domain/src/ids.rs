//! Strongly-typed UUID v7 identifiers.
//!
//! Every aggregate gets its own newtype so a `SeriesId` can never be passed where
//! a `ProviderId` is expected. All ids are UUID v7 — time-sortable, index-friendly
//! primary keys generated in-app (see the schema note in `docs/design.md` §6).

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Mint a fresh, time-sortable identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wrap an existing UUID (e.g. one read back from the database).
            #[must_use]
            pub const fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            /// The inner UUID.
            #[must_use]
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

id_type!(
    /// Identifies a [`crate::entities::Provider`].
    ProviderId
);
id_type!(
    /// Identifies a canonical [`crate::entities::Series`].
    SeriesId
);
id_type!(
    /// Identifies a [`crate::entities::SeriesSource`].
    SeriesSourceId
);
id_type!(
    /// Identifies a [`crate::entities::Chapter`].
    ChapterId
);
id_type!(
    /// Identifies a user account.
    UserId
);
id_type!(
    /// Identifies a tag.
    TagId
);
id_type!(
    /// Identifies a scan run.
    ScanRunId
);
id_type!(
    /// Identifies a scan task.
    ScanTaskId
);
id_type!(
    /// Identifies a notification.
    NotificationId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_sortable_by_creation_time() {
        let a = SeriesId::new();
        let b = SeriesId::new();
        // UUID v7 embeds a millisecond timestamp; `b` is minted no earlier than `a`.
        assert!(b >= a);
    }

    #[test]
    fn serde_is_transparent() {
        let id = UserId::new();
        let json = serde_json::to_string(&id).unwrap();
        // Serialized as a bare UUID string, not a wrapper object.
        assert!(json.starts_with('"') && json.ends_with('"'));
        let back: UserId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }
}
