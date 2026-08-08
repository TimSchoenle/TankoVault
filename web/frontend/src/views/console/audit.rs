//! The privileged-action audit trail (design §16): filtered, paged, newest first.
//!
//! Every filter here is a server-side predicate. Filtering a page client-side would silently
//! answer a different question than the one asked — the panel would say "no matches" when it
//! means "no matches in the forty rows I happen to hold".

use crate::api;
use crate::components::{async_view, CompactPager, SkeletonRows, Window};
use crate::i18n::use_i18n;
use crate::models::*;
use crate::util::rel_time;
use crate::views::console::query::Window as TimeWindow;
use crate::views::console::{config_editor_text, use_console_nav, ConsoleQuery, RefreshTick};
use dioxus::prelude::*;
use progenitor_client::ResponseValue;

/// Rows per page. The server's default; it clamps regardless.
const PAGE_SIZE: i64 = 40;

/// The tone an action token is drawn in.
///
/// Destructive actions are the ones an operator scans the trail *for*, and a wall of identical
/// neutral pills is what made that scan slow.
fn action_tone(action: &str) -> &'static str {
    const DESTRUCTIVE: [&str; 4] = [".delete", ".revoke", ".erasure", ".purge"];
    const GRANTS: [&str; 3] = [".grant", ".permissions", ".suspend"];
    if DESTRUCTIVE.iter().any(|suffix| action.contains(suffix)) {
        "ik-pill vermilion"
    } else if GRANTS.iter().any(|suffix| action.contains(suffix)) {
        "ik-pill amber"
    } else {
        "ik-pill"
    }
}

/// The catalogue wording for an action, falling back to the raw token.
///
/// A server-side action this build has never heard of renders as itself rather than as a
/// missing key: the vocabulary lives in the handlers, and the console must not need a release
/// to display a new one.
fn action_label(i18n: crate::i18n::Translator, action: &str) -> String {
    i18n.t_opt(&format!("console.audit.action.{action}"))
        .unwrap_or_else(|| action.to_owned())
}

/// Privileged-action audit trail: filters, paging, and a per-row detail expander.
#[component]
pub(super) fn AuditPanel(tick: RefreshTick) -> Element {
    let api = api::use_api();
    let i18n = use_i18n();
    let nav = use_console_nav();
    let view = nav.query();

    let action = view.status.clone();
    let target = view.q.clone();
    let since = view.since;
    let page = i64::from(view.page);

    let trail = use_resource(use_reactive!(|(action, target, since, page)| {
        tick.track();
        let client = api.client();
        async move {
            let mut request = client
                .audit_log()
                .limit(u32::try_from(PAGE_SIZE).unwrap_or(40))
                .offset(u32::try_from(page * PAGE_SIZE).unwrap_or(0));
            if let Some(action) = action.as_deref() {
                request = request.action(action);
            }
            if !target.trim().is_empty() {
                request = request.target(target.trim());
            }
            if let Some(from) = since.since_iso() {
                request = request.since(from);
            }
            request
                .send()
                .await
                .map(ResponseValue::into_inner)
                .map_err(|e| api::friendly_error(i18n, e))
        }
    }));

    // The picker's vocabulary comes from the data, so a newly recorded action is filterable
    // the first time it happens.
    let actions = use_resource(move || {
        let client = api.client();
        async move {
            client
                .audit_actions()
                .send()
                .await
                .map(ResponseValue::into_inner)
                .unwrap_or_default()
        }
    });

    let (rows, total) = match &*trail.read() {
        Some(Ok(page_data)) => (page_data.items.clone(), page_data.total),
        _ => (Vec::new(), 0),
    };
    let window = Window {
        offset: page * PAGE_SIZE,
        page_len: i64::try_from(rows.len()).unwrap_or(0),
        total,
    };
    let known_actions = actions.read().clone().unwrap_or_default();

    rsx! {
        section { style: "margin-bottom:18px;",
            h3 { {i18n.t("console.audit.title")} }
            div { class: "ik-flex", style: "gap:8px;flex-wrap:wrap;margin-bottom:10px;",
                select {
                    class: "ik-input",
                    "aria-label": i18n.t("console.audit.filter.action"),
                    value: view.status.clone().unwrap_or_default(),
                    onchange: move |event: FormEvent| {
                        let chosen = event.value();
                        let next = ConsoleQuery {
                            status: (!chosen.is_empty()).then_some(chosen),
                            page: 0,
                            ..nav.query()
                        };
                        nav.filter(next);
                    },
                    option { value: "", {i18n.t("console.audit.filter.anyAction")} }
                    for key in known_actions.clone() {
                        option { key: "{key}", value: "{key}", {action_label(i18n, &key)} }
                    }
                }
                select {
                    class: "ik-input",
                    "aria-label": i18n.t("console.audit.filter.window"),
                    value: view.since.token(),
                    onchange: move |event: FormEvent| {
                        let next = ConsoleQuery {
                            since: TimeWindow::parse_token(&event.value()),
                            page: 0,
                            ..nav.query()
                        };
                        nav.filter(next);
                    },
                    for option in TimeWindow::ALL {
                        option {
                            key: "{option.label_key()}",
                            value: option.token(),
                            {i18n.t(option.label_key())}
                        }
                    }
                }
                input {
                    class: "ik-input",
                    style: "flex:1;min-width:14ch;",
                    r#type: "search",
                    placeholder: i18n.t("console.audit.filter.target"),
                    "aria-label": i18n.t("console.audit.filter.target"),
                    value: "{view.q}",
                    oninput: move |event: FormEvent| {
                        nav.filter(nav.query().with_search(event.value()));
                    },
                }
            }
            {
                async_view(
                    &trail,
                    tick.reload(),
                    || rsx! { SkeletonRows { count: 6, height: 22 } },
                    |_| {
                        if rows.is_empty() {
                            return rsx! {
                                div { class: "ik-empty", style: "padding:24px;",
                                    {i18n.t("console.audit.empty")}
                                }
                            };
                        }
                        rsx! {
                            div { class: "ik-tablewrap",
                                table { class: "ik-table ik-table-compact",
                                    thead {
                                        tr {
                                            th { {i18n.t("console.audit.col.when")} }
                                            th { {i18n.t("console.audit.col.actor")} }
                                            th { {i18n.t("console.audit.col.action")} }
                                            th { {i18n.t("console.audit.col.target")} }
                                            th { style: "text-align:right;", {i18n.t("console.audit.col.detail")} }
                                        }
                                    }
                                    tbody {
                                        for entry in rows.clone() {
                                            AuditRow { key: "{entry.id}", entry: Signal::new(entry) }
                                        }
                                    }
                                }
                            }
                        }
                    },
                )
            }
            CompactPager {
                page,
                window,
                on_page: move |next: i64| {
                    nav.select(nav.query().with_page(u32::try_from(next).unwrap_or(0)));
                },
            }
        }
    }
}

