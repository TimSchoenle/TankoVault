//! One tuning row, shared by the two surfaces that publish one: the recommender's registry and
//! the duplicate sweep's automatic-merge policy.
//!
//! Shared because the *row* is the same thing on both — a compiled description, a range, an
//! effective value, who last moved it, and the two controls that record or withdraw a decision.
//! What differs is which endpoint the buttons call, which is [`Surface`], and how the value is
//! rendered, which is the kind. Two copies of this drifted the moment either page was touched;
//! the visible symptom was a page that still looked right and no longer said the same thing as
//! the server.
//!
//! Deliberately not on the shared auto-refresh tick, for the reason `flags` is not: a background
//! refetch landing between an operator typing a value and pressing Save would discard it.

use crate::api;
use crate::components::{ErrorLine, OutcomeLine, StepUpGate};
use crate::hooks::{use_busy, use_outcome, Busy, Outcome, Reload};
use crate::i18n::{use_i18n, Translator};
use crate::icons::{Ic, Icon};
use crate::util::iso_date;
use crate::wire::types::{
    Applies, MergePolicyKnob, SetMergePolicy, SetTunable, TunableKind, TunableView,
};
use dioxus::prelude::*;
use inkstone_ui::{Button, Pill, ToggleButton, Tone};

/// Which surface a row belongs to, and therefore which endpoints its controls call.
///
/// The two are separately permissioned — `recsys.write` against `merge.write` — and the server
/// refuses a key belonging to the other, so this is the client half of a boundary that is
/// enforced whatever this enum says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Surface {
    Recsys,
    Matching,
}

/// One tuning value as a row renders it, whichever surface published it.
#[derive(Clone, PartialEq)]
pub(super) struct Knob {
    pub(super) surface: Surface,
    pub(super) key: String,
    pub(super) title: String,
    pub(super) description: String,
    pub(super) kind: TunableKind,
    pub(super) applies: Applies,
    pub(super) value: f64,
    /// What this deployment falls back to — the compiled default for the recommender, the
    /// configured `matching` value for the sweep. Either way it is what resetting returns to.
    pub(super) default_value: f64,
    pub(super) min: f64,
    pub(super) max: f64,
    pub(super) overridden: bool,
    /// Whether `min` is a privacy threshold rather than a tuning decision.
    pub(super) privacy_floor: bool,
    pub(super) note: Option<String>,
    pub(super) updated_by: Option<String>,
    pub(super) updated_at: Option<String>,
}

impl Knob {
    /// A recommendation tunable, whose kind and `applies` are already typed on the wire.
    pub(super) fn recsys(row: &TunableView) -> Self {
        Self {
            surface: Surface::Recsys,
            key: row.key.to_string(),
            title: row.title.clone(),
            description: row.description.clone(),
            kind: row.kind,
            applies: row.applies,
            value: row.value,
            default_value: row.default_value,
            min: row.min,
            max: row.max,
            overridden: row.overridden,
            privacy_floor: row.privacy_floor,
            note: row.note.clone(),
            updated_by: row.updated_by.clone(),
            updated_at: row.updated_at.clone(),
        }
    }

    /// A merge-policy knob, whose kind and `applies` travel as tokens.
    ///
    /// A token this build does not know falls back to the shapes that render *something* honest
    /// — a plain number, and the badge that promises the least — rather than dropping the row.
    /// A knob missing from the page cannot be reset, and this page is the only thing that can
    /// withdraw an override.
    pub(super) fn matching(row: &MergePolicyKnob) -> Self {
        Self {
            surface: Surface::Matching,
            key: row.key.clone(),
            title: row.title.clone(),
            description: row.description.clone(),
            kind: row.kind.parse().unwrap_or(TunableKind::Weight),
            applies: row.applies.parse().unwrap_or(Applies::NextSweep),
            value: row.value,
            default_value: row.default_value,
            min: row.min,
            max: row.max,
            overridden: row.overridden,
            privacy_floor: false,
            note: row.note.clone(),
            updated_by: row.updated_by.clone(),
            updated_at: row.updated_at.clone(),
        }
    }

    /// Whether this knob is on, for the kinds that are a switch rather than a number.
    fn is_on(&self) -> bool {
        self.value >= 0.5
    }
}

