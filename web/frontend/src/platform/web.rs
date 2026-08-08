//! The browser implementation of [`crate::platform`], typed through `web-sys`.
//!
//! Read the module contract in `mod.rs` before adding anything here — in particular, why none of
//! this may be written as `document::eval`.

use dioxus::history::History;
use futures_util::StreamExt as _;
use wasm_bindgen::JsCast as _;

pub(crate) fn store_get(key: &str) -> Option<String> {
    storage()?.get_item(key).ok().flatten()
}

pub(crate) fn store_set(key: &str, value: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(key, value);
    }
}

pub(crate) fn store_remove(key: &str) {
    if let Some(storage) = storage() {
        let _ = storage.remove_item(key);
    }
}

/// `window.localStorage`, if this browser exposes it to the document; a policy block and
/// "unset" both collapse to `None` here.
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

pub(crate) fn root_attribute(name: &str) -> Option<String> {
    root()?.get_attribute(name)
}

pub(crate) fn set_root_attribute(name: &str, value: &str) {
    if let Some(root) = root() {
        let _ = root.set_attribute(name, value);
    }
}

pub(crate) fn remove_root_attribute(name: &str) {
    if let Some(root) = root() {
        let _ = root.remove_attribute(name);
    }
}

/// The `<html>` element.
fn root() -> Option<web_sys::Element> {
    web_sys::window()?.document()?.document_element()
}

/// Set directly rather than through an `HtmlElement` downcast.
pub(crate) fn set_document_language(tag: &str) {
    set_root_attribute("lang", tag);
}

pub(crate) fn set_document_title(title: &str) {
    if let Some(document) = web_sys::window().and_then(|window| window.document()) {
        document.set_title(title);
    }
}

pub(crate) fn navigate_to(url: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_href(url);
    }
}

pub(crate) fn select_focused_text() {
    let Some(field) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element())
        .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
        return;
    };
    field.select();
}

/// The history provider the router is launched with: `dioxus-web`'s own, with one behaviour
/// taken out.
///
/// [`WebHistory`](dioxus::web::WebHistory) ends *every* navigation by scrolling the window back
/// to the top — a replace exactly as much as a push. That is right for a push and wrong for a
/// replace, because a replace is how a screen corrects its own address without navigating: the
/// moment Discover's second page of covers came into view it recorded the position in `?at=`,
/// the window jumped to the top, the first page reported itself as the topmost one again, and
/// `at` was replaced straight back to nothing — the reader could not get past the first page.
/// The debounced filter boxes on the watchlist and in the console replace per keystroke for the
/// same reason and paid the same price. Only the browser build ever showed it: the desktop
/// renderer routes through `MemoryHistory`, which has no scroll position to move.
pub(crate) fn history_provider() -> std::rc::Rc<dyn History> {
    std::rc::Rc::new(InPlaceHistory {
        inner: dioxus::web::WebHistory::default(),
    })
}

/// [`history_provider`]'s type. Everything is `WebHistory`'s except [`History::replace`].
struct InPlaceHistory {
    inner: dioxus::web::WebHistory,
}

impl History for InPlaceHistory {
    fn current_route(&self) -> String {
        self.inner.current_route()
    }

    fn current_prefix(&self) -> Option<String> {
        self.inner.current_prefix()
    }

    fn can_go_back(&self) -> bool {
        self.inner.can_go_back()
    }

    fn go_back(&self) {
        self.inner.go_back();
    }

    fn can_go_forward(&self) -> bool {
        self.inner.can_go_forward()
    }

    fn go_forward(&self) {
        self.inner.go_forward();
    }

    fn push(&self, route: String) {
        self.inner.push(route);
    }

    /// The current entry, re-addressed and left where it is.
    ///
    /// The `[x, y]` state shape is `WebHistory`'s and has to stay it: its own `popstate` handler
    /// reads the entry back to restore the scroll position on a back, and anything else there
    /// reads as "no position recorded".
    fn replace(&self, route: String) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(history) = window.history() else {
            return;
        };
        let state = js_sys::Array::new();
        state.push(&wasm_bindgen::JsValue::from_f64(
            window.scroll_x().unwrap_or_default(),
        ));
        state.push(&wasm_bindgen::JsValue::from_f64(
            window.scroll_y().unwrap_or_default(),
        ));
        let url = full_path(self.inner.current_prefix().as_deref(), &route);
        let _ = history.replace_state_with_url(&state, "", Some(&url));
    }

    fn external(&self, url: String) -> bool {
        self.inner.external(url)
    }

    fn updater(&self, callback: std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.inner.updater(callback);
    }

    fn include_prevent_default(&self) -> bool {
        self.inner.include_prevent_default()
    }
}

