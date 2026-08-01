//! Reading JSON API responses that may arrive as raw JSON, JSON re-serialised inside
//! solver-rendered markup, or an unsolved interstitial; [`parse_json_body`] recovers the first
//! two and names the third instead of reporting it as malformed JSON.

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
    // Best-first order: report the first failure, since a later one says less about what arrived.
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
/// `<pre>` blocks are tried first since that is the original text, not a re-rendering; the scan
/// then widens to the raw document and finally to entity-decoded text content, which recovers a
/// payload a JSON viewer pretty-printed into per-token elements.
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
///
/// Single-pass and linear in body length — re-scanning to the end of the document from every
/// `{` is a remotely triggerable quadratic DoS given the fetch stack's multi-MiB body cap.
fn collect_objects(doc: &str, out: &mut Vec<String>) {
    /// How many candidate openings to track at once — bounds the working set against a
    /// hostile body while staying above [`MAX_CANDIDATES`].
    const TRACKED_OPENS: usize = MAX_CANDIDATES * 8;

    let bytes = doc.as_bytes();
    // (start, end_inclusive) of each balanced candidate, gathered in one pass.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    // Openings still waiting for their closing brace, as (nesting depth, byte offset).
    let mut pending: Vec<(usize, usize)> = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (i, &b) in bytes.iter().enumerate() {
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
            b'{' => {
                depth += 1;
                // Only object-opening braces (key or empty object next) count as candidates —
                // keeps the scan off JS blocks and CSS rules in the rendered page.
                if pending.len() < TRACKED_OPENS && opens_object(bytes, i) {
                    pending.push((depth, i));
                }
            }
            b'}' => {
                if depth == 0 {
                    continue;
                }
                if let Some(&(open_depth, start)) = pending.last()
                    && open_depth == depth
                {
                    pending.pop();
                    spans.push((start, i));
                }
                depth -= 1;
            }
            _ => {}
        }
    }

    // Document order, which for nested objects is outermost first.
    spans.sort_unstable();
    for (start, end) in spans {
        if out.len() >= MAX_CANDIDATES {
            return;
        }
        out.push(doc[start..=end].to_owned());
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

/// Turn an exhausted parse into the most specific error the response supports.
fn failure(what: &str, resp: &FetchResponse, cause: Option<&serde_json::Error>) -> AdapterError {
    if let Some(kind) = detect_challenge_body(&resp.body) {
        return AdapterError::Challenged {
            url: resp.url.clone(),
            kind,
        };
    }
    // Defence in depth: only reached when nothing upstream reported the rate limit honestly;
    // calling the page malformed JSON here would repeat that same misclassification.
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

    /// Regression: `collect_objects` used to re-scan to end-of-document from every `{`, which
    /// is quadratic and remotely triggerable given the fetch stack's body-size cap.
    #[test]
    fn collect_objects_is_linear_in_body_length() {
        let depth = 60_000;
        let doc = format!(
            "{}{}",
            r#"{"a":"#.repeat(depth),
            "1".to_owned() + &"}".repeat(depth)
        );
        let started = std::time::Instant::now();
        let mut out = Vec::new();
        collect_objects(&doc, &mut out);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "quadratic scan regressed: {:?}",
            started.elapsed()
        );
        assert_eq!(out.len(), MAX_CANDIDATES);
    }

    #[test]
    fn collect_objects_yields_outermost_first() {
        let mut out = Vec::new();
        collect_objects(r#"prefix {"a":{"b":1}} suffix {"c":2}"#, &mut out);
        assert_eq!(
            out,
            vec![
                r#"{"a":{"b":1}}"#.to_owned(),
                r#"{"b":1}"#.to_owned(),
                r#"{"c":2}"#.to_owned(),
            ]
        );
    }

    /// Braces inside string values must not move the depth, and an unterminated object must
    /// not swallow the objects that follow it.
    #[test]
    fn collect_objects_is_string_aware_and_tolerates_unbalanced_input() {
        let mut out = Vec::new();
        collect_objects(r#"{"a":"} { \" {"} then {"b":2}"#, &mut out);
        assert_eq!(
            out,
            vec![r#"{"a":"} { \" {"}"#.to_owned(), r#"{"b":2}"#.to_owned()]
        );

        let mut unbalanced = Vec::new();
        collect_objects(r#"{"never":"closed" [ {"ok":1}"#, &mut unbalanced);
        assert_eq!(unbalanced, vec![r#"{"ok":1}"#.to_owned()]);
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
        // A JSON viewer pretty-prints the payload into spans; stripping markup recovers it.
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
        // A rendered "Too Many Requests" notice under a success status.
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
