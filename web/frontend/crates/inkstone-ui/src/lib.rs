//! Inkstone — a Dioxus component kit with no application in it.
//!
//! Every component here renders the `ik-*` class contract defined by `styles/inkstone.css` and
//! nothing else: no i18n runtime, no icon set, no HTTP client, no router. Anything an app has an
//! opinion about arrives as a prop — text as `String`, glyphs as `Element`, actions as
//! `EventHandler`. That is what makes the directory liftable into another Dioxus project
//! alongside `styles/inkstone.css`.
//!
//! # Why typed variants rather than class strings
//!
//! The kit exists because `class: "ik-btn danger"` compiles, renders, and is wrong: no such rule
//! was ever written, so the destructive button looked exactly like the neutral one beside it.
//! Four modifiers were dead this way when the kit was extracted. [`Tone`] and [`Size`] can only
//! name classes the stylesheet defines, so that failure mode is gone rather than documented.

mod button;
mod feedback;
mod field;
mod layout;
mod overlay;
mod pill;
pub mod skin;
mod table;
mod tabs;
mod tone;

pub use button::{button_class, Button, ButtonType, IconButton, ToggleButton};
pub use feedback::{
    async_list, async_view, EmptyBox, ErrorBox, ErrorLine, Outcome, OutcomeLine, Skeleton,
    SkeletonGrid, SkeletonRows,
};
pub use field::{
    Checkbox, Field, FieldShell, SearchField, SegControl, SegOption, SelectField, SliderRow,
    TextArea, TextInput,
};
pub use layout::{Brush, Card, Kv, KvRow, Row, Section, Stack, Tile};
pub use overlay::{Modal, ModalSize};
pub use pill::{Chip, Pill, StatusDot};
pub use skin::{use_skin, Flag, Inkstone, Part, Skin, SkinHandle, SkinProvider, Variant};
pub use table::{Table, TableColumn};
pub use tabs::{TabBar, TabItem};
pub use tone::{Align, Gap, Justify, Size, Tone};
