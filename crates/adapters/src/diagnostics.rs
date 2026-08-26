//! Rendering a fetch response into a bounded, single-line log entry for adapter errors.

use tankovault_fetch::FetchResponse;

/// Characters of the body quoted back in an error — enough to recognise what arrived.
const DIAGNOSTIC_CHARS: usize = 240;

/// How much of the tail is searched for the document's closing tag.
///
/// A complete document closes in its last line; nothing that reaches an adapter carries
/// kilobytes after `</html>`.
const TAIL_SCAN_BYTES: usize = 4096;

/// The response envelope *minus* its URL and status, for errors that already name both.
pub(crate) fn envelope(resp: &FetchResponse) -> String {
    let unterminated = if looks_unterminated(&resp.body) {
        " unterminated_html=true"
    } else {
        ""
    };
    format!(
        "content_type={:?} bytes={} from_cache={}{unterminated} body_prefix={:?}",
        resp.header("content-type").unwrap_or("<none>"),
        resp.body.len(),
        resp.from_cache,
        body_prefix(&resp.body),
    )
}

/// The full response envelope, for errors whose own message names neither URL nor status.
pub(crate) fn describe(resp: &FetchResponse) -> String {
    format!("url={} status={} {}", resp.url, resp.status, envelope(resp))
}

/// The reason phrase for `status`, so a log line reads `404 Not Found` rather than `404`.
///
/// Covers the statuses a crawl actually meets and falls back to the class name, which is the
/// part that decides what happens next: a `4xx` is our request, a `5xx` is their server.
pub(crate) fn http_reason(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        410 => "Gone",
        429 => "Too Many Requests",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        other => match other / 100 {
            3 => "Redirection",
            4 => "Client Error",
            5 => "Server Error",
            _ => "Unexpected Status",
        },
    }
}

/// Whether an HTML document opened and never closed — the shape a cut transfer leaves.
///
/// Named in the envelope rather than failed at the fetch, because `</html>` is optional in HTML
/// and a minifier may drop it: this is the note that stops the *next* truncation being read as
/// a redesign, not a rule about what a page must contain. A selector error on a body flagged
/// here is a transport question, and the byte count says where the document stopped.
fn looks_unterminated(body: &str) -> bool {
    let head = body[..char_boundary_at_or_below(body, 512)].to_ascii_lowercase();
    if !head.contains("<html") && !head.contains("<!doctype html") {
        return false;
    }
    let from = char_boundary_at_or_below(body, body.len().saturating_sub(TAIL_SCAN_BYTES));
    !body[from..].to_ascii_lowercase().contains("</html")
}

/// The largest char boundary of `s` at or below `at`, so a slice never splits a code point.
fn char_boundary_at_or_below(s: &str, at: usize) -> usize {
    let mut cut = at.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

/// A bounded, whitespace-collapsed prefix of `body`, so one failure is one log line.
fn body_prefix(body: &str) -> String {
    let mut out = String::new();
    let mut written = 0usize;
    let mut gap = false;
    for ch in body.trim().chars() {
        if written >= DIAGNOSTIC_CHARS {
            out.push('…');
            break;
        }
        if ch.is_whitespace() {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push(' ');
            written += 1;
        }
        gap = false;
        out.push(ch);
        written += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(body: &str) -> FetchResponse {
        FetchResponse {
            status: 404,
            url: "https://example.test/manga/x/".to_owned(),
            headers: vec![("content-type".to_owned(), "text/html".to_owned())],
            body: body.to_owned(),
            from_cache: false,
        }
    }

    #[test]
    fn body_prefix_is_bounded_and_collapsed() {
        let prefix = body_prefix(&format!("  <html>\n\n  {}  ", "x".repeat(1_000)));
        assert!(
            prefix.chars().count() <= DIAGNOSTIC_CHARS + 1,
            "prefix must stay within the cap (plus the ellipsis): {prefix}"
        );
        assert!(
            prefix.starts_with("<html> x"),
            "runs of whitespace collapse"
        );
        assert!(prefix.ends_with('…'), "truncation is marked");
    }

    #[test]
    fn envelope_names_the_body_without_repeating_url_or_status() {
        let described = envelope(&resp("<!DOCTYPE html><title>Not Found</title>"));
        assert!(described.contains("content_type=\"text/html\""));
        assert!(described.contains("Not Found"), "body is quoted back");
        assert!(
            !described.contains("example.test"),
            "the URL belongs to the error message, not the envelope"
        );
    }

    #[test]
    fn describe_adds_url_and_status() {
        let described = describe(&resp("{}"));
        assert!(described.contains("url=https://example.test/manga/x/"));
        assert!(described.contains("status=404"));
    }

    /// A document cut mid-transfer is flagged, so the selector it fails is read as a symptom.
    ///
    /// The bug this pins: an origin flushing gzip per member answered with a `200` carrying
    /// only its `<head>`, and the only thing the operator saw was
    /// `required element not found: series title` against markup the site had not touched.
    #[test]
    fn a_document_that_never_closes_is_flagged() {
        let cut = "<!DOCTYPE html><html lang=\"en\"><head><title>Series</title><style>.a{colo";
        assert!(envelope(&resp(cut)).contains("unterminated_html=true"));
        assert!(
            !envelope(&resp(&format!(
                "{cut}r:red}}</style></head><body></body></html>"
            )))
            .contains("unterminated_html"),
            "a closed document carries no flag"
        );
    }

    /// Only HTML is judged: a JSON body and a sitemap have no `</html>` to miss.
    #[test]
    fn only_html_bodies_are_judged_unterminated() {
        assert!(!looks_unterminated("{\"data\":{\"chapters\":[]}}"));
        assert!(!looks_unterminated(
            "<?xml version=\"1.0\"?><urlset><url><loc>https://x.test/</loc></url></urlset>"
        ));
    }

    #[test]
    fn reason_falls_back_to_the_status_class() {
        assert_eq!(http_reason(404), "Not Found");
        assert_eq!(http_reason(418), "Client Error");
        assert_eq!(http_reason(599), "Server Error");
    }
}
