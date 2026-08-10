# inkstone-ui

A Dioxus 0.7 component kit with no application in it.

`dioxus` is its only dependency, and the renderer is not chosen here — the crate compiles
unchanged for a WASM SPA and for a desktop webview.

## What is in it

| Module     | Components                                                              |
| ---------- | ----------------------------------------------------------------------- |
| `button`   | `Button`, `ToggleButton`, `IconButton`, `button_class`                   |
| `field`    | `TextInput`, `TextArea`, `SelectField`, `Checkbox`, `SegControl`, `SliderRow`, `SearchField`, `Field`, `FieldShell` |
| `pill`     | `Pill`, `Chip`, `StatusDot`                                              |
| `layout`   | `Row`, `Stack`, `Card`, `Tile`, `Section`, `Kv`, `KvRow`, `Brush`        |
| `table`    | `Table`, `TableColumn`                                                   |
| `feedback` | `Skeleton`, `SkeletonRows`, `SkeletonGrid`, `EmptyBox`, `ErrorBox`, `ErrorLine`, `OutcomeLine`, `async_view`, `async_list` |
| `overlay`  | `Modal`                                                                  |
| `tabs`     | `TabBar`, `TabItem`                                                      |

Two enums cut across all of them: `Tone` (what a control *means* — `Primary`, `Danger`,
`Positive`, …) and `Size` (`Xs`, `Sm`, `Md`).

## Using it in another project

1. Copy `crates/inkstone-ui/` into the consumer and add it as a path dependency.
2. Serve `styles/inkstone.css`, or `@import` it from whatever stylesheet the app already builds.
   If that build scans source files for class names (Tailwind does), point the scanner at this
   crate's `src/**/*.rs` as well — the markup lives here, not in the consumer.
3. Define the colour custom properties listed at the head of `styles/inkstone.css`. Everything
   else — shape, density, type, fill — already has a default and is overridable; see
   *Restyling* below.

Nothing else is required. There is no provider to mount, no context to install, no global state.

## Restyling: three axes

**No component in this crate contains a class name.** Each one names a `Part` and the `Variant`s
that apply to it; a `Skin` decides what that renders as. `skin::tests::no_component_names_a_class`
keeps it that way — the abstraction is worth nothing if one element keeps a literal, because
that element silently ignores the skin and a consumer's design system comes out
ninety-five per cent applied with no way to see which five per cent is missing.

### Axis 3: the vocabulary

```rust
struct Bootstrap;
impl Skin for Bootstrap {
    fn part(&self, part: &Part) -> &str {
        match part { Part::Button => "btn", _ => "" }
    }
    fn modifier(&self, part: &Part, variant: &Variant) -> &str {
        match (part, variant) {
            (Part::Button, Variant::Tone(Tone::Danger)) => "btn-danger",
            _ => "",
        }
    }
}

rsx! { SkinProvider { skin: SkinHandle::new(Bootstrap), {app} } }
```

A prefix parameter would not have reached this: Bootstrap repeats the base in every modifier,
and a utility framework has no base at all. `part` + `modifier` + `join` covers both without
either being a special case, and `join` is overridable for a vocabulary that is not
space-separated.

`Part` is the kit's whole surface and is deliberately closed, so a skin that handles every
variant is provably complete. It is `#[non_exhaustive]`, so adding a component is not a breaking
change for a skin with a `_` arm — but a skin that matches exhaustively will be told.

A skin is resolved at render from context, so it is a build-time choice; there is nothing to
subscribe to. Runtime restyling is axes 1 and 2 below, which need no re-render at all.

## Axes 1 and 2: theme and style, both custom properties

Nothing visual is hard-coded in Rust, and after the token pass nothing is hard-coded in the
rules either. There are two independent axes, and they compose:

**Theme** — the palette. `--acc`, `--jade`, `--star`, `--surface`, `--border`, the text ramp.
Swapping these is a new colour scheme; the app this came from flips them per `[data-theme]` and
`[data-accent]` at runtime.

**Style** — shape, density, type and fill. The `--ik-*` block at the head of
`styles/inkstone.css`: radii, paddings, font sizes and weights, border width, the focus ring,
and a tint scale that decides how *filled* a tone-tinted control is. Every rule reads these;
none of them names a literal.

So a whole new style is an override block on a selector you choose:

```css
[data-style="solid"] {
  --ik-radius-btn: 3px; --ik-radius-pill: 3px; --ik-radius-input: 3px;
  --ik-tint-btn: 100%; --ik-tint-pill: 100%; --ik-edge: 100%;
  --ik-pad-btn: 14px 22px; --ik-weight-btn: 700; --ik-border: 2px;
}
```

That is the entire change: square corners, solid fills, chunky controls, heavier rules — no rule
edited, no component touched, and the reader's theme and accent still apply on top. Because
custom properties inherit, the same block scoped to a container restyles one region rather than
the page.

`tone::tests::every_token_has_a_default` holds every `var(--ik-*)` a rule reads to a default in
that block, so the kit still renders correctly on a host that overrides none of them — and a
typo'd token, which would otherwise silently drop a control's padding, fails the build instead.

### Adding a tone or a size

`Tone::Custom("brand")` and `Size::Custom("jumbo")` render `ik-btn brand` / `ik-btn jumbo`
against rules in *your* stylesheet, so the vocabulary is open without forking the kit. The
guarantee does not follow you across that line: `every_variant_has_a_rule` can only read
`styles/inkstone.css`. Mirror it over your own tokens — it is a dozen lines, and it is the
difference between a variant that works and one that silently renders as the base.

## The rules the kit encodes

**A variant can only name a class that exists.** The kit was extracted after `ik-btn danger`,
`ik-btn ghost`, `ik-btn vermilion` and `ik-pill ghost` were all found at call sites with no rule
behind any of them — five buttons, one of them a queue drain, rendering as ordinary neutral
controls. The built-in `Tone`s and `Size`s resolve to a closed set, and
`tone::tests::every_variant_has_a_rule` reads `styles/inkstone.css` to prove each one is
defined — including that no tone collapses onto the bare base class without a stated reason,
which is how `Positive` and `Caution` buttons were briefly indistinguishable from neutral ones.

**Controls are controlled.** Every input takes its value and returns edits; none holds state.
The consumer's state usually lives in a URL or a shared signal, and a control keeping a second
copy is how the two drift apart.

**Accessibility is not a prop you can forget.** `IconButton` requires a `label`, `Table`
requires a `caption`, `TabBar` implements roving tabindex and arrow-key navigation, `Modal`
takes focus and closes on `Escape`, and a field's error replaces its hint in `aria-describedby`
rather than being appended after it.

**Text is already translated.** No message catalogue, no formatter. Every user-visible string
arrives as a `String` the caller resolved, which is why the kit has no opinion about i18n.

**Glyphs are `Element`s.** The kit ships no icons, so `icon:` takes rendered markup.

## Layout primitives instead of inline styles

`Row` and `Stack` exist to replace `style: "display:flex;gap:8px;align-items:baseline;"`. An
inline style is invisible to the stylesheet, to the theme, and to any audit that greps for a
class. `Gap`, `Align` and `Justify` are closed scales, so spacing cannot drift into arbitrary
pixel values.

## Constraints it was built under

The app this came from serves a Content-Security-Policy with no `'unsafe-eval'` and bans
`document::eval`, so the kit contains no JavaScript, no `dangerous_inner_html` and no DOM
portal — `Modal` renders inline under a scrim with its own stacking context rather than being
moved to the end of `<body>`.
