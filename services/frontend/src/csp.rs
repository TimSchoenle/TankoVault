//! The SPA's Content Security Policy: built from the shell's inline scripts, and sent with every
//! response.

use std::path::Path;
use std::sync::Arc;

use axum::http::HeaderValue;
use csp_shell::presets::cloudflare;
use csp_shell::{Csp, Policy};
use tankovault_frontend::config::CloudflareConfig;

/// Build the Content-Security-Policy for the SPA shell.
///
/// The access token lives only in memory; this CSP is the ceiling on where an injected script
/// could send it. [`Csp::spa_wasm`] is the policy this service's own hand-rolled one was
/// extracted into: no `'unsafe-eval'` — `'wasm-unsafe-eval'` covers WebAssembly instantiation and
/// nothing here calls Dioxus's `document::eval` (banned; its web impl is `new Function(…)`, see
/// `web/frontend/src/platform/`) — `connect-src 'self'` as the exfiltration ceiling, and any
/// `https:`/`data:` host in `img-src` for provider-sourced cover art.
///
/// The `'sha256-…'` entries admit the shell's inline boot scripts, scanned from the served shell
/// at startup rather than hardcoded: a constant drifts the moment the shell is edited, and the
/// browser then refuses the script with nothing surfaced server-side.
///
/// `connect-src` already covers Cloudflare's `/cdn-cgi/challenge-platform/` beacons and
/// `script-src 'self'` its scripts, since both are served from this origin — the flags in
/// [`CloudflareConfig`] exist for the two things that are *not* reachable from `'self'`.
pub(super) fn content_security_policy(
    shell: Option<&str>,
    shell_path: &Path,
    config: &CloudflareConfig,
) -> Policy {
    let mut csp = Csp::spa_wasm();
    if let Some(source) = shell {
        let scan = csp_shell::scan_shell(source);
        // The scanner is deliberately not an HTML parser. Anything it accepted but could not
        // read exactly yields a hash no browser computes, so the limit is logged rather than
        // left as a blank page with nothing to attribute it to.
        for warning in &scan.warnings {
            tracing::warn!(
                shell = %shell_path.display(),
                %warning,
                "the app shell hit a documented scanner limit; its hashes may not match"
            );
        }
        csp = csp.with_scan(&scan);
    }
    if config.turnstile {
        csp = cloudflare::turnstile(csp);
    }
    if config.script_nonce {
        csp = cloudflare::script_nonce(csp);
    }
    csp.build()
}

/// Supplies the `Content-Security-Policy` value for each static response.
///
/// Two shapes because a deployment that is not behind Cloudflare has no nonce slot reserved, and
/// its header is then a constant: rendering it once at startup keeps the per-response cost at a
/// `HeaderValue` clone, and only the nonce case pays a render per response.
#[derive(Clone)]
pub(super) enum CspHeader {
    Constant(HeaderValue),
    PerResponse(Arc<Policy>),
}

impl CspHeader {
    pub(super) fn new(policy: Policy) -> Self {
        if policy.is_per_response() {
            Self::PerResponse(Arc::new(policy))
        } else {
            Self::Constant(header_value(&policy))
        }
    }
}

impl<T> tower_http::set_header::MakeHeaderValue<T> for CspHeader {
    fn make_header_value(&mut self, _response: &T) -> Option<HeaderValue> {
        Some(match self {
            Self::Constant(value) => value.clone(),
            Self::PerResponse(policy) => header_value(policy),
        })
    }
}

/// Render one response's policy.
pub(super) fn header_value(policy: &Policy) -> HeaderValue {
    #[expect(
        clippy::unneeded_wildcard_pattern,
        reason = "naming `cache_control` turns a rename of the field this obligation rests on into a compile error"
    )]
    let csp_shell::Headers {
        content_security_policy,
        // A nonce served from a stored copy is pinned across every reader. `FixedResponse::shell`
        // meets this with `no-store` and no validator; `no-cache` is not enough, because a `304`
        // pairs one response's nonce with another's body (`a_nonced_shell_is_served_uncacheable`).
        cache_control: _,
        // Cloudflare reads the nonce out of the response header, so nothing is stamped into the
        // shell and this service never needs the value itself.
        ..
    } = policy.headers();

    // `csp-policy` refuses any term that could carry a separator or a control character into the
    // rendered header, so this cannot fail; a restrictive fallback still beats refusing to serve.
    HeaderValue::from_str(&content_security_policy)
        .unwrap_or_else(|_| HeaderValue::from_static("default-src 'self'"))
}
