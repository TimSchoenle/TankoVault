//! The deployment's own identity, published so the client renders it instead of a literal.
//!
//! **Unauthenticated on purpose**, for the same reason `/v1/legal` is: the sign-in card carries
//! the wordmark and the footer, and a client that has to authenticate before it learns what it is
//! called would show this project's name to the readers of a fork.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse as _, Response};
use serde::Serialize;
use tankovault_config::BrandingConfig;
use utoipa::ToSchema;

use crate::openapi::BRANDING_TAG;
use crate::state::AppState;

/// How long a client may reuse the branding without revalidating.
///
/// The same five minutes `/v1/legal` uses, and for the same reason: it changes only when the
/// operator restarts, but it is asked for on every page load, and an operator who has just
/// renamed their deployment should see it within a coffee break rather than an hour.
const CACHE_POLICY: &str = "public, max-age=300";

/// The resolved identity, held in [`AppState`].
///
/// Resolution happens once at boot — the wordmark split and the licence label do not depend on
/// the request — with the single exception of the copyright year, which is filled in per response
/// so a long-running deployment does not keep claiming the year it was started in.
#[derive(Clone)]
pub struct Branding(Arc<BrandingConfig>);

impl Branding {
    /// Resolves the operator's configuration once, at boot. Cloning the result is an `Arc`
    /// bump, which is what makes it cheap to hold in per-request state.
    #[must_use]
    pub fn new(config: BrandingConfig) -> Self {
        Self(Arc::new(config))
    }

    /// The product name, in prose. What email, page titles and the authenticator prompt use.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0.name
    }

    /// The identifiable crawler user-agent the operator configured, if any.
    #[must_use]
    pub fn bot_user_agent(&self) -> Option<&str> {
        self.0.bot_user_agent.as_deref()
    }

    /// The view served to clients.
    fn view(&self) -> BrandingView {
        let (lead, accent) = self.0.wordmark();
        BrandingView {
            name: self.0.name.clone(),
            wordmark: WordmarkView { lead, accent },
            tagline: self.0.tagline.clone(),
            copyright: CopyrightView {
                holder: self.0.copyright.holder.clone(),
                year: self.0.copyright.year.clone().unwrap_or_else(current_year),
                notice: self.0.copyright.notice.clone(),
            },
            licence: LicenceView {
                name: self.0.licence.name.clone(),
                url: self.0.licence.url.clone(),
            },
            project_url: self.0.project_url.clone(),
            releases_url: self.0.releases_url.clone(),
        }
    }
}

/// The current UTC year, as the footer would print it.
///
/// UTC rather than the reader's zone: a copyright notice is not a clock, and resolving it
/// per-reader would make the response uncacheable for a difference nobody can observe.
fn current_year() -> String {
    time::OffsetDateTime::now_utc().year().to_string()
}

/// What this deployment calls itself.
#[derive(Debug, Serialize, ToSchema)]
pub struct BrandingView {
    /// The product name in prose.
    pub name: String,
    /// The lockup drawn in the rail and the footer.
    pub wordmark: WordmarkView,
    /// An operator-supplied tagline that replaces the translated one. Absent keeps the
    /// catalogue's.
    pub tagline: Option<String>,
    /// The footer's copyright line.
    pub copyright: CopyrightView,
    /// How this deployment's code is licensed.
    pub licence: LicenceView,
    /// Where the project lives.
    pub project_url: String,
    /// Where the native client is downloaded.
    pub releases_url: String,
}

/// The two halves of the wordmark.
#[derive(Debug, Serialize, ToSchema)]
pub struct WordmarkView {
    /// Drawn in the body colour.
    pub lead: String,
    /// Drawn in the accent colour; absent draws the lockup as one word.
    pub accent: Option<String>,
}

/// The footer's copyright line.
#[derive(Debug, Serialize, ToSchema)]
pub struct CopyrightView {
    /// Who holds it.
    pub holder: String,
    /// The year or range, resolved to the current year when the operator set none.
    pub year: String,
    /// The whole notice verbatim, when the operator supplied one. Renders instead of composing
    /// `holder` and `year`.
    pub notice: Option<String>,
}

/// The licence label and where its text lives.
#[derive(Debug, Serialize, ToSchema)]
pub struct LicenceView {
    pub name: String,
    /// Absent renders the label as plain text rather than a dead link.
    pub url: Option<String>,
}

/// Read the deployment's branding
///
/// The name, wordmark, tagline, copyright and links the client renders. Unauthenticated: the
/// sign-in card shows all of it.
#[utoipa::path(
    get,
    path = "/v1/branding",
    tag = BRANDING_TAG,
    responses(
        (status = 200, description = "This deployment's identity", body = BrandingView),
    )
)]
pub async fn branding(State(state): State<AppState>) -> Response {
    ([(CACHE_CONTROL, CACHE_POLICY)], Json(state.branding.view())).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_year_resolves_rather_than_rendering_empty() {
        let view = Branding::new(BrandingConfig::default()).view();
        assert_eq!(view.copyright.year, current_year());
    }

    #[test]
    fn a_configured_year_is_served_verbatim() {
        let branding = Branding::new(BrandingConfig {
            copyright: tankovault_config::CopyrightConfig {
                year: Some("2024–2026".to_owned()),
                ..tankovault_config::CopyrightConfig::default()
            },
            ..BrandingConfig::default()
        });
        assert_eq!(branding.view().copyright.year, "2024–2026");
    }
}