/// One group heading and its rows. A heading over nothing reads as a failed load, so an empty
/// group renders as nothing at all.
#[component]
pub(super) fn KnobGroup(
    title: String,
    rows: Vec<Knob>,
    can_write: bool,
    reload: Reload,
    gate: StepUpGate,
) -> Element {
    if rows.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "ik-subhead", style: "margin-top:18px;", "{title}" }
        div { class: "ik-tablewrap",
            for row in rows {
                KnobRow { key: "{row.key}", row, can_write, reload, gate }
            }
        }
    }
}

/// One tuning value: what it does, what it is, and the controls to change it.
#[component]
pub(super) fn KnobRow(row: Knob, can_write: bool, reload: Reload, gate: StepUpGate) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let busy = use_busy();
    let outcome = use_outcome();

    let key = row.key.clone();
    let value = row.value;
    let kind = row.kind;
    let mut draft = use_signal(|| format_value(value, kind));
    let mut note = use_signal(String::new);

    // Re-seed when the server's value moves under the editor — a reset, or another operator's
    // write. Without this a reset leaves the field showing the number that was just withdrawn.
    use_effect(use_reactive!(|(value, kind)| {
        draft.set(format_value(value, kind));
    }));

    let changed = row.overridden && (value - row.default_value).abs() > f64::EPSILON;
    let parsed = draft.read().trim().parse::<f64>().ok();
    let in_range = parsed.is_some_and(|n| n >= row.min && n <= row.max);
    let dirty = parsed.is_some_and(|n| (n - value).abs() > f64::EPSILON);
    let toggle = kind == TunableKind::Toggle;
    let on_now = row.is_on();
    let surface = row.surface;

    rsx! {
        div { class: "ik-row", style: "align-items:flex-start;",
            div { class: "grow",
                div { class: "ik-flex", style: "gap:8px;align-items:center;flex-wrap:wrap;",
                    strong { style: "font-size:13px;", "{row.title}" }
                    AppliesPill { applies: row.applies }
                    if changed {
                        Pill {
                            tone: Tone::Accent,
                            {i18n.t("console.recsys.changed")}
                        }
                    }
                    if row.privacy_floor {
                        Pill {
                            class: "star",
                            title: i18n.t("console.recsys.privacyFloorHint"),
                            {i18n.t("console.recsys.privacyFloor")}
                        }
                    }
                }
                div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:2px;", "{key}" }
                p { class: "ik-muted", style: "font-size:12px;margin:6px 0 0;max-width:74ch;",
                    "{row.description}"
                }
                div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:4px;",
                    if toggle {
                        {
                            i18n.args(
                                "console.recsys.boundsToggle",
                                &[("default", &on_off(i18n, row.default_value >= 0.5))],
                            )
                        }
                    } else {
                        {
                            i18n.args(
                                bounds_key(surface),
                                &[
                                    ("min", &format_value(row.min, row.kind)),
                                    ("max", &format_value(row.max, row.kind)),
                                    ("default", &format_value(row.default_value, row.kind)),
                                ],
                            )
                        }
                    }
                }
                if let Some(stored) = row.note.clone() {
                    p { style: "font-size:12px;margin:4px 0 0;", "“{stored}”" }
                }
                if let Some(by) = row.updated_by.clone() {
                    div { class: "ik-mono ik-muted", style: "font-size:11px;margin-top:2px;",
                        {
                            let when = iso_date(row.updated_at.as_deref()).to_owned();
                            i18n.args("console.recsys.changedBy", &[("user", &by), ("date", &when)])
                        }
                    }
                }
                OutcomeLine { outcome: outcome.read().clone() }
                if !toggle && parsed.is_some() && !in_range {
                    ErrorLine { message: i18n.t("console.recsys.outOfRange") }
                }
            }

            if can_write {
                div { class: "ik-flex", style: "gap:6px;flex-shrink:0;align-items:flex-start;",
                    if toggle {
                        // The switch *is* the write: a guard has one other state, so asking for
                        // it and then asking again for Save would be two clicks for one decision.
                        ToggleButton {
                            on: on_now,
                            disabled: busy.is_busy(),
                            on_toggle: {
                                let key = key.clone();
                                move |_| {
                                    write(WriteRequest {
                                        api,
                                        i18n,
                                        surface,
                                        key: key.clone(),
                                        value: Some(if on_now { 0.0 } else { 1.0 }),
                                        note: String::new(),
                                        busy,
                                        outcome,
                                        reload,
                                        gate,
                                    });
                                }
                            },
                            {on_off(i18n, on_now)}
                        }
                    } else {
                        div { style: "display:flex;flex-direction:column;gap:6px;width:190px;",
                            input {
                                class: "ik-input",
                                style: "font-family:var(--font-mono);font-size:13px;padding:7px 10px;",
                                r#type: "text",
                                inputmode: "decimal",
                                "aria-label": i18n.args("console.recsys.valueLabel", &[("title", &row.title)]),
                                value: "{draft}",
                                oninput: move |event| draft.set(event.value()),
                            }
                            input {
                                class: "ik-input",
                                style: "font-size:12px;padding:7px 10px;",
                                r#type: "text",
                                placeholder: i18n.t("console.recsys.notePlaceholder"),
                                "aria-label": i18n.t("console.recsys.notePlaceholder"),
                                value: "{note}",
                                oninput: move |event| note.set(event.value()),
                            }
                        }
                        SaveButton {
                            surface: row.surface,
                            tunable: key.clone(),
                            value: parsed.unwrap_or(value),
                            note,
                            enabled: dirty && in_range,
                            busy,
                            outcome,
                            reload,
                            gate,
                        }
                    }
                    // Only when there is an override to withdraw; otherwise reset does nothing.
                    if row.overridden {
                        ResetButton {
                            surface: row.surface,
                            tunable: key.clone(),
                            busy,
                            outcome,
                            reload,
                            gate,
                        }
                    }
                }
            } else if toggle {
                span { class: "ik-mono", style: "font-size:13px;flex:none;",
                    {on_off(i18n, on_now)}
                }
            } else {
                span { class: "ik-mono", style: "font-size:13px;flex:none;",
                    {format_value(value, row.kind)}
                }
            }
        }
    }
}

