//! Reading JSON API responses that do not always arrive as JSON.
//!
//! A provider's XHR endpoint is fetched through the same stack as its pages, so one of three
//! things comes back:
//!
//! 1. **Raw JSON** — a plain fetch that was not challenged.
//! 2. **JSON re-serialised inside rendered markup** — a challenge solver returns what the
//!    headless browser *displayed*, so the payload arrives inside a `<pre>` block, possibly
//!    entity-escaped, possibly interleaved with a browser JSON viewer's own markup.
//! 3. **Somebody else's page** — an interstitial the fetch stack could not solve away.
//!
//! [`parse_json_body`] recovers (1) and (2), and names (3) for what it is instead of
//! reporting it as malformed JSON. When it does fail, the error carries the response
//! envelope — status, content type, size, and a bounded prefix of the body — because
//! "could not parse" without the thing that could not be parsed is not a diagnosis.

use crate::diagnostics::describe;
use crate::error::AdapterError;
use crate::html::unescape_entities;
use serde::de::DeserializeOwned;
use tankovault_fetch::FetchResponse;
use tankovault_solver::{detect_challenge_body, is_rate_limit_page};

/// Upper bound on JSON candidates extracted from one body. A document embedding more
/// distinct objects than this is not a wrapped API response, and scanning on would cost more
/// than it could recover.
const MAX_CANDIDATES: usize = 8;

/// Deserialize `T` from a response whose body is JSON, or contains JSON.
///
/// `what` names the endpoint in error messages (e.g. `"kunmanga chapters API"`).
///
/// # Errors
/// - [`AdapterError::Challenged`] if the body is a bot-management interstitial.
/// - [`AdapterError::Parse`], carrying the response envelope, if no candidate deserialized.
pub(crate) fn parse_json_body<T: DeserializeOwned>(
    what: &str,
    resp: &FetchResponse,
) -> Result<T, AdapterError> {
    // The first failure is the one worth reporting: candidates are ordered best-first, so a
    // later one failing says less about what the provider actually sent.
    let mut cause: Option<serde_json::Error> = None;

    // Fast path: an unwrapped JSON body, parsed without copying it first.
    let trimmed = resp.body.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        match serde_json::from_str::<T>(trimmed) {
            Ok(value) => return Ok(value),
            Err(e) => {
                cause.get_or_insert(e);
            }
        }
    }

    for candidate in wrapped_candidates(&resp.body) {
        match serde_json::from_str::<T>(candidate.trim()) {
            Ok(value) => return Ok(value),
            Err(e) => {
                cause.get_or_insert(e);
            }
        }
    }

    Err(failure(what, resp, cause.as_ref()))
}

/// JSON objects embedded in rendered markup, best candidate first.
///
/// `<pre>` blocks come first because that is where a browser puts a document it had no
/// renderer for — the payload there is the original text, not a re-rendering of it. Failing
/// that the scan widens to the document as received, and finally to its text content with
/// entities decoded, which is what recovers a payload a JSON viewer pretty-printed into
/// per-token elements.
fn wrapped_candidates(body: &str) -> Vec<String> {
    let mut out = Vec::new();

    for block in pre_blocks(body) {
        collect_objects(&unescape_entities(&strip_tags(block)), &mut out);
    }
    collect_objects(body, &mut out);
    collect_objects(&unescape_entities(&strip_tags(body)), &mut out);

    out
}

/// The inner text of every `<pre …>…</pre>` element, in document order.
fn pre_blocks(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for tail in body.split("<pre").skip(1) {
        let Some(open_end) = tail.find('>') else {
            continue;
        };
        let inner = &tail[open_end + 1..];
        let end = inner.find("</pre").unwrap_or(inner.len());
        out.push(&inner[..end]);
    }
    out
}

