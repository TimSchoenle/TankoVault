//! Rendering a fetch response into a bounded, single-line log entry for adapter errors.

use tankovault_fetch::FetchResponse;

/// Characters of the body quoted back in an error — enough to recognise what arrived.
const DIAGNOSTIC_CHARS: usize = 240;

/// The response envelope *minus* its URL and status, for errors that already name both.
pub(crate) fn envelope(resp: &FetchResponse) -> String {
    format!(
        "content_type={:?} bytes={} from_cache={} body_prefix={:?}",
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

    #[test]
    fn reason_falls_back_to_the_status_class() {
        assert_eq!(http_reason(404), "Not Found");
        assert_eq!(http_reason(418), "Client Error");
        assert_eq!(http_reason(599), "Server Error");
    }
}
