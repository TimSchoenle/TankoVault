//! The reader's own half of the adult-content gate: their stored decision and the age
//! attestation that has to precede it.
//!
//! The deployment's half is [`Feature::CatalogueAdultContent`], and these routes stay reachable
//! when it is off. That is deliberate: a reader may read and revise their preference either way,
//! it simply has no effect until an operator opens the gate. Hiding the preference behind the
//! flag would mean an operator turning it on silently exposes whatever readers had set months
//! ago, with no chance to review it first.

use crate::error::ApiResult;
use crate::openapi::ME_ACCOUNT_TAG;
use crate::state::{AppState, AuthUser};
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use tankovault_domain::Feature;
use time::OffsetDateTime;
use utoipa::ToSchema;

/// The caller's content settings and what the deployment currently allows.
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent facts the settings screen shows together, plus the resolved               answer: collapsing them would move the reasoning about *why* the gate is shut               into the client, which is the one place it must not be duplicated"
)]
#[derive(Debug, Serialize, ToSchema)]
pub struct ContentPrefsDto {
    /// The caller's stored preference. Independent of whether it currently has any effect.
    pub adult_opt_in: bool,
    /// Whether the account has ever confirmed it is of age.
    ///
    /// Drives the client's decision to ask: an account with this already `true` is changing a
    /// setting, not making a declaration, and re-prompting it would train readers to click
    /// through the one dialog that is supposed to mean something.
    pub age_attested: bool,
    /// When it attested, or `null` if it never has.
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>)]
    pub age_attested_at: Option<OffsetDateTime>,
    /// Whether this deployment permits adult content at all.
    ///
    /// Published so the client can explain an opted-in reader's empty shelf as an operator
    /// decision rather than leaving it looking like the preference failed to save.
    pub allowed_by_deployment: bool,
    /// The resolved answer: whether adult series are actually being shown to this caller.
    ///
    /// The conjunction of everything above, computed server-side. A client recombining the
    /// parts itself is a second implementation of the gate, and the two will disagree.
    pub effective: bool,
}

/// A change to the caller's content settings.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ContentPrefsUpdate {
    /// The preference to store.
    pub adult_opt_in: bool,
    /// The caller's confirmation, in this request, that they are of age.
    ///
    /// Required to turn [`Self::adult_opt_in`] on for an account that has never attested;
    /// ignored when it already has, and ignored when opting out. It is a declaration the reader
    /// makes, so it is a field on the request rather than something the server infers from
    /// having asked.
    #[serde(default)]
    pub confirm_age: bool,
}

/// Get content preferences
///
/// The caller's adult-content decision, whether they have attested their age, and whether this
/// deployment permits adult content at all.
#[utoipa::path(
    get,
    path = "/v1/me/content-prefs",
    tag = ME_ACCOUNT_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The caller's content settings", body = ContentPrefsDto),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
    )
)]
pub async fn content_prefs(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<ContentPrefsDto>> {
    let prefs = tankovault_db::repo::users::get_content_prefs(&state.pool, user.user_id).await?;
    Ok(Json(render(&state, prefs)))
}

/// Replace content preferences
///
/// Stores the caller's adult-content decision. Turning it on requires `confirm_age` unless the
/// account has attested before; the attestation is recorded once and kept across later changes,
/// so opting out and back in does not ask again.
///
/// A `409` means the opt-in was refused for want of an attestation. The preference is unchanged
/// — this endpoint does not partially apply.
#[utoipa::path(
    put,
    path = "/v1/me/content-prefs",
    tag = ME_ACCOUNT_TAG,
    request_body = ContentPrefsUpdate,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The stored settings", body = ContentPrefsDto),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 409, description = "opting in requires an age attestation", body = crate::error::ProblemDetails),
    )
)]
pub async fn put_content_prefs(
    State(state): State<AppState>,
    user: AuthUser,
    Json(body): Json<ContentPrefsUpdate>,
) -> ApiResult<Json<ContentPrefsDto>> {
    let before = tankovault_db::repo::users::get_content_prefs(&state.pool, user.user_id).await?;
    let prefs = tankovault_db::repo::users::set_content_prefs(
        &state.pool,
        user.user_id,
        body.adult_opt_in,
        body.confirm_age,
    )
    .await?;

    // Audited because it is a consent record, not a display setting. If a reader ever disputes
    // having declared their age, the audit trail is the only thing that can answer, and it has
    // to carry when the declaration was made and from where. `first_attestation` marks the one
    // request that established it, since every later change echoes the same stored timestamp.
    if prefs.adult_opt_in != before.adult_opt_in {
        // The shelf is cached per reader and keyed on their taste profile, which a content
        // decision does not move. Without this, opting out leaves the previous shelf — adult
        // recommendations included — being served until the TTL expires, which is the one
        // moment a reader is most likely to be watching for the change to take effect.
        tankovault_db::repo::recsys::clear_shelf(&state.pool, user.user_id).await?;
    }

    if prefs.adult_opt_in != before.adult_opt_in || prefs.age_attested_at != before.age_attested_at
    {
        state
            .audit
            .record(
                user.event("content.adult_prefs_changed")
                    .detail(serde_json::json!({
                        "adult_opt_in": prefs.adult_opt_in,
                        "was_opted_in": before.adult_opt_in,
                        "first_attestation": before.age_attested_at.is_none()
                            && prefs.age_attested_at.is_some(),
                    })),
            )
            .await;
    }

    Ok(Json(render(&state, prefs)))
}

/// Combine the stored preference with the deployment flag into the answer the client renders.
fn render(state: &AppState, prefs: tankovault_db::repo::users::ContentPrefs) -> ContentPrefsDto {
    let allowed_by_deployment = state.features.is_enabled(Feature::CatalogueAdultContent);
    ContentPrefsDto {
        adult_opt_in: prefs.adult_opt_in,
        age_attested: prefs.age_attested_at.is_some(),
        age_attested_at: prefs.age_attested_at,
        allowed_by_deployment,
        effective: allowed_by_deployment && prefs.adult_opt_in,
    }
}
