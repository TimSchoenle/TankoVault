//! Wire DTOs generated at compile time from `wire_schema.json` (a plain JSON Schema
//! derived from the api service's `utoipa` component schemas — see `xtask openapi` and
//! `services/api/src/openapi.rs`). This is the single source of truth for every
//! request/response shape shared with the backend; regenerate the schema file and rebuild
//! to pick up backend changes instead of hand-editing types here.
//!
//! Presentation-only helpers (`label()`, `token()`, …) and frontend-only types that aren't
//! 1:1 JSON bodies live in `crate::models`, not here.

typify::import_types!(
    schema = "wire_schema.json",
    derives = [PartialEq],
    patch = {
        // Small C-like enums: used with by-value `Copy` semantics throughout the views
        // (iterated in `ALL`/`COLUMNS` arrays, stored in signals, matched by value).
        ContentType = { derives = [Copy, Eq] },
        SeriesStatus = { derives = [Copy, Eq] },
        WatchStatus = { derives = [Copy, Eq] },
        RunState = { derives = [Copy, Eq] },
        ScanMode = { derives = [Copy, Eq] },
        // Typed ids: `uuid::Uuid`-backed newtypes, used by-value in route params, signal
        // state, and as map keys throughout the views.
        SeriesId = { derives = [Copy, Eq, Hash] },
        ChapterId = { derives = [Copy, Eq, Hash] },
        ProviderId = { derives = [Copy, Eq, Hash] },
        ScanRunId = { derives = [Copy, Eq, Hash] },
        ScanTaskId = { derives = [Copy, Eq, Hash] },
        SeriesSourceId = { derives = [Copy, Eq, Hash] },
        TagId = { derives = [Copy, Eq, Hash] },
        UserId = { derives = [Copy, Eq, Hash] },
        AuthorId = { derives = [Copy, Eq, Hash] },
        NotificationId = { derives = [Copy, Eq, Hash] },
    },
);