/// Drop element markup, keeping text content.
///
/// Only correct for text a DOM produced — where a literal `<` in the payload is necessarily
/// escaped as `&lt;` — which is exactly the case this module handles.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for ch in s.chars() {
        match ch {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Append every balanced JSON object found in `doc` (outermost first) to `out`, stopping at
/// [`MAX_CANDIDATES`].
fn collect_objects(doc: &str, out: &mut Vec<String>) {
    let bytes = doc.as_bytes();
    for (start, _) in doc.match_indices('{') {
        if out.len() >= MAX_CANDIDATES {
            return;
        }
        // Only braces that open an object with a key (or an empty object) are plausible API
        // payloads; this is what keeps the scan off every JavaScript block and CSS rule in a
        // rendered page.
        if !opens_object(bytes, start) {
            continue;
        }
        if let Some(span) = balanced_span(doc, start) {
            out.push(span.to_owned());
        }
    }
}

/// Whether the byte after the brace at `start` (ignoring whitespace) starts a key or closes
/// an empty object.
fn opens_object(bytes: &[u8], start: usize) -> bool {
    bytes
        .iter()
        .skip(start + 1)
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|b| matches!(b, b'"' | b'}'))
}

/// The slice from the brace at `start` to the brace that closes it, or `None` if it never
/// closes.
///
/// String-aware, so braces inside JSON string values do not shift the depth. Scanning bytes
/// is safe for the same reason it is fast: every delimiter is ASCII, and no byte of a
/// multi-byte UTF-8 sequence can collide with one.
fn balanced_span(doc: &str, start: usize) -> Option<&str> {
    let bytes = doc.as_bytes();
    if bytes.get(start) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&doc[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Turn an exhausted parse into the most specific error the response supports.
fn failure(what: &str, resp: &FetchResponse, cause: Option<&serde_json::Error>) -> AdapterError {
    if let Some(kind) = detect_challenge_body(&resp.body) {
        return AdapterError::Challenged {
            url: resp.url.clone(),
            kind,
        };
    }
    // Defence in depth: the fetch stack turns a rate-limit notice into a `429` before it gets
    // here, but only if *something* upstream reported a status honestly. When nothing did,
    // the page is still the page, and calling it malformed JSON would be a third layer
    // repeating the same mistake.
    if is_rate_limit_page(&resp.body) {
        return AdapterError::Throttled {
            url: resp.url.clone(),
        };
    }
    // Distinguishing "the JSON is not what we expect" from "there is no JSON here" is the
    // difference between a contract change and a blocked fetch — two different fixes.
    let detail = match cause {
        Some(e) => format!("response did not match the expected shape ({e})"),
        None => "response contained no JSON object".to_owned(),
    };
    AdapterError::Parse(format!("{what}: {detail}; {}", describe(resp)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct Envelope {
        data: Payload,
    }

    #[derive(Debug, Deserialize)]
    struct Payload {
        items: Vec<u32>,
    }

    fn resp(body: &str) -> FetchResponse {
        FetchResponse {
            status: 200,
            url: "https://example.test/api/items".to_owned(),
            headers: vec![("content-type".to_owned(), "text/html".to_owned())],
            body: body.to_owned(),
            from_cache: false,
        }
    }

    #[test]
    fn parses_a_raw_json_body() {
        let env: Envelope = parse_json_body("api", &resp(r#"{"data":{"items":[1,2]}}"#)).unwrap();
        assert_eq!(env.data.items, vec![1, 2]);
    }

    #[test]
    fn parses_json_a_browser_rendered_into_a_pre_block() {
        let body = concat!(
            "<html><head><meta name=\"color-scheme\" content=\"light dark\"></head><body>",
            "<pre style=\"word-wrap: break-word;\">{\"data\":{\"items\":[7]}}</pre>",
            "</body></html>"
        );
        let env: Envelope = parse_json_body("api", &resp(body)).unwrap();
        assert_eq!(env.data.items, vec![7]);
    }

    #[test]
    fn parses_entity_escaped_json() {
        let body = "<pre>{&quot;data&quot;:{&quot;items&quot;:[3]}}</pre>";
        let env: Envelope = parse_json_body("api", &resp(body)).unwrap();
        assert_eq!(env.data.items, vec![3]);
    }

    #[test]
    fn parses_json_split_across_viewer_markup() {
        // A browser JSON viewer pretty-prints the payload into spans; the text content still
        // reconstructs the document, so stripping the markup recovers it.
        let body = concat!(
            "<div class=\"json-formatter\"><span class=\"brace\">{</span>",
            "<span class=\"key\">&quot;data&quot;</span>:<span class=\"brace\">{</span>",
            "<span class=\"key\">&quot;items&quot;</span>:[<span class=\"num\">5</span>]",
            "<span class=\"brace\">}</span><span class=\"brace\">}</span></div>"
        );
        let env: Envelope = parse_json_body("api", &resp(body)).unwrap();
        assert_eq!(env.data.items, vec![5]);
    }

    #[test]
    fn ignores_script_and_style_braces_before_the_payload() {
        let body = concat!(
            "<html><head><style>body{margin:0}</style>",
            "<script>if (window.x) { window.y = 1; }</script></head>",
            "<body><pre>{\"data\":{\"items\":[9]}}</pre></body></html>"
        );
        let env: Envelope = parse_json_body("api", &resp(body)).unwrap();
        assert_eq!(env.data.items, vec![9]);
    }

    #[test]
    fn braces_inside_strings_do_not_end_the_object() {
        let body = r#"<pre>{"data":{"items":[1]},"note":"a } brace \" in a string"}</pre>"#;
        let env: Envelope = parse_json_body("api", &resp(body)).unwrap();
        assert_eq!(env.data.items, vec![1]);
    }

    #[test]
    fn a_challenge_page_is_reported_as_a_challenge_not_a_parse_failure() {
        let body = "<html><head><title>Just a moment...</title></head><body>\
                    <script src=\"/cdn-cgi/challenge-platform/h/b/orchestrate\"></script>\
                    </body></html>";
        let err = parse_json_body::<Envelope>("api", &resp(body)).unwrap_err();
        assert!(
            matches!(err, AdapterError::Challenged { .. }),
            "expected a challenge, got {err}"
        );
    }

    #[test]
    fn a_rate_limit_page_is_reported_as_throttling_not_a_parse_failure() {
        // The exact shape that reached production: a rendered "Too Many Requests" notice
        // carrying a success status, because everything upstream of here had been told 200.
        let body = "<html lang=\"en\"><head> <meta charset=\"utf-8\"> \
                    <title>Too Many Requests</title> </head><body></body></html>";
        let err = parse_json_body::<Envelope>("api", &resp(body)).unwrap_err();
        assert!(
            matches!(err, AdapterError::Throttled { .. }),
            "expected throttling, got {err}"
        );
        assert!(err.is_transient(), "a throttled fetch is worth retrying");
    }

    #[test]
    fn a_shape_mismatch_names_the_missing_field() {
        let err = parse_json_body::<Envelope>("api", &resp(r#"{"success":false}"#)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("did not match the expected shape"), "{msg}");
        assert!(msg.contains("data"), "{msg}");
    }

    #[test]
    fn a_body_without_json_reports_the_response_envelope() {
        let err =
            parse_json_body::<Envelope>("api", &resp("<h1>502 Bad Gateway</h1>")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("contained no JSON object"), "{msg}");
        assert!(msg.contains("status=200"), "{msg}");
        assert!(msg.contains("https://example.test/api/items"), "{msg}");
        assert!(msg.contains("502 Bad Gateway"), "{msg}");
    }

    #[test]
    fn the_quoted_prefix_is_bounded_and_single_line() {
        let body = format!("<h1>\n  nope\n</h1>{}", "x".repeat(4096));
        let err = parse_json_body::<Envelope>("api", &resp(&body)).unwrap_err();
        let msg = err.to_string();
        assert!(!msg.contains('\n'), "{msg}");
        assert!(msg.len() < 700, "diagnostic too long: {}", msg.len());
    }
}
