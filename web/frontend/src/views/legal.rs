//! `/legal/:slug` — one operator-published document, at the prose measure.
//!
//! # Why there is no HTML anywhere in this file
//!
//! The body is **operator input**, not developer input: it comes out of a file on a mounted
//! volume that whoever runs this instance edits. Rendering Markdown to an HTML string and
//! injecting it would need `dangerous_inner_html`, and would make the correctness of this page
//! depend on a sanitiser being right about every input.
//!
//! So `pulldown-cmark` is used as a pull parser and its events are mapped onto `rsx!` nodes
//! directly. There is no HTML string at any point, which means an operator's file has nothing to
//! inject *into*; a raw `<script>` in the source arrives as `Event::Html` and is rendered as the
//! literal text it is.

use crate::api;
use crate::components::{async_view, SkeletonBlock};
use crate::i18n::use_i18n;
use crate::icons::{Ic, Icon};
use crate::models::LegalDocumentView;
use crate::state::legal::legal_title;
use crate::title::PageTitle;
use crate::Route;
use dioxus::prelude::*;
use progenitor_client::ResponseValue;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[component]
pub(crate) fn Legal(slug: String) -> Element {
    let i18n = use_i18n();
    let api = api::use_api();
    let published = use_context::<PageTitle>();
    let language = i18n.language();

    let document = use_resource(use_reactive!(|(slug, language)| {
        let client = api.client();
        async move {
            client
                .legal_document()
                .slug(&slug)
                .lang(&language)
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    rsx! {
        {
            async_view(
                &document,
                use_reload_noop(),
                || rsx! { SkeletonBlock { height: 420 } },
                move |doc: &LegalDocumentView| {
                    // The document's own name is something the route cannot spell: it is the
                    // operator's title, in the locale the server chose.
                    published
                        .set(
                            Route::Legal { slug: doc.slug.clone() },
                            legal_title(i18n, &doc.slug, doc.title.as_deref()),
                        );
                    rsx! { Document { doc: doc.clone() } }
                },
            )
        }
    }
}

/// `async_view` takes a reload handle for its error state's retry. This page has no mutation to
/// invalidate, so it gets a fresh one rather than the shell's.
fn use_reload_noop() -> crate::hooks::Reload {
    crate::hooks::use_reload()
}

/// The rendered document: title, the last-updated and locale notes, then the prose.
#[component]
fn Document(doc: LegalDocumentView) -> Element {
    let i18n = use_i18n();
    let requested = i18n.language();
    rsx! {
        div { class: "ik-legal",
            div { class: "ik-flex", style: "gap:9px;margin-bottom:2px;",
                Ic { icon: Icon::Gavel, size: 18 }
                span { class: "ik-kicker", {i18n.t("footer.legal")} }
            }
            h1 { class: "ik-page-title", style: "margin-bottom:6px;",
                {legal_title(i18n, &doc.slug, doc.title.as_deref())}
            }
            div { class: "ik-legal-meta",
                if let Some(updated) = doc.updated.as_ref().filter(|u| !u.trim().is_empty()) {
                    span { {i18n.args("legal.updated", &[("date", updated)])} }
                }
                // Only when the two differ: saying "shown in English" on the English page is
                // noise, but a German reader looking at English text needs to know it is the
                // only version there is, not the operator's German.
                if doc.locale != requested {
                    span { class: "ik-legal-locale",
                        Ic { icon: Icon::Language, size: 13 }
                        {i18n.args("legal.localeNote", &[("locale", &doc.locale.to_uppercase())])}
                    }
                }
            }
            div { class: "ik-prose", {markdown(&doc.body)} }
        }
    }
}

/// Render Markdown as `rsx!` nodes.
///
/// A block-level assembly over `pulldown-cmark`'s event stream: inline runs are collected into a
/// `Vec<Inline>` and emitted when their block closes, because `rsx!` has nowhere to keep
/// open-element state between iterations of a loop.
///
/// Unsupported blocks are not dropped — a table an operator wrote must not silently vanish from
/// a policy — they degrade to their text content.
fn markdown(source: &str) -> Element {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);

    let mut blocks: Vec<Block> = Vec::new();
    let mut inline: Vec<Inline> = Vec::new();
    let mut style = Style::default();
    let mut link: Option<String> = None;
    let mut list_depth = 0usize;
    let mut item: Vec<Inline> = Vec::new();
    let mut items: Vec<Vec<Inline>> = Vec::new();
    let mut ordered = false;
    let mut quoting = false;

    for event in Parser::new_ext(source, options) {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                inline.clear();
                style = Style::default();
            }
            Event::End(TagEnd::Heading(level)) => {
                blocks.push(Block::Heading(level, std::mem::take(&mut inline)));
            }
            Event::Start(Tag::CodeBlock(_)) => inline.clear(),
            Event::End(TagEnd::CodeBlock) => {
                blocks.push(Block::Code(
                    std::mem::take(&mut inline)
                        .into_iter()
                        .map(|i| i.text)
                        .collect(),
                ));
            }
            Event::Start(Tag::BlockQuote(_)) => quoting = true,
            Event::End(TagEnd::BlockQuote(_)) => quoting = false,
            Event::Start(Tag::List(start)) => {
                ordered = start.is_some();
                list_depth += 1;
                items.clear();
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
                blocks.push(Block::List(ordered, std::mem::take(&mut items)));
            }
            Event::End(TagEnd::Item) => items.push(std::mem::take(&mut item)),
            Event::End(TagEnd::Paragraph) => {
                let run = std::mem::take(&mut inline);
                if list_depth > 0 {
                    item.extend(run);
                } else if quoting {
                    blocks.push(Block::Quote(run));
                } else {
                    blocks.push(Block::Paragraph(run));
                }
            }
            Event::Rule => blocks.push(Block::Rule),
            Event::Start(Tag::Emphasis) => style.italic = true,
            Event::End(TagEnd::Emphasis) => style.italic = false,
            Event::Start(Tag::Strong) => style.bold = true,
            Event::End(TagEnd::Strong) => style.bold = false,
            Event::Start(Tag::Link { dest_url, .. }) => link = Some(dest_url.to_string()),
            Event::End(TagEnd::Link) => link = None,
            Event::Code(text) => inline.push(Inline {
                text: text.to_string(),
                style: Style {
                    code: true,
                    ..style
                },
                href: link.clone(),
            }),
            // Raw HTML is text. This is the whole reason the events are mapped by hand.
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                inline.push(Inline {
                    text: text.to_string(),
                    style,
                    href: link.clone(),
                });
            }
            Event::SoftBreak | Event::HardBreak => inline.push(Inline {
                text: " ".to_owned(),
                style,
                href: link.clone(),
            }),
            _ => {}
        }
    }
    // Text left buffered by a block this mapping does not model (a table cell) becomes a
    // paragraph rather than being dropped — a table an operator wrote must not silently vanish
    // from a policy.
    if !inline.is_empty() {
        blocks.push(Block::Paragraph(inline));
    }

    rsx! {
        for (index, block) in blocks.into_iter().enumerate() {
            {render_block(index, block)}
        }
    }
}

