//! `/licenses` — the third-party notices, rendered.
//!
//! The obligation is old and unchanged: a reader whose browser downloads and runs the WASM
//! bundle has received a binary distribution, and almost every licence in it requires its text
//! to travel along. What changed is the shape it arrives in. The footer used to hand that reader
//! `/third-party-notices` in a new tab — half a megabyte of `text/plain`, outside the app, with
//! the licence a crate ships under findable only by scrolling — and a document nobody can read
//! discharges the obligation on paper and not in fact.
//!
//! So the same notices are rendered here instead, from the structured inventory
//! `xtask notices --json` writes into the image: grouped by licence, each distinct text
//! reproduced once and naming the dependencies that ship it, with the plain-text document still
//! linked for anyone who wants to `curl` it.
//!
//! **The inventory is fetched, not embedded.** It is 1.1 MB (71 KB over the wire, compressed),
//! and `include_str!`ing it would put every byte into the WASM bundle every reader downloads —
//! including the ones who never open this screen. That is the same reasoning that keeps the
//! plain-text document out of the served bundle; see `FrontendConfig::notices_json_path`.

use dioxus::prelude::*;
use serde::Deserialize;

use crate::components::{async_view, SkeletonBlock};
use crate::hooks::use_reload;
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::platform;

/// Where `services/frontend` publishes the structured inventory.
///
/// That crate's `NOTICES_JSON_ROUTE` is the same literal — this is a separate workspace, so there
/// is no compile-time relationship between the two — and `xtask repo-lint` is what holds them
/// equal.
const NOTICES_JSON_ROUTE: &str = "/third-party-notices.json";

/// Dependency names a collapsed notice is labelled with before the list is cut short. Sized to a
/// couple of lines at the screen's measure — enough to tell two notices apart, short enough that
/// the row stays a row.
const LABEL_NAMES: usize = 12;

/// The plain-text document, still published and still linked: it is the copy that survives
/// without a browser, and the one a licence audit will want to diff.
const NOTICES_ROUTE: &str = "/third-party-notices";

/// The inventory as `xtask notices --json` writes it, one entry per dependency graph.
///
/// Hand-written rather than generated, unlike everything in [`crate::models`]: this is not an API
/// payload. It is an artefact of this repository's own build, and the only thing that can move it
/// out from under this file is a change to `xtask/src/notices.rs` in the same commit.
#[derive(Clone, PartialEq, Deserialize)]
struct Inventory {
    sections: Vec<Section>,
}

/// One graph — the service binaries, or the bundle running in the reader's browser.
#[derive(Clone, PartialEq, Deserialize)]
struct Section {
    /// `backend` or `frontend`, translated through `licenses.section.*`.
    slug: String,
    /// The generator's own English heading, used when the catalogue has no string for `slug`.
    /// A section added to `xtask` and not to the catalogues is then named rather than blank.
    title: String,
    ships_as: String,
    licences: Vec<Licence>,
}

/// One licence, and every distinct notice reproduced under it.
#[derive(Clone, PartialEq, Deserialize)]
struct Licence {
    id: String,
    name: String,
    /// Distinct dependencies covered, counted by the generator so this screen and the plain-text
    /// document cannot disagree.
    crates: i64,
    notices: Vec<Notice>,
}

/// One notice and the dependencies that ship it.
#[derive(Clone, PartialEq, Deserialize)]
struct Notice {
    text: String,
    crates: Vec<CrateRef>,
}

#[derive(Clone, PartialEq, Deserialize)]
struct CrateRef {
    name: String,
    version: String,
}

impl Notice {
    /// The collapsed row's label: the dependencies this notice covers, enough of them to
    /// recognise it by.
    ///
    /// Names without versions, and deduplicated: a graph holding three `hashbrown` versions under
    /// one notice would otherwise label the row `hashbrown, hashbrown, hashbrown`.
    ///
    /// Truncated because the largest Apache-2.0 notice here covers 322 crates, and forty lines of
    /// names is not an index of anything. Nothing is lost by cutting it: the row's body lists
    /// every dependency with its version, the count beside the label states the total, and the
    /// body is in the DOM whether the row is open or not — so find-in-page and a crawler still
    /// reach every name.
    fn labelled(&self) -> String {
        let mut names: Vec<&str> = self.crates.iter().map(|c| c.name.as_str()).collect();
        // The generator sorts by name then version, so equal names arrive adjacent.
        names.dedup();
        if names.len() > LABEL_NAMES {
            names.truncate(LABEL_NAMES);
            return format!("{}, …", names.join(", "));
        }
        names.join(", ")
    }
}