/// The URL a route addresses: the router speaks in prefix-less routes, so a deployment served
/// under a base path has it put back on here.
fn full_path(prefix: Option<&str>, route: &str) -> String {
    match prefix {
        Some(prefix) => format!("{prefix}{route}"),
        None => route.to_owned(),
    }
}

pub(crate) fn now_ms() -> f64 {
    js_sys::Date::now()
}

pub(crate) fn parse_timestamp_ms(text: &str) -> f64 {
    js_sys::Date::parse(text)
}

pub(crate) fn format_timestamp_iso(ms: f64) -> Option<String> {
    js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms))
        .to_iso_string()
        .as_string()
}

pub(crate) fn local_hour() -> u32 {
    js_sys::Date::new_0().get_hours()
}

pub(crate) async fn sleep_ms(ms: u32) {
    gloo_timers::future::TimeoutFuture::new(ms).await;
}

pub(crate) fn preferred_language() -> Option<String> {
    web_sys::window()?.navigator().language()
}

pub(crate) fn origin() -> String {
    // Unlike the browser's own `fetch`, reqwest rejects a relative URL with a builder error, so
    // this needs the concrete origin. An empty base outside a browser is the honest answer.
    web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_default()
}

/// Built from a `Blob` + object URL + a synthetic anchor click, which is the only way to make a
/// browser save a document the app already holds in memory.
///
/// The object URL is revoked immediately after the click. The download has already been handed
/// to the browser at that point, and leaving it alive pins the blob for the lifetime of the
/// document — which, for a personal-data export, means keeping the reader's entire record in
/// memory until they navigate away.
#[expect(
    clippy::unused_async,
    reason = "the surface is async because the desktop side opens a file dialog; the browser \
              hands the download over synchronously"
)]
pub(crate) async fn save_text_file(
    filename: &str,
    mime: &str,
    contents: &str,
) -> Result<(), &'static str> {
    let failed = || "common.downloadRefused";

    let parts = js_sys::Array::new();
    parts.push(&wasm_bindgen::JsValue::from_str(contents));
    let options = web_sys::BlobPropertyBag::new();
    options.set_type(mime);
    let blob =
        web_sys::Blob::new_with_str_sequence_and_options(&parts, &options).map_err(|_| failed())?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| failed())?;

    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(failed)?;
    let anchor = document
        .create_element("a")
        .map_err(|_| failed())?
        .dyn_into::<web_sys::HtmlAnchorElement>()
        .map_err(|_| failed())?;
    anchor.set_href(&url);
    anchor.set_download(filename);
    anchor.click();

    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}

/// An `EventSource` and the named subscriptions taken from it.
///
/// The source is held as well as the subscriptions because closing the connection is the
/// caller's job and a subscription does not own it.
pub(crate) struct EventStream {
    source: gloo_net::eventsource::futures::EventSource,
    /// Merged rather than pumped in turn: the names arrive on their own cadences, and awaiting
    /// one at a time would stall the fast one behind the slow one's tick.
    events:
        futures_util::stream::SelectAll<gloo_net::eventsource::futures::EventSourceSubscription>,
}

impl EventStream {
    /// The next message as `(event name, data)`, or `None` once the stream ends.
    ///
    /// A transport error ends the stream rather than being reported: the ticket in the URL is
    /// spent, so `EventSource`'s own retry cannot succeed and the caller has to mint a new one.
    pub(crate) async fn next(&mut self) -> Option<(String, String)> {
        loop {
            let Ok((name, message)) = self.events.next().await? else {
                return None;
            };
            if let Some(text) = message.data().as_string() {
                return Some((name, text));
            }
        }
    }

    pub(crate) fn close(self) {
        self.source.close();
    }
}

#[expect(
    clippy::unused_async,
    reason = "the surface is async because the desktop side awaits the response headers; \
              `EventSource` connects in the background"
)]
pub(crate) async fn subscribe(url: &str, events: &[&str]) -> Option<EventStream> {
    let mut source = gloo_net::eventsource::futures::EventSource::new(url).ok()?;
    let mut merged = futures_util::stream::SelectAll::new();
    for event in events {
        let Ok(subscription) = source.subscribe(*event) else {
            source.close();
            return None;
        };
        merged.push(subscription);
    }
    Some(EventStream {
        source,
        events: merged,
    })
}

#[cfg(test)]
mod tests {
    use super::full_path;

    /// A replace builds its own URL here rather than going through `WebHistory`, and losing the
    /// deployment's base path is the way that goes wrong without anything failing: the address
    /// bar would read `/discover` under an app served from `/app`, and the reader would find out
    /// on the next reload.
    #[test]
    fn a_replace_keeps_the_deployment_prefix() {
        assert_eq!(full_path(None, "/discover?at=24"), "/discover?at=24");
        assert_eq!(
            full_path(Some("/app"), "/discover?at=24"),
            "/app/discover?at=24"
        );
    }
}
