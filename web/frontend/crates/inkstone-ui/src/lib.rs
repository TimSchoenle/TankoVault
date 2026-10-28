//! Owns Inkstone: a Dioxus component kit with no application in it.
//!
//! Every component here renders the `ik-*` class contract defined by `styles/inkstone.css` and
//! nothing else: no i18n runtime, no icon set, no HTTP client, no router. Anything an app has an
//! opinion about arrives as a prop: text as `String`, glyphs as `Element`, actions as
//! `EventHandler`. That is what makes the directory liftable into another Dioxus project
//! alongside `styles/inkstone.css`.
//!
//! # Why typed variants instead of class strings
//!
//! A class string compiles and renders whether or not the stylesheet defines its modifier: a
//! misspelt or missing `danger` rule draws the destructive button exactly like the neutral one
//! beside it. [`Tone`] and [`Size`] can only name classes the stylesheet defines, so that failure
//! mode is impossible, not documented.

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
