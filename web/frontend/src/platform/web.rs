//! The browser implementation of [`crate::platform`], typed through `web-sys`.
//!
//! Read the module contract in `mod.rs` before adding anything here — in particular, why none of
//! this may be written as `document::eval`.

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
