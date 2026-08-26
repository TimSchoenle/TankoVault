//! The class vocabulary, behind a trait.
//!
//! No component in this crate contains a class name. Each one names a [`Part`] and the
//! [`Variant`]s that apply to it, and a [`Skin`] decides what that renders as. [`Inkstone`] is
//! the default and produces the `ik-*` classes `styles/inkstone.css` defines; a consumer that
//! installs its own reaches an entirely different design system without forking a component.
//!
//! # Why a trait rather than a prefix
//!
//! A prefix only reaches vocabularies shaped like this one. The interesting targets are not:
//! Bootstrap repeats the base in every modifier, and a utility framework has no base at all —
//! its "button" is a dozen atoms and its tone is two more. Both fall out of [`Skin::part`] +
//! [`Skin::modifier`] + [`Skin::join`] without either being a special case.
//!
//! Do not write literal utility class names in these comments as examples. Tailwind's scanner
//! reads this file — it is under an `@source` glob, which is what keeps the `ik-*` names alive —
//! and it cannot tell a comment from markup, so an illustrative `bg-…` emits a real rule into
//! the shipped stylesheet.

use crate::tone::{Align, Gap, Justify, Size, Tone};
use dioxus::prelude::*;
use std::sync::Arc;

/// An element the kit renders, named by what it is rather than by what it is called.
///
/// The set is closed on purpose: it is the kit's whole surface, so a [`Skin`] that handles every
/// variant is provably complete. Adding a component here means adding a `Part`, which makes
/// every existing skin fail to compile until it has an answer — that is the point.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[non_exhaustive]
pub enum Part {
    // Controls
    Button,
    IconButton,
    Input,
    Select,
    Range,
    Checkbox,
    Field,
    FieldLabel,
    FieldHint,
    FieldError,
    Seg,
    SegItem,
    SliderRow,
    SliderLabel,
    SliderValue,
    SearchField,
    SearchIcon,
    SearchInput,
    SearchHits,
    // Status
    Pill,
    Chip,
    StatusDot,
    // Layout
    Row,
    Stack,
    Tile,
    Panel,
    PanelHead,
    PanelHeadEnd,
    Section,
    SectionHead,
    SectionHeadEnd,
    SectionLabel,
    DefinitionList,
    DefinitionKey,
    Divider,
    // Data
    TableWrap,
    Table,
    NumericCell,
    VisuallyHidden,
    // Feedback
    Skeleton,
    SkeletonRow,
    SkeletonGrid,
    SkeletonCard,
    SkeletonCover,
    SkeletonCardBody,
    Empty,
    Error,
    ErrorLine,
    Outcome,
    // Overlay
    ModalScrim,
    Modal,
    ModalHead,
    ModalIntro,
    ModalBody,
    ModalFoot,
    // Navigation
    Tabs,
    TabsScroll,
    Tab,
    TabCount,
    // Typography
    Mono,
}

/// A boolean state a part can be in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[non_exhaustive]
pub enum Flag {
    /// Fills its container.
    Block,
    /// Wraps onto more lines.
    Wrap,
    /// Takes the free space in a flex line.
    Grow,
    /// The lit one of a set.
    Active,
    /// The chosen one of a set — [`Part::SegItem`]'s form of [`Flag::Active`].
    Selected,
    /// Against an inspector's edge rather than a page's.
    Flush,
    /// Denser than the default.
    Compact,
    /// Bounded rather than growing, and scrolled in place.
    Scroll,
    /// At the trailing edge, set off from the rest.
    Apart,
    /// A narrower key column.
    Narrow,
    /// Rendered in the monospace face.
    Mono,
    /// Wider than the default.
    Wide,
    /// Cautionary rather than neutral.
    Warn,
    /// Reports success.
    Ok,
    /// Reports failure.
    Err,
}

/// One axis of variation on a [`Part`].
///
/// [`Variant::None`] exists so a caller can build a fixed-length list with conditional entries
/// and not allocate: see [`Variant::flag`].
#[derive(Clone, Copy, PartialEq, Debug)]
#[non_exhaustive]
pub enum Variant {
    None,
    Tone(Tone),
    Size(Size),
    Gap(Gap),
    Align(Align),
    Justify(Justify),
    Flag(Flag),
}

impl Variant {
    /// `flag` when `on`, nothing otherwise.
    #[must_use]
    pub fn flag(on: bool, flag: Flag) -> Self {
        if on {
            Self::Flag(flag)
        } else {
            Self::None
        }
    }
}