/// The formatting flags in force over one run of text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    italic: bool,
    code: bool,
}

/// One run of text with its formatting and, when it is inside a link, its target.
#[derive(Debug, Clone)]
struct Inline {
    text: String,
    style: Style,
    href: Option<String>,
}

/// A finished block, ready to render.
enum Block {
    Heading(HeadingLevel, Vec<Inline>),
    Paragraph(Vec<Inline>),
    Quote(Vec<Inline>),
    Code(String),
    List(bool, Vec<Vec<Inline>>),
    Rule,
}

fn render_block(index: usize, block: Block) -> Element {
    match block {
        Block::Heading(level, runs) => {
            let class = match level {
                HeadingLevel::H1 | HeadingLevel::H2 => "ik-prose-h2",
                _ => "ik-prose-h3",
            };
            rsx! { div { key: "b{index}", class: "{class}", {render_inline(&runs)} } }
        }
        Block::Paragraph(runs) => rsx! { p { key: "b{index}", {render_inline(&runs)} } },
        Block::Quote(runs) => {
            rsx! { blockquote { key: "b{index}", class: "ik-prose-quote", {render_inline(&runs)} } }
        }
        Block::Code(text) => rsx! { pre { key: "b{index}", class: "ik-prose-code", "{text}" } },
        Block::Rule => rsx! { hr { key: "b{index}", class: "ik-prose-rule" } },
        Block::List(ordered, items) => {
            if ordered {
                rsx! {
                    ol { key: "b{index}",
                        for (n, runs) in items.iter().enumerate() {
                            li { key: "i{n}", {render_inline(runs)} }
                        }
                    }
                }
            } else {
                rsx! {
                    ul { key: "b{index}",
                        for (n, runs) in items.iter().enumerate() {
                            li { key: "i{n}", {render_inline(runs)} }
                        }
                    }
                }
            }
        }
    }
}

/// One block's inline runs. A link's target is passed through unchanged except for the scheme
/// check — an operator writing `javascript:` in a policy is either confused or hostile, and
/// neither is a link worth rendering.
fn render_inline(runs: &[Inline]) -> Element {
    rsx! {
        for (index, run) in runs.iter().enumerate() {
            {
                let text = run.text.clone();
                let class = match (run.style.bold, run.style.italic, run.style.code) {
                    (_, _, true) => "ik-mono",
                    (true, true, _) => "ik-prose-bi",
                    (true, false, _) => "ik-prose-b",
                    (false, true, _) => "ik-prose-i",
                    _ => "",
                };
                match run.href.as_deref().filter(|href| is_safe_href(href)) {
                    Some(href) => rsx! {
                        a {
                            key: "r{index}",
                            class: "ik-link",
                            href: "{href}",
                            rel: "noopener noreferrer nofollow",
                            "{text}"
                        }
                    },
                    None => rsx! { span { key: "r{index}", class: "{class}", "{text}" } },
                }
            }
        }
    }
}

/// Whether a link target is one this page will render.
///
/// An allowlist, not a `javascript:` denylist: `\tjavascript:` and `JaVaScRiPt:` both defeat the
/// obvious check, and the set of schemes a policy document legitimately links is small.
fn is_safe_href(href: &str) -> bool {
    let trimmed = href.trim_start();
    trimmed.starts_with('/')
        || trimmed.starts_with('#')
        || ["https://", "http://", "mailto:"].iter().any(|scheme| {
            trimmed.len() >= scheme.len() && trimmed[..scheme.len()].eq_ignore_ascii_case(scheme)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one security property of this page: an operator's file cannot introduce a scheme the
    /// browser would execute. Case and leading whitespace both defeat a `starts_with`
    /// denylist, which is why this is an allowlist.
    #[test]
    fn only_navigable_schemes_are_rendered_as_links() {
        assert!(is_safe_href("https://example.org"));
        assert!(is_safe_href("HTTPS://example.org"));
        assert!(is_safe_href("mailto:privacy@example.org"));
        assert!(is_safe_href("/account"));
        assert!(is_safe_href("#retention"));

        assert!(!is_safe_href("javascript:alert(1)"));
        assert!(!is_safe_href("  JaVaScRiPt:alert(1)"));
        assert!(!is_safe_href("data:text/html,<script>"));
        assert!(!is_safe_href("vbscript:msgbox"));
    }
}