#[component]
pub(crate) fn Licenses() -> Element {
    let i18n = use_i18n();
    let reload = use_reload();

    let inventory = use_resource(move || {
        reload.track();
        async move { fetch_inventory().await }
    });

    rsx! {
        div { class: "ik-licences",
            div { class: "ik-flex", style: "gap:9px;margin-bottom:2px;",
                Ic { icon: Icon::Gavel, size: 18 }
                span { class: "ik-kicker", {i18n.t("footer.openSource")} }
            }
            h1 { class: "ik-page-title", style: "margin-bottom:6px;", {i18n.t("licenses.title")} }
            p { class: "ik-licences-intro", {i18n.t("licenses.intro")} }
            p { class: "ik-licences-note", {i18n.t("licenses.note")} }
            a {
                class: "ik-link ik-licences-plain",
                href: "{NOTICES_ROUTE}",
                target: "_blank",
                rel: "noopener noreferrer",
                {i18n.t("licenses.plainText")}
                Ic { icon: Icon::OpenInNew, size: 12 }
            }

            {
                async_view(
                    &inventory,
                    reload,
                    || rsx! { SkeletonBlock { height: 420 } },
                    move |found: &Option<Inventory>| match found {
                        Some(inventory) => rsx! { Sections { inventory: inventory.clone() } },
                        // Not an error box: an image built before the inventory existed, and a
                        // desktop build with no server chosen yet, are both "nothing to show"
                        // rather than "something went wrong", and a retry button would keep
                        // offering to do it again.
                        None => rsx! {
                            p { class: "ik-note", {i18n.t("licenses.unavailable")} }
                        },
                    },
                )
            }
        }
    }
}

/// The inventory, or `None` when this deployment publishes none.
///
/// A 404 is the expected absence — an image built before this existed, or a checkout where
/// `xtask notices --json` has not run — and is not reported as a failure. Anything else is: a
/// reader who cannot reach the notices should be told, not shown an empty page.
async fn fetch_inventory() -> Result<Option<Inventory>, String> {
    let origin = platform::origin();
    // The desktop build before its first-run connect screen: there is no server to resolve the
    // document against, so there is nothing to fetch and nothing to report.
    if origin.is_empty() {
        return Ok(None);
    }

    let url = format!("{}{NOTICES_JSON_ROUTE}", origin.trim_end_matches('/'));
    let response = reqwest::get(url).await.map_err(|_| "error.network")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err("error.network".to_owned());
    }
    response
        .json::<Inventory>()
        .await
        .map(Some)
        .map_err(|_| "error.network".to_owned())
}

/// Every graph, each with its summary and its notices.
#[component]
fn Sections(inventory: Inventory) -> Element {
    let i18n = use_i18n();
    rsx! {
        {inventory.sections.iter().map(|section| {
            let heading = i18n
                .t_opt(&format!("licenses.section.{}", section.slug))
                .unwrap_or_else(|| section.title.clone());
            rsx! {
                section { key: "{section.slug}", class: "ik-licences-part",
                    h2 { class: "ik-prose-h2", "{heading}" }
                    p { class: "ik-licences-ships", "{section.ships_as}" }
                    Summary { section: section.clone() }
                    {section.licences.iter().map(|licence| rsx! {
                        Group { key: "{section.slug}-{licence.id}", licence: licence.clone() }
                    })}
                }
            }
        })}
    }
}

/// The licences one graph resolves to, as chips: how much of it each covers, and how many
/// distinct texts that took.
#[component]
fn Summary(section: Section) -> Element {
    let i18n = use_i18n();
    rsx! {
        ul { class: "ik-licence-chips",
            {section.licences.iter().map(|licence| rsx! {
                li { key: "{licence.id}", class: "ik-licence-chip",
                    span { class: "ik-licence-chip-id", "{licence.id}" }
                    span { class: "ik-licence-chip-name", "{licence.name}" }
                    span { class: "ik-licence-chip-count", {coverage(i18n, licence)} }
                }
            })}
        }
    }
}

