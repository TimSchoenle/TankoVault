//! The concrete external trackers this service can sync with.
//!
//! Each provider is one directory implementing [`crate::provider::ExternalProvider`], and
//! nothing outside that directory may see its private vocabulary — statuses cross the boundary
//! as `WatchStatus`, entries as `RemoteEntry`, metadata as `RemoteMetadata`. Adding a second
//! provider is a sibling directory plus one registry entry in `main`, which is the whole point
//! of the trait (design: generalized multi-provider sync).

pub(crate) mod anilist;