/// Decides what every part and variant is called.
///
/// Implement it to put the kit's components on a different design system. The two required
/// methods are lookups; [`Skin::join`] has a default that is right for space-separated class
/// vocabularies, which is most of them.
pub trait Skin: Send + Sync + 'static {
    /// The base class for a part. `""` for a part this skin draws with nothing of its own.
    fn part(&self, part: &Part) -> &str;

    /// The class a variant adds to a part. `""` when the variant needs none — a default size,
    /// or a tone the base class already expresses.
    fn modifier(&self, part: &Part, variant: &Variant) -> &str;

    /// Assemble a base and its modifiers. Space-separated, empties dropped, base first.
    fn join(&self, base: &str, modifiers: &[&str]) -> String {
        let width = base.len() + modifiers.iter().map(|m| m.len() + 1).sum::<usize>();
        let mut out = String::with_capacity(width);
        out.push_str(base);
        for modifier in modifiers {
            if modifier.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(modifier);
        }
        out
    }
}

/// The kit's own vocabulary: the `ik-*` classes `styles/inkstone.css` defines.
#[derive(Clone, Copy, Default, Debug)]
pub struct Inkstone;

impl Skin for Inkstone {
    fn part(&self, part: &Part) -> &str {
        match part {
            Part::Button => "ik-btn",
            // Composed, not standalone: an icon button is a button that is square.
            Part::IconButton => "ik-btn ik-iconbtn",
            Part::Input => "ik-input",
            Part::Select => "ik-select",
            Part::Range => "ik-range",
            Part::Checkbox => "ik-check",
            Part::Field => "ik-field",
            Part::FieldLabel | Part::DefinitionKey | Part::SliderLabel => "k",
            Part::FieldHint => "ik-field-hint",
            Part::FieldError => "ik-field-error",
            Part::Seg => "ik-seg",
            // Two parts this skin draws with nothing of their own: the stylesheet styles
            // `.ik-seg button` by element, so a segment needs no class until it is selected;
            // and a section is a bare wrapper whose head carries all the styling.
            Part::SegItem | Part::Section => "",
            Part::SliderRow => "ik-slider-row",
            Part::SliderValue => "v",
            Part::SearchField => "ik-searchfield",
            Part::SearchIcon => "ik-searchfield-icon",
            Part::SearchInput => "ik-searchfield-input",
            Part::SearchHits => "ik-searchfield-hits ik-mono",
            Part::Pill => "ik-pill",
            Part::Chip => "ik-chip",
            Part::StatusDot => "ik-status-dot",
            Part::Row => "ik-flex",
            Part::Stack => "ik-stack",
            Part::Tile => "ik-tile",
            Part::Panel => "ik-panel",
            Part::PanelHead => "ik-card-head",
            Part::PanelHeadEnd => "ik-card-head-end",
            Part::SectionHead => "ik-sec-head",
            Part::SectionHeadEnd => "ik-sec-head-end",
            Part::SectionLabel => "ik-sec-lbl",
            Part::DefinitionList => "ik-kv",
            Part::Divider => "ik-brush",
            Part::TableWrap => "ik-tablewrap",
            Part::Table => "ik-table",
            Part::NumericCell => "ik-num",
            Part::VisuallyHidden => "ik-visually-hidden",
            Part::Skeleton => "ik-skeleton",
            Part::SkeletonRow => "ik-row",
            Part::SkeletonGrid => "ik-grid",
            Part::SkeletonCard => "ik-card",
            Part::SkeletonCover => "ik-skeleton ik-skel-cover",
            Part::SkeletonCardBody => "ik-card-body",
            Part::Empty => "ik-empty",
            Part::Error => "ik-error",
            Part::ErrorLine => "ik-error-line",
            Part::Outcome => "ik-outcome",
            Part::ModalScrim => "ik-modal-scrim",
            Part::Modal => "ik-modal",
            Part::ModalHead => "ik-modal-head",
            Part::ModalIntro => "ik-modal-intro",
            Part::ModalBody => "ik-modal-body",
            Part::ModalFoot => "ik-modal-foot",
            Part::Tabs => "ik-tabs",
            Part::TabsScroll => "ik-tabs-scroll",
            Part::Tab => "ik-tab",
            Part::TabCount => "ik-tab-count",
            Part::Mono => "ik-mono",
        }
    }

    fn modifier(&self, part: &Part, variant: &Variant) -> &str {
        match variant {
            Variant::None => "",
            Variant::Tone(tone) => Self::tone(*part, *tone),
            Variant::Size(size) => Self::size(*part, *size),
            Variant::Gap(gap) => match gap {
                Gap::None => "ik-g-0",
                Gap::Xs => "ik-g-xs",
                Gap::Sm => "ik-g-sm",
                Gap::Md => "",
                Gap::Lg => "ik-g-lg",
            },
            Variant::Align(align) => match align {
                Align::Start => "ik-a-start",
                Align::Center => "",
                Align::Baseline => "ik-a-base",
                Align::End => "ik-a-end",
                Align::Stretch => "ik-a-stretch",
            },
            Variant::Justify(justify) => match justify {
                Justify::Start => "",
                Justify::Center => "ik-j-center",
                Justify::End => "ik-j-end",
                Justify::Between => "ik-j-between",
            },
            Variant::Flag(flag) => Self::flag(*part, *flag),
        }
    }
}