/// One licence and every distinct notice reproduced under it.
///
/// Each notice is a disclosure row labelled by the dependencies it covers, so the collapsed
/// section reads as an index of the graph and only the licence file itself has to be opened.
/// Collapsed, not deferred — `details` hides its contents, it does not withhold them — so
/// find-in-page and a crawler still reach every word of every licence.
#[component]
fn Group(licence: Licence) -> Element {
    let i18n = use_i18n();
    rsx! {
        section { class: "ik-licence-group",
            h3 { class: "ik-licence-group-head",
                span { "{licence.name} ({licence.id})" }
                span { class: "ik-licence-group-count", {coverage(i18n, &licence)} }
            }
            {licence.notices.iter().enumerate().map(|(i, notice)| {
                let label = notice.labelled();
                rsx! {
                    details { key: "{licence.id}-{i}", class: "ik-licence-notice",
                        summary {
                            span { class: "ik-licence-notice-crates", "{label}" }
                            span { class: "ik-licence-notice-count",
                                {i18n.plural("licenses.crates", count(notice.crates.len()), &[])}
                            }
                        }
                        ul { class: "ik-licence-notice-list",
                            {notice.crates.iter().map(|krate| rsx! {
                                li { key: "{krate.name} {krate.version}",
                                    span { "{krate.name}" }
                                    span { class: "ik-licence-notice-version", "{krate.version}" }
                                }
                            })}
                        }
                        pre { class: "ik-licence-text", "{notice.text}" }
                    }
                }
            })}
        }
    }
}

/// `N dependencies`, and `· M licence texts` when there is more than one.
///
/// The second clause is omitted at one because "1 licence text" beside a licence invites the
/// reader to look for a distinction that is not there.
fn coverage(i18n: Translator, licence: &Licence) -> String {
    let crates = i18n.plural("licenses.crates", licence.crates, &[]);
    if licence.notices.len() > 1 {
        let texts = i18n.plural("licenses.texts", count(licence.notices.len()), &[]);
        return format!("{crates} · {texts}");
    }
    crates
}

/// A length as the plural rule wants it. Saturating rather than `as`: a count that wrapped
/// negative would pick the wrong plural form for a reason nobody would ever find.
fn count(len: usize) -> i64 {
    i64::try_from(len).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `xtask notices --json` writes, cut to two notices. Parsing it here is the only
    /// thing holding this file to that one: the generator is in another workspace, so a field
    /// renamed there fails at this assertion rather than on a reader's screen.
    const SAMPLE: &str = r#"{"sections":[{"slug":"backend","title":"Part 1 of 2","ships_as":"the service binaries","licences":[{"id":"MIT","name":"MIT License","crates":3,"notices":[{"text":"Copyright (c) 2020","crates":[{"name":"hashbrown","version":"0.14.5"},{"name":"hashbrown","version":"0.15.5"},{"name":"serde","version":"1.0.0"}]}]}]}]}"#;

    #[test]
    fn the_generators_document_parses() {
        let inventory: Inventory = serde_json::from_str(SAMPLE).expect("parses");
        let section = &inventory.sections[0];
        assert_eq!(section.slug, "backend");
        assert_eq!(section.ships_as, "the service binaries");
        let licence = &section.licences[0];
        assert_eq!(licence.id, "MIT");
        assert_eq!(licence.crates, 3);
        assert_eq!(licence.notices[0].text, "Copyright (c) 2020");
    }

    /// A graph can hold two versions of one crate under one notice, and the label a collapsed row
    /// carries must not read `hashbrown, hashbrown`. The count beside it still counts both.
    #[test]
    fn a_notice_labels_a_crate_once_however_many_versions_it_covers() {
        let inventory: Inventory = serde_json::from_str(SAMPLE).expect("parses");
        let notice = &inventory.sections[0].licences[0].notices[0];
        assert_eq!(notice.labelled(), "hashbrown, serde");
        assert_eq!(notice.crates.len(), 3);
    }
}
