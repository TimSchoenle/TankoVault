//! The panel that says what changed, on the start after this installation changed version.
//!
//! It exists because the unattended path is otherwise entirely invisible: an update stages
//! itself while the reader is elsewhere, the installer runs at a start they see as a slow launch,
//! and the only trace is an OS toast that may well have gone to a notification centre nobody
//! opens. A dot on the title bar says *something* happened; this says **what**.
//!
//! The notes are the release body as its author wrote it, rendered through [`crate::markdown`] —
//! `rsx!` nodes, never an HTML string. They are text from github.com, which makes them exactly
//! the kind of input the `dangerous_inner_html` ban is about.
//!
//! Reached two ways, and the same panel either way: opened for the reader at a version change,
//! and opened by the settings sheet for a release on offer, where the question is the mirror
//! image — what *would* I get.

use crate::i18n::use_i18n;
use crate::markdown::markdown;
use crate::update::{self, Notes, ReleaseNotes, Status, UpdateState};
use dioxus::prelude::*;
use inkstone_ui::{Button, Modal, ModalSize, Tone};

/// What the panel is about: a version this installation has taken, or one it has only found.
///
/// The notes read the same either way, and the sentence above them does not: "you are now on
/// 2.1.0" is false for a release the reader has not downloaded, and that is the reading the
/// panel invites when it opens with a changelog and no context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Occasion {
    Applied,
    Offered,
}

impl Occasion {
    fn intro_key(self) -> &'static str {
        match self {
            Self::Applied => "settings.update.notes.appliedIntro",
            Self::Offered => "settings.update.notes.offeredIntro",
        }
    }
}

#[component]
pub(crate) fn WhatsNew() -> Element {
    let i18n = use_i18n();
    let state = use_context::<UpdateState>();
    let notes = state.notes();
    // Read from the status rather than passed in, so the panel cannot be told it is one thing
    // while the updater believes another.
    let occasion = if matches!(state.status(), Status::Applied { .. }) {
        Occasion::Applied
    } else {
        Occasion::Offered
    };
    // The version is the panel's whole subject, so it comes from the notes when they have
    // arrived and from the status when they have not — a panel headed "what's new in" with
    // nothing after it is worse than one that waits.
    let version = match &notes {
        Notes::Ready(ready) => ready.version.clone(),
        Notes::Fetching | Notes::None => subject_version(&state.status()),
    };
    let page = match &notes {
        Notes::Ready(ready) => Some(ready.page.clone()),
        Notes::Fetching | Notes::None => None,
    };

    rsx! {
        Modal {
            title: i18n.args("settings.update.notes.title", &[("version", &version)]),
            intro: Some(i18n.t(occasion.intro_key())),
            size: ModalSize::Wide,
            // It only ever presents; there is nothing half-entered for a stray click to lose.
            dismiss_on_backdrop: true,
            on_close: move |()| update::close_panel(state),
            footer: Some(rsx! {
                if let Some(page) = page {
                    Button {
                        on_click: move |_| crate::platform::navigate_to(&page),
                        {i18n.t("settings.update.openPage")}
                    }
                }
                Button {
                    tone: Tone::Primary,
                    on_click: move |_| update::close_panel(state),
                    {i18n.t("common.close")}
                }
            }),
            NotesBody { notes }
        }
    }
}

/// The version the panel is about, for the moment before the notes have arrived.
fn subject_version(status: &Status) -> String {
    match status {
        Status::Applied { version }
        | Status::Available { version, .. }
        | Status::Staged { version }
        | Status::Unsupported { version, .. } => version.clone(),
        Status::Idle
        | Status::Checking
        | Status::UpToDate
        | Status::Downloading { .. }
        | Status::Failed(_) => crate::build_info::VERSION.to_owned(),
    }
}

/// The notes themselves, or an honest sentence about why there are none.
#[component]
fn NotesBody(notes: Notes) -> Element {
    let i18n = use_i18n();
    rsx! {
        match notes {
            Notes::Ready(ReleaseNotes { body: Some(body), .. }) => rsx! {
                div { class: "ik-prose ik-update-notes", {markdown(&body)} }
            },
            // Told apart on purpose. One is a release published with an empty body, which is a
            // fact about that release; the other is a machine that could not ask, which is a
            // fact about this moment and may not be true in a minute.
            Notes::Ready(ReleaseNotes { body: None, .. }) => rsx! {
                p { class: "ik-muted", {i18n.t("settings.update.notes.empty")} }
            },
            Notes::Fetching => rsx! {
                p { class: "ik-muted", {i18n.t("settings.update.notes.fetching")} }
            },
            Notes::None => rsx! {
                p { class: "ik-muted", {i18n.t("settings.update.notes.unavailable")} }
            },
        }
    }
}
