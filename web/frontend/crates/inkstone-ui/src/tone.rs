//! The axes every control in the kit varies on.
//!
//! These are *meanings*, not appearances: nothing here knows a class name or a colour. What a
//! tone renders as is [`crate::skin`]'s business, which is what lets one component tree serve
//! more than one design system.

/// What a control means.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tone {
    /// The default: no claim on attention.
    #[default]
    Neutral,
    /// The one action a screen exists for. At most one per view.
    Primary,
    /// Secondary, but still wants to be found.
    Accent,
    /// Destructive and irreversible: delete, purge, revoke, drain.
    Danger,
    /// A confirmation or healthy state.
    Positive,
    /// A caution that is not yet a failure.
    Caution,
    /// Chrome-free but still a hit target.
    Ghost,
    /// Text that happens to be clickable.
    Bare,
    /// A tone the active skin defines and this crate does not, named by the class it renders.
    ///
    /// The escape hatch that keeps the set open: add `.ik-btn.brand` to your own stylesheet and
    /// pass `Tone::Custom("brand")` without forking the kit.
    ///
    /// **The guarantee stops here.** `every_variant_has_a_rule` proves the built-in tones
    /// against `styles/inkstone.css`, the only stylesheet this crate can read. A custom token's
    /// rule lives in yours, so a typo is exactly the dead modifier this kit exists to prevent —
    /// mirror that test over your own tokens.
    Custom(&'static str),
}

impl Tone {
    /// Every tone the kit defines itself, in no particular order. [`Tone::Custom`] is not one,
    /// by construction — which is what makes this the exact set the kit's own stylesheet is
    /// answerable for. Public so a consumer can enumerate them, typically to render a gallery
    /// of a new skin against every tone it has to cover.
    pub const BUILT_IN: &'static [Self] = &[
        Self::Neutral,
        Self::Primary,
        Self::Accent,
        Self::Danger,
        Self::Positive,
        Self::Caution,
        Self::Ghost,
        Self::Bare,
    ];
}

/// How large a control is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Size {
    /// Dense: table rows, card overlays, toolbars.
    Xs,
    /// Compact: inspectors, sidebars, panel footers.
    Sm,
    /// The default body-sized control.
    #[default]
    Md,
    /// A size the active skin defines and this crate does not. See [`Tone::Custom`].
    Custom(&'static str),
}

impl Size {
    /// Every size the kit defines itself. See [`Tone::BUILT_IN`].
    pub const BUILT_IN: &'static [Self] = &[Self::Xs, Self::Sm, Self::Md];
}

/// The spacing scale. A closed set, so spacing cannot drift into arbitrary pixel values.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Gap {
    None,
    /// Glyph to its label.
    Xs,
    /// Controls in a group.
    Sm,
    /// The default — unrelated items on one line.
    #[default]
    Md,
    /// Separate regions.
    Lg,
}

/// Cross-axis alignment.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Align {
    Start,
    #[default]
    Center,
    /// Text of different sizes sharing one line.
    Baseline,
    End,
    Stretch,
}

/// Main-axis distribution.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    /// First item left, last item right — the toolbar shape.
    Between,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skin::{Inkstone, Part, Skin, Variant};

    /// Every tone and size the default skin can render names a class the stylesheet defines, or
    /// is listed below as a deliberate collapse onto the base class.
    ///
    /// The bug this pins: `ik-btn danger`, `ik-btn ghost`, `ik-btn vermilion` and `ik-pill
    /// ghost` were all written at call sites with no rule behind any of them, so five buttons —
    /// one of them a queue drain — drew as ordinary neutral controls.
    ///
    /// An earlier version skipped every empty modifier, and so passed while `Tone::Positive` and
    /// `Tone::Caution` on a *button* resolved to `""`: a confirm read exactly like a cancel,
    /// which is the same failure one size smaller. Do not reintroduce a blanket skip. A tone
    /// that genuinely has no modifier belongs in `BASE_IS_THE_TONE` with the reason.
    #[test]
    fn every_variant_has_a_rule() {
        /// `(part, tone)` pairs where the unmodified base class *is* the tone, with why.
        const BASE_IS_THE_TONE: &[(Part, Tone, &str)] = &[
            (
                Part::Button,
                Tone::Neutral,
                "the bordered surface button is the neutral one",
            ),
            (
                Part::Pill,
                Tone::Neutral,
                "the bordered pill is the neutral one",
            ),
            (
                Part::Pill,
                Tone::Bare,
                "a pill is already chrome-light; `Bare` distinguishes a button from its border, \
                 and a pill has nothing further to give up",
            ),
        ];

        let css = include_str!("../styles/inkstone.css");
        let skin = Inkstone;
        for part in [Part::Button, Part::Pill] {
            let base = skin.part(&part);
            for &tone in Tone::BUILT_IN {
                let modifier = skin.modifier(&part, &Variant::Tone(tone));
                assert!(
                    !modifier.is_empty()
                        || BASE_IS_THE_TONE
                            .iter()
                            .any(|(entry, allowed, _)| *entry == part && *allowed == tone),
                    "{tone:?} renders a bare `{base}`, so it is indistinguishable from \
                     `Tone::Neutral`. Add a rule, or list the pair in BASE_IS_THE_TONE with the \
                     reason that is intended"
                );
                assert!(
                    modifier.is_empty() || css.contains(&format!(".{base}.{modifier}")),
                    "{tone:?} renders `{base} {modifier}`, which no rule defines"
                );
            }
            for &size in Size::BUILT_IN {
                let modifier = skin.modifier(&part, &Variant::Size(size));
                assert!(
                    modifier.is_empty() || css.contains(&format!(".{base}.{modifier}")),
                    "{size:?} on {part:?} renders a class no rule defines"
                );
            }
        }
    }

    /// Every `--ik-*` a rule reads is defined in the stylesheet's own `:root` block.
    ///
    /// A `var(--ik-typo)` with no default and no host override resolves to nothing, and the
    /// declaration is dropped: a button silently loses its padding, or its border, with no error
    /// anywhere. The defaults exist so the kit renders correctly on a host that sets none of
    /// them, which is exactly the case no consumer will think to test.
    #[test]
    fn every_token_has_a_default() {
        let css = include_str!("../styles/inkstone.css");
        let root = css
            .split_once(":root {")
            .and_then(|(_, rest)| rest.split_once('}'))
            .expect("the stylesheet must declare its token defaults in a `:root` block")
            .0;

        // Comments are stripped first: a declaration that follows one shares its `;`-delimited
        // chunk, and naive splitting drops every token that happens to open a group.
        let mut plain = String::with_capacity(root.len());
        let mut rest = root;
        while let Some((before, after)) = rest.split_once("/*") {
            plain.push_str(before);
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
        }
        plain.push_str(rest);

        let defined: Vec<&str> = plain
            .split(';')
            .filter_map(|declaration| declaration.split_once(':'))
            .map(|(name, _)| name.trim())
            .filter(|name| name.starts_with("--ik-"))
            .collect();
        assert!(defined.len() > 20, "the token block looks truncated");

        for (index, _) in css.match_indices("var(--ik-") {
            let rest = &css[index + "var(".len()..];
            let name = &rest[..rest.find(')').expect("unclosed var()")];
            assert!(
                defined.contains(&name),
                "`{name}` is read by a rule but has no default in the `:root` block"
            );
        }
    }
}