/// When a change to this value actually reaches a reader.
///
/// Shown on every row because it is this surface's most likely failure: an operator raises a
/// value baked into stored vectors, sees no change, and concludes the page is broken.
#[component]
fn AppliesPill(applies: Applies) -> Element {
    let i18n = use_i18n();
    let (class, key) = match applies {
        Applies::Immediately => ("ik-pill jade", "console.recsys.applies.immediately"),
        Applies::NextBuild => ("ik-pill", "console.recsys.applies.nextBuild"),
        Applies::NextFullBuild => ("ik-pill star", "console.recsys.applies.nextFullBuild"),
        Applies::NextSweep => ("ik-pill", "console.recsys.applies.nextSweep"),
    };
    rsx! {
        span { class: "{class}", {i18n.t(key)} }
    }
}

/// Record an explicit decision for one value.
#[component]
fn SaveButton(
    surface: Surface,
    tunable: String,
    value: f64,
    note: Signal<String>,
    enabled: bool,
    busy: Busy,
    outcome: Signal<Outcome>,
    reload: Reload,
    gate: StepUpGate,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let mut note = note;

    let click = move |_| {
        let written = note.peek().trim().to_owned();
        write(WriteRequest {
            api,
            i18n,
            surface,
            key: tunable.clone(),
            value: Some(value),
            note: written,
            busy,
            outcome,
            reload,
            gate,
        });
        note.set(String::new());
    };

    rsx! {
        Button {
            tone: Tone::Primary,
            style: "flex:none;",
            disabled: busy.is_busy() || !enabled,
            on_click: click,
            {i18n.t("common.save")}
        }
    }
}

/// Drop the stored override so the value follows what the deployment falls back to.
///
/// Distinct from writing that same number, which records a decision that would survive a future
/// change of that fallback — so this is its own control rather than a "type the default" hint.
#[component]
fn ResetButton(
    surface: Surface,
    tunable: String,
    busy: Busy,
    outcome: Signal<Outcome>,
    reload: Reload,
    gate: StepUpGate,
) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();

    let click = move |_| {
        write(WriteRequest {
            api,
            i18n,
            surface,
            key: tunable.clone(),
            value: None,
            note: String::new(),
            busy,
            outcome,
            reload,
            gate,
        });
    };

    rsx! {
        Button {
            style: "flex:none;",
            disabled: busy.is_busy(),
            title: i18n.t("console.recsys.resetHint"),
            on_click: click,
            Ic { icon: Icon::Refresh, size: 14 }
        }
    }
}