/// One trail row, with the stored `detail` behind an expander.
///
/// `detail` is what actually changed — the before and after of a permission or a flag — and it
/// was fetched and thrown away before this panel could show it.
#[component]
fn AuditRow(entry: Signal<AuditEntry>) -> Element {
    let i18n = use_i18n();
    let mut open = use_signal(|| false);
    let a = entry.read();
    let actor = a
        .actor
        .clone()
        .unwrap_or_else(|| i18n.t("console.audit.system"));
    let target = a.target.clone().unwrap_or_else(|| i18n.t("time.unknown"));
    let has_detail =
        !a.detail.is_null() && a.detail.as_object().is_none_or(|object| !object.is_empty());
    let expanded = *open.read();

    rsx! {
        tr {
            td { class: "ik-muted ik-mono", style: "font-size:12px;white-space:nowrap;",
                "{rel_time(i18n, Some(a.created_at.as_str()))}"
            }
            td { "{actor}" }
            td {
                span { class: action_tone(&a.action), {action_label(i18n, &a.action)} }
            }
            td { class: "ik-mono ik-muted", style: "font-size:12px;word-break:break-all;", "{target}" }
            td { style: "text-align:right;",
                if has_detail {
                    button {
                        class: "ik-btn xs",
                        "aria-expanded": if expanded { "true" } else { "false" },
                        onclick: move |_| {
                            let next = !*open.peek();
                            open.set(next);
                        },
                        if expanded {
                            {i18n.t("console.audit.hideDetail")}
                        } else {
                            {i18n.t("console.audit.showDetail")}
                        }
                    }
                }
            }
        }
        if expanded {
            tr {
                td { colspan: "5", style: "padding:0 10px 10px;",
                    pre {
                        class: "ik-mono",
                        style: "margin:0;padding:10px;background:var(--surface);border-radius:8px;\
                                font-size:11.5px;overflow-x:auto;white-space:pre-wrap;",
                        {config_editor_text(&a.detail)}
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::action_tone;

    /// The tone is derived from the token, not from a table of known actions — a destructive
    /// action this build has never heard of must still be drawn as destructive, because the
    /// operator scanning the trail for one is the reason the tone exists.
    #[test]
    fn an_unknown_destructive_action_is_still_toned_as_destructive() {
        assert_eq!(action_tone("admin.provider.delete"), "ik-pill vermilion");
        assert_eq!(action_tone("some.future.thing.revoke"), "ik-pill vermilion");
        assert_eq!(action_tone("privacy.erasure.fulfil"), "ik-pill vermilion");
        assert_eq!(action_tone("admin.user.permissions"), "ik-pill amber");
        assert_eq!(action_tone("scan.trigger"), "ik-pill");
    }
}