impl Inkstone {
    /// The tone modifier. Split per part because the same meaning is drawn differently on a
    /// button and on a pill, and on most parts is not drawn at all.
    fn tone(part: Part, tone: Tone) -> &'static str {
        match part {
            Part::Button | Part::IconButton => match tone {
                Tone::Neutral => "",
                Tone::Primary => "primary",
                Tone::Accent => "acc",
                Tone::Danger => "danger",
                Tone::Positive => "jade",
                Tone::Caution => "amber",
                Tone::Ghost => "ghost",
                Tone::Bare => "bare",
                Tone::Custom(modifier) => modifier,
            },
            Part::Pill | Part::StatusDot => match tone {
                Tone::Neutral | Tone::Bare => "",
                Tone::Primary | Tone::Accent | Tone::Danger => "acc",
                Tone::Positive => "jade",
                Tone::Caution => "amber",
                Tone::Ghost => "ghost",
                Tone::Custom(modifier) => modifier,
            },
            // A custom tone is the caller's own class and is honoured on any part; the built-in
            // tones are not drawn outside the two families above.
            _ => match tone {
                Tone::Custom(modifier) => modifier,
                _ => "",
            },
        }
    }

    fn size(part: Part, size: Size) -> &'static str {
        match part {
            Part::Button | Part::IconButton => match size {
                Size::Xs => "xs",
                Size::Sm => "sm",
                Size::Md => "",
                Size::Custom(modifier) => modifier,
            },
            _ => match size {
                Size::Custom(modifier) => modifier,
                _ => "",
            },
        }
    }

    fn flag(part: Part, flag: Flag) -> &'static str {
        match (part, flag) {
            (Part::Button | Part::IconButton, Flag::Block) => "block",
            (Part::Row, Flag::Wrap) => "ik-wrap",
            (Part::Row | Part::Stack, Flag::Grow) => "ik-grow",
            // The range input grows inside its slider row, which is a different rule from the
            // layout primitives' — `.ik-range.grow` sets `width: auto` as well as `flex: 1`.
            (Part::Range, Flag::Grow) => "grow",
            (Part::Chip | Part::Tab, Flag::Active) => "active",
            (Part::Chip, Flag::Warn) => "warn",
            (Part::SegItem, Flag::Selected) => "on",
            (Part::Tabs, Flag::Flush) => "flush",
            (Part::Tab, Flag::Apart) => "apart",
            (Part::Table, Flag::Compact) => "ik-table-compact",
            (Part::TableWrap, Flag::Scroll) => "scroll",
            (Part::DefinitionList, Flag::Narrow) => "narrow",
            (Part::Input | Part::Select, Flag::Mono) => "ik-mono",
            (Part::Modal, Flag::Wide) => "wide",
            (Part::Outcome, Flag::Ok) => "ok",
            (Part::Outcome, Flag::Err) => "err",
            _ => "",
        }
    }
}

/// A cheap, cloneable handle to the skin in force.
///
/// Defaults to [`Inkstone`], so a consumer that installs nothing still gets a working kit.
#[derive(Clone)]
pub struct SkinHandle(Arc<dyn Skin>);

impl SkinHandle {
    /// Wrap a skin for [`SkinProvider`].
    #[must_use]
    pub fn new(skin: impl Skin) -> Self {
        Self(Arc::new(skin))
    }

    /// The class for a part and its variants.
    #[must_use]
    pub fn class(&self, part: Part, variants: &[Variant]) -> String {
        self.class_with(part, variants, "")
    }

    /// [`SkinHandle::class`] plus a caller's own classes, appended last so they can override.
    #[must_use]
    pub fn class_with(&self, part: Part, variants: &[Variant], extra: &str) -> String {
        let skin = self.0.as_ref();
        let mut modifiers: Vec<&str> = variants
            .iter()
            .map(|variant| skin.modifier(&part, variant))
            .collect();
        modifiers.push(extra);
        skin.join(skin.part(&part), &modifiers)
    }
}

impl Default for SkinHandle {
    fn default() -> Self {
        Self(Arc::new(Inkstone))
    }
}

impl PartialEq for SkinHandle {
    /// By identity: two handles are the same skin when they point at the same one. A `Skin` is
    /// a lookup table with no state, so there is nothing else to compare, and this is only here
    /// because Dioxus props require it.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for SkinHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SkinHandle")
    }
}