/// One write, with everything the three controls that make one have in common.
///
/// `api` and `i18n` are carried rather than resolved inside [`write`]: both are hooks, and a
/// hook called from an event handler runs outside the render that establishes hook order.
struct WriteRequest {
    api: api::Api,
    i18n: Translator,
    surface: Surface,
    key: String,
    /// `None` withdraws the override.
    value: Option<f64>,
    note: String,
    busy: Busy,
    outcome: Signal<Outcome>,
    reload: Reload,
    gate: StepUpGate,
}

/// Send one write behind the panel's step-up gate, then reload the list it came from.
///
/// The whole list is refetched rather than the row patched from the response: a page that
/// believed its own request can show a value the server refused to store, and both endpoints
/// answer with the whole registry precisely so it does not have to.
fn write(req: WriteRequest) {
    let WriteRequest {
        api,
        i18n,
        surface,
        key,
        value,
        note,
        busy,
        mut outcome,
        reload,
        gate,
    } = req;

    gate.attempt(move || {
        if !busy.claim() {
            return;
        }
        let key = key.clone();
        let note = note.clone();
        let client = gate.client(api);
        outcome.set(None);
        spawn(async move {
            let sent = match (surface, value) {
                (Surface::Recsys, Some(value)) => {
                    let body = SetTunable {
                        value,
                        note: (!note.is_empty()).then_some(note),
                    };
                    client
                        .set_tunable()
                        .key(key)
                        .body(body)
                        .send()
                        .await
                        .map(|_| ())
                }
                (Surface::Recsys, None) => client.reset_tunable().key(key).send().await.map(|_| ()),
                (Surface::Matching, Some(value)) => {
                    let body = SetMergePolicy {
                        value,
                        note: (!note.is_empty()).then_some(note),
                    };
                    client
                        .set_merge_policy()
                        .key(key)
                        .body(body)
                        .send()
                        .await
                        .map(|_| ())
                }
                (Surface::Matching, None) => client
                    .reset_merge_policy()
                    .key(key)
                    .send()
                    .await
                    .map(|_| ()),
            };
            if let Err(e) = sent {
                if !gate.refused(api::Refusal::of(&e)) {
                    let message =
                        api::problem_detail(&e).unwrap_or_else(|| api::guarded_error(i18n, e));
                    outcome.set(Some(Err(message)));
                }
            }
            busy.release();
            reload.bump();
        });
    });
}

/// How to word the fallback this surface resets to.
///
/// The recommender's is the value the build ships with; the sweep's is whatever this deployment
/// configured, which is the same number only until someone sets a `matching` key. Saying "ships
/// as" over a configured value would name a number nothing uses.
const fn bounds_key(surface: Surface) -> &'static str {
    match surface {
        Surface::Recsys => "console.recsys.bounds",
        Surface::Matching => "console.recsys.boundsConfigured",
    }
}

/// A switch's state, as a word rather than a number.
fn on_off(i18n: crate::i18n::Translator, on: bool) -> String {
    i18n.t(if on {
        "console.recsys.toggle.on"
    } else {
        "console.recsys.toggle.off"
    })
}

/// Render a tuning value the way its kind means it.
///
/// Whole numbers for the kinds the pipeline rounds anyway, two decimals for the fractional ones
/// — a shelf size shown as `12.00` invites an operator to wonder what the fraction does.
pub(super) fn format_value(value: f64, kind: TunableKind) -> String {
    match kind {
        TunableKind::Count | TunableKind::Days | TunableKind::Seconds | TunableKind::Toggle => {
            format!("{value:.0}")
        }
        TunableKind::Ratio | TunableKind::Weight => format!("{value:.2}"),
    }
}

#[cfg(test)]
mod tests {
    use super::format_value;
    use crate::wire::types::TunableKind;

    /// A count shown with decimals reads as a value the pipeline honours fractionally, which
    /// none of the counted kinds do.
    #[test]
    fn counted_kinds_render_whole_and_fractional_ones_do_not() {
        assert_eq!(format_value(12.0, TunableKind::Count), "12");
        assert_eq!(format_value(21600.0, TunableKind::Seconds), "21600");
        assert_eq!(format_value(0.7, TunableKind::Ratio), "0.70");
        assert_eq!(format_value(-0.6, TunableKind::Weight), "-0.60");
    }
}