/// The skin in force for this subtree, or [`Inkstone`] if none was installed.
///
/// Deliberately not a hook — it reads context on every render rather than caching in a slot, so
/// it is safe to call inside a branch. A skin swap therefore takes effect wherever the subtree
/// re-renders; nothing here subscribes to it, because a skin is a build-time choice. Runtime
/// restyling is the `--ik-*` custom properties, which need no re-render at all.
#[must_use]
pub fn use_skin() -> SkinHandle {
    try_consume_context::<SkinHandle>().unwrap_or_default()
}

/// Install a skin for everything below it.
#[component]
pub fn SkinProvider(skin: SkinHandle, children: Element) -> Element {
    use_context_provider(|| skin);
    rsx! {
        {children}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_compose_after_the_base() {
        let skin = SkinHandle::default();
        assert_eq!(
            skin.class(
                Part::Button,
                &[
                    Variant::Tone(Tone::Danger),
                    Variant::Size(Size::Xs),
                    Variant::flag(true, Flag::Block),
                ]
            ),
            "ik-btn danger xs block"
        );
    }

    #[test]
    fn absent_variants_and_flags_add_nothing() {
        let skin = SkinHandle::default();
        assert_eq!(
            skin.class(
                Part::Button,
                &[
                    Variant::Tone(Tone::Neutral),
                    Variant::Size(Size::Md),
                    Variant::flag(false, Flag::Block),
                ]
            ),
            "ik-btn"
        );
    }

    #[test]
    fn a_caller_s_own_classes_come_last() {
        let skin = SkinHandle::default();
        assert_eq!(
            skin.class_with(Part::Pill, &[Variant::Tone(Tone::Positive)], "star"),
            "ik-pill jade star"
        );
    }

    /// A part whose base class is empty must not open with a stray space, or the class attribute
    /// starts with one and every equality assertion in a consumer's tests fails for no reason.
    #[test]
    fn an_empty_base_does_not_leak_whitespace() {
        let skin = SkinHandle::default();
        assert_eq!(
            skin.class(Part::SegItem, &[Variant::flag(true, Flag::Selected)]),
            "on"
        );
        assert_eq!(skin.class(Part::SegItem, &[]), "");
    }

    /// The escape hatch has to reach parts the built-in tones do not draw, or a consumer's own
    /// vocabulary stops at buttons and pills.
    #[test]
    fn a_custom_tone_is_honoured_on_any_part() {
        let skin = SkinHandle::default();
        assert_eq!(
            skin.class(Part::Tile, &[Variant::Tone(Tone::Custom("brand"))]),
            "ik-tile brand"
        );
        assert_eq!(
            skin.class(Part::Tile, &[Variant::Tone(Tone::Danger)]),
            "ik-tile"
        );
    }

    /// Only this module may name a class.
    ///
    /// The whole abstraction is worth nothing if a component keeps one literal: that element
    /// silently ignores the skin, so a consumer's design system comes out ninety-five per cent
    /// applied and nobody can see which five per cent is missing. `class:` must always be a
    /// value the skin produced.
    #[test]
    fn no_component_names_a_class() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("the crate must have a src directory") {
            let path = entry.expect("readable directory entry").path();
            if path.file_name().is_some_and(|name| name == "skin.rs")
                || path.extension().is_none_or(|ext| ext != "rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("readable source file");
            for (number, line) in text.lines().enumerate() {
                // Doc comments quote class names to explain the contract; only real markup
                // counts.
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                if code.contains("class: \"") {
                    offenders.push(format!(
                        "{}:{}",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        number + 1
                    ));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "class literals outside `skin.rs` at {offenders:?} — name a `Part` instead"
        );
    }

    /// A skin with a different shape entirely — the case a prefix parameter could not have
    /// served. Bootstrap repeats the base in every modifier and would otherwise need each
    /// component rewritten.
    #[test]
    fn a_foreign_vocabulary_needs_no_component_changes() {
        struct Bootstrap;
        impl Skin for Bootstrap {
            fn part(&self, part: &Part) -> &str {
                match part {
                    Part::Button => "btn",
                    Part::Input => "form-control",
                    _ => "",
                }
            }

            fn modifier(&self, part: &Part, variant: &Variant) -> &str {
                match (part, variant) {
                    (Part::Button, Variant::Tone(Tone::Danger)) => "btn-danger",
                    (Part::Button, Variant::Tone(Tone::Primary)) => "btn-primary",
                    (Part::Button, Variant::Size(Size::Sm)) => "btn-sm",
                    _ => "",
                }
            }
        }

        let skin = SkinHandle::new(Bootstrap);
        assert_eq!(
            skin.class(
                Part::Button,
                &[Variant::Tone(Tone::Danger), Variant::Size(Size::Sm)]
            ),
            "btn btn-danger btn-sm"
        );
    }
}
