//! Provider CRUD, re-solve, per-provider stats, and the dry-run adapter test.

use super::scans::TriggerScan;
use crate::audit::audit;
use crate::error::{ApiError, ApiResult};
use crate::openapi::ADMIN_PROVIDERS_TAG;
use crate::state::{AppState, AuthUser};
use crate::views::IntoView;
use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde::Serialize;
use tankovault_db::repo::providers::NewProvider;
use tankovault_domain::{
    AdapterKind, Permission, Politeness, PolitenessInput, PresetDefinition, Provider, ProviderId,
    ProviderState, ScanMode,
};
use utoipa::ToSchema;

/// List providers
#[utoipa::path(
    get,
    path = "/v1/admin/providers",
    tag = ADMIN_PROVIDERS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "All providers", body = Vec<Provider>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_providers(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<Provider>>> {
    user.require(Permission::ProvidersRead).await?;
    Ok(Json(
        tankovault_db::repo::providers::list(&state.pool).await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateProvider {
    pub slug: String,
    pub name: String,
    pub base_url: String,
    pub adapter: AdapterKind,
    #[serde(default = "empty_object")]
    pub config: serde_json::Value,
    #[serde(default)]
    pub politeness: Option<PolitenessInput>,
}

/// The crawl budget a request settles on: the block it sent, or the server's defaults when it
/// sent none.
///
/// The two absences are different statements. No `emulation` key *inside* a block means "no
/// emulation" — that is what lets the console turn impersonation off, since a generated client
/// omits a `None` field rather than sending `null`. No block at all means the caller is not
/// specifying a crawl budget, and the registration form deliberately never guesses one.
fn resolve_politeness(sent: Option<PolitenessInput>) -> Politeness {
    sent.map_or_else(Politeness::default, Into::into)
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

/// Refuses a `base_url` the crawler must not reach — persisted, so a limited role could
/// otherwise point the scheduled scanner at internal hosts indefinitely, not just once.
async fn validate_base_url(base_url: &str) -> ApiResult<()> {
    tankovault_domain::ssrf::validate_and_resolve(base_url)
        .await
        .map_err(|e| {
            tracing::warn!(url = %base_url, error = %e, "refused a provider base_url");
            ApiError::BadRequest(format!(
                "base_url {base_url:?} is not an allowed target: {e}"
            ))
        })
}

/// Create a provider
#[utoipa::path(
    post,
    path = "/v1/admin/providers",
    tag = ADMIN_PROVIDERS_TAG,
    request_body = CreateProvider,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Created", body = Provider),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 400, description = "Provider slug is not a legal bus token", body = crate::error::ProblemDetails),
        (status = 409, description = "Provider slug already exists", body = crate::error::ProblemDetails),
    )
)]
pub async fn create_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateProvider>,
) -> ApiResult<Json<Provider>> {
    user.require(Permission::ProvidersCreate).await?;
    // The slug becomes the NATS task subject; one NATS rejects yields a provider whose scans
    // are planned but never run.
    if !tankovault_contracts::is_valid_provider_slug(&req.slug) {
        return Err(ApiError::BadRequest(format!(
            "provider slug {:?} must be non-empty and contain only letters, digits, '-' or '_'",
            req.slug
        )));
    }
    validate_base_url(&req.base_url).await?;
    let provider = tankovault_db::repo::providers::create(
        &state.pool,
        NewProvider {
            slug: req.slug,
            name: req.name,
            base_url: req.base_url,
            adapter: req.adapter,
            config: req.config,
            politeness: resolve_politeness(req.politeness),
            // Never preset-managed, and that includes the console's "clone" — which posts here
            // with another provider's fields. A copy is the operator's from the moment it
            // exists; only the installer creates rows that follow a preset.
            preset_slug: None,
        },
    )
    .await?;

    audit(
        &state,
        &user,
        "provider.create",
        &provider.id.to_string(),
        &serde_json::json!({
            "slug": provider.slug,
            "base_url": provider.base_url,
        }),
    )
    .await;

    Ok(Json(provider))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateProvider {
    pub name: String,
    pub base_url: String,
    #[serde(default = "empty_object")]
    pub config: serde_json::Value,
    #[serde(default)]
    pub politeness: Option<PolitenessInput>,
}

/// Refuse an edit to a preset-owned field of a provider that still follows its preset.
///
/// The next install run would overwrite the edit anyway, so accepting it would mean storing a
/// change that quietly disappears at the next rollout. Politeness is deliberately absent from
/// the comparison: it is not preset-owned, and tuning a crawl budget on a managed provider is
/// meant to work without unlocking anything.
///
/// The console disables these inputs, so reaching this is either a direct API call or a stale
/// tab — both of which are exactly why the rule lives here and not only in the UI.
///
/// # Errors
/// `Conflict` (409) naming the fields that would have been overwritten.
fn refuse_locked_edit(before: &Provider, req: &UpdateProvider) -> ApiResult<()> {
    if !before.preset.as_ref().is_some_and(|link| link.locked) {
        return Ok(());
    }
    let mut owned = Vec::new();
    if before.name != req.name {
        owned.push("name");
    }
    if before.base_url != req.base_url {
        owned.push("base_url");
    }
    if before.config != req.config {
        owned.push("config");
    }
    if owned.is_empty() {
        return Ok(());
    }
    Err(ApiError::Conflict(format!(
        "provider {} follows its built-in preset; unlock it before editing {}",
        before.slug,
        owned.join(", ")
    )))
}

/// Update a provider
///
/// Includes the domain-migration `base_url` change: one field, and every stored relative link
/// re-resolves against the new domain.
#[utoipa::path(
    patch,
    path = "/v1/admin/providers/{id}",
    tag = ADMIN_PROVIDERS_TAG,
    params(("id" = ProviderId, Path, description = "Provider id")),
    request_body = UpdateProvider,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Updated", body = Provider),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Provider not found", body = crate::error::ProblemDetails),
        (status = 409, description = "Provider follows a preset; unlock it before editing the preset-owned fields", body = crate::error::ProblemDetails),
    )
)]
pub async fn update_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
    Json(req): Json<UpdateProvider>,
) -> ApiResult<Json<Provider>> {
    user.require(Permission::ProvidersWrite).await?;
    let before = tankovault_db::repo::providers::get(&state.pool, id).await?;
    refuse_locked_edit(&before, &req)?;
    validate_base_url(&req.base_url).await?;
    let provider = tankovault_db::repo::providers::update(
        &state.pool,
        id,
        &req.name,
        &req.base_url,
        &req.config,
        resolve_politeness(req.politeness),
    )
    .await?;

    let migrated = before.base_url != provider.base_url;
    audit(
        &state,
        &user,
        "provider.update",
        &id.to_string(),
        &serde_json::json!({
            "domain_migration": migrated,
            "base_url_from": before.base_url,
            "base_url_to": provider.base_url,
        }),
    )
    .await;

    Ok(Json(provider))
}

/// List the built-in provider presets
///
/// The preset catalogue this deployment's last install run recorded — the definitions
/// `bootstrap seed-providers` installs from and re-applies to every locked provider. The
/// console reads it to show what a managed provider would look like if it followed its preset
/// again, and what crawl budget the preset suggests.
///
/// An empty list means the install job has not run since the preset catalogue became data;
/// `updated_at` on each entry is how old the recorded catalogue is.
#[utoipa::path(
    get,
    path = "/v1/admin/providers/presets",
    tag = ADMIN_PROVIDERS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "The recorded preset catalogue", body = Vec<PresetDefinition>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn list_provider_presets(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<PresetDefinition>>> {
    user.require(Permission::ProvidersRead).await?;
    Ok(Json(
        tankovault_db::repo::provider_presets::list(&state.pool).await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetPresetLock {
    /// `false` detaches the provider so an operator can edit it freely; `true` re-applies the
    /// preset now and resumes following it.
    pub locked: bool,
}

/// Follow or stop following a provider's preset
///
/// Unlocking is the console's "edit freely" action: the provider keeps naming the preset it came
/// from, but no rollout rewrites it again, and the preset-owned fields become editable.
///
/// Locking is the reverse, and it is **destructive to local edits**: it re-applies the preset's
/// `name`, `base_url`, `adapter` and `config` immediately, discarding whatever the operator had
/// there. Politeness and health state are untouched in both directions — they are not
/// preset-owned.
#[utoipa::path(
    post,
    path = "/v1/admin/providers/{id}/preset",
    tag = ADMIN_PROVIDERS_TAG,
    params(("id" = ProviderId, Path, description = "Provider id")),
    request_body = SetPresetLock,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Updated", body = Provider),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Provider not found", body = crate::error::ProblemDetails),
        (status = 409, description = "Provider came from no preset, or this build no longer ships it", body = crate::error::ProblemDetails),
    )
)]
pub async fn set_provider_preset_lock(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
    Json(req): Json<SetPresetLock>,
) -> ApiResult<Json<Provider>> {
    user.require(Permission::ProvidersWrite).await?;
    let before = tankovault_db::repo::providers::get(&state.pool, id).await?;
    let Some(link) = before.preset.as_ref() else {
        return Err(ApiError::Conflict(format!(
            "provider {} was not installed from a preset, so there is nothing to follow",
            before.slug
        )));
    };

    let provider = if req.locked {
        // Re-linking has to write the preset it is re-linking to, so a build that no longer
        // ships it is refused rather than leaving the row locked to nothing.
        let preset = tankovault_db::repo::provider_presets::get(&state.pool, &link.slug)
            .await?
            .ok_or_else(|| {
                ApiError::Conflict(format!(
                    "this build no longer ships the preset {:?}; the provider stays yours",
                    link.slug
                ))
            })?;
        tankovault_db::repo::providers::apply_preset(&state.pool, id, &preset).await?
    } else {
        tankovault_db::repo::providers::set_preset_lock(&state.pool, id, false).await?
    };

    audit(
        &state,
        &user,
        "provider.preset_lock",
        &id.to_string(),
        &serde_json::json!({
            "slug": provider.slug,
            "preset": link.slug,
            "locked": req.locked,
            // The one thing an audit reader needs that the flag does not carry: re-locking
            // overwrote whatever was in the preset-owned fields.
            "discarded_local_edits": req.locked,
        }),
    )
    .await;

    Ok(Json(provider))
}

/// Delete a provider
///
/// Remove a provider entirely. Its stored source links cascade-delete (FK `ON DELETE
/// CASCADE`); scan-run history is retained with a nulled provider. Admin-only because it is
/// destructive and irreversible.
#[utoipa::path(
    delete,
    path = "/v1/admin/providers/{id}",
    tag = ADMIN_PROVIDERS_TAG,
    params(("id" = ProviderId, Path, description = "Provider id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Acknowledged", body = serde_json::Value, example = json!({"ok": true})),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Provider not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn delete_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::ProvidersDelete).await?;
    let before = tankovault_db::repo::providers::get(&state.pool, id).await?;
    tankovault_db::repo::providers::delete(&state.pool, id).await?;
    audit(
        &state,
        &user,
        "provider.delete",
        &id.to_string(),
        &serde_json::json!({ "slug": before.slug, "base_url": before.base_url }),
    )
    .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetProviderState {
    pub state: ProviderState,
}

/// Set a provider's health state
///
/// Operator override of a provider's health state: `disabled` pauses all crawling; `active`
/// re-enables it (e.g. clearing a tripped circuit breaker). The scanner/circuit breaker may
/// still transition it afterwards.
#[utoipa::path(
    post,
    path = "/v1/admin/providers/{id}/state",
    tag = ADMIN_PROVIDERS_TAG,
    params(("id" = ProviderId, Path, description = "Provider id")),
    request_body = SetProviderState,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Updated", body = Provider),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, a step-up is required, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Provider not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn set_provider_state(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
    Json(req): Json<SetProviderState>,
) -> ApiResult<Json<Provider>> {
    user.require(Permission::ProvidersState).await?;
    tankovault_db::repo::providers::set_state(&state.pool, id, req.state).await?;
    let provider = tankovault_db::repo::providers::get(&state.pool, id).await?;
    audit(
        &state,
        &user,
        "provider.set_state",
        &id.to_string(),
        &serde_json::json!({ "state": req.state.as_str() }),
    )
    .await;
    Ok(Json(provider))
}

/// Re-solve a provider
///
/// Re-solve/refresh a single provider by queuing a **fast** re-scan (frontend §9.5). This is
/// the console "Re-solve" action; it is proxied to the control-plane planner exactly like
/// [`trigger_scan`], scoped to one provider.
#[utoipa::path(
    post,
    path = "/v1/admin/providers/{id}/resolve",
    tag = ADMIN_PROVIDERS_TAG,
    params(("id" = ProviderId, Path, description = "Provider id")),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Scan queued, forwarded from the control-plane", body = tankovault_contracts::admin::ScanTriggeredView),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Provider not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn resolve_provider(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
) -> ApiResult<Json<tankovault_contracts::admin::ScanTriggeredView>> {
    user.require(Permission::ProvidersTest).await?;
    // Confirm the provider exists (and surface a clean 404 otherwise) before queuing work.
    let provider = tankovault_db::repo::providers::get(&state.pool, id).await?;

    let req = TriggerScan {
        provider_id: Some(id),
        mode: ScanMode::Fast,
    };
    let Json(body) = state.control_plane.post("/internal/scans", &req).await?;

    audit(
        &state,
        &user,
        "provider.resolve",
        &id.to_string(),
        &serde_json::json!({ "slug": provider.slug, "mode": "fast" }),
    )
    .await;
    Ok(Json(body))
}

/// Get per-provider crawl stats
///
/// Per-provider crawl statistics table.
#[utoipa::path(
    get,
    path = "/v1/admin/providers/stats",
    tag = ADMIN_PROVIDERS_TAG,
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Per-provider stats", body = Vec<tankovault_contracts::admin::ProviderStatView>),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
    )
)]
pub async fn provider_stats(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<Vec<tankovault_contracts::admin::ProviderStatView>>> {
    user.require(Permission::ProvidersRead).await?;
    // Served from a snapshot: this groups every chapter row by provider, and the console's stats
    // tab and providers tab both ask for it. See `crate::cache`.
    let pool = state.pool.clone();
    let rows = state
        .provider_stats
        .get(move || {
            let pool = pool.clone();
            async move { tankovault_db::repo::stats::provider_stats(&pool).await }
        })
        .await?;
    Ok(Json(rows.into_view()))
}

#[derive(Debug, Deserialize, Serialize, Default, ToSchema)]
pub struct TestAdapterRequest {
    /// Optional relative series path to also fetch metadata + chapters for.
    #[serde(default)]
    pub path: Option<String>,
}

/// Dry-run a provider's adapter
///
/// Dry-run the provider's adapter against the live site and return the parsed sample so
/// operators can fix selectors without a deploy (design §11/§17). Bounded by a timeout;
/// SSRF and rate limits are enforced by the injected fetch stack.
///
/// The body is deliberately free-form JSON: it is a diagnostic dump whose shape follows
/// whatever the adapter managed to parse, and the console renders it verbatim. It is still
/// declared as a schema so the generated client can return it, rather than forcing callers
/// onto an untyped side channel.
#[utoipa::path(
    post,
    path = "/v1/admin/providers/{id}/test",
    tag = ADMIN_PROVIDERS_TAG,
    params(("id" = ProviderId, Path, description = "Provider id")),
    request_body(content = Option<TestAdapterRequest>, description = "Optional relative series path to also fetch metadata + chapters for"),
    security(("bearer_auth" = [])),
    responses(
        (status = 200, description = "Dry-run sample (adapter list/fetch results, each individually ok/error)", body = serde_json::Value, example = json!({"provider": "kunmanga", "latest": {"ok": true, "items": []}})),
        (status = 400, description = "Adapter build failed or the dry-run timed out", body = crate::error::ProblemDetails),
        (status = 401, description = "authentication required", body = crate::error::ProblemDetails),
        (status = 403, description = "no second factor is enrolled, or the caller does not hold the required permission", body = crate::error::ProblemDetails),
        (status = 404, description = "Provider not found", body = crate::error::ProblemDetails),
    )
)]
pub async fn test_adapter(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<ProviderId>,
    body: Option<Json<TestAdapterRequest>>,
) -> ApiResult<Json<serde_json::Value>> {
    user.require(Permission::ProvidersTest).await?;
    let req = body.map(|b| b.0).unwrap_or_default();

    // Proxied to the worker: it already carries the fetch stack, so hosting the dry-run here
    // would double the TLS stack in the API image. Authorization and the audit record stay
    // here — the worker trusts only the internal token, so this tier still answers "may this
    // operator dry-run this provider?".
    let Json(sample): Json<serde_json::Value> = state
        .worker
        .post(&format!("/internal/providers/{id}/test"), &req)
        .await?;

    audit(
        &state,
        &user,
        "provider.test",
        &id.to_string(),
        &serde_json::json!({ "path": req.path }),
    )
    .await;
    Ok(Json(sample))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tankovault_domain::PresetLink;

    fn provider(locked: bool, linked: bool) -> Provider {
        Provider {
            id: ProviderId::new(),
            slug: "kunmanga".to_owned(),
            name: "KunManga".to_owned(),
            base_url: "https://kunmanga.invalid".to_owned(),
            adapter: AdapterKind::Custom,
            config: serde_json::json!({ "latest": { "item": "div.shipped" } }),
            state: ProviderState::Active,
            politeness: Politeness::default(),
            preset: linked.then(|| PresetLink {
                slug: "kunmanga".to_owned(),
                locked,
                synced_at: None,
            }),
            last_full_scan_at: None,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn edit(from: &Provider) -> UpdateProvider {
        UpdateProvider {
            name: from.name.clone(),
            base_url: from.base_url.clone(),
            config: from.config.clone(),
            politeness: None,
        }
    }

    /// Registering a provider without a politeness block must leave it emulating Chrome.
    ///
    /// A block that carries no `emulation` key means "no emulation" — that is what makes the
    /// console's picker work at all, since a generated client omits a `None` field instead of
    /// sending `null`. Reading an absent *block* the same way put every newly registered
    /// provider behind an identifiable bot user-agent, which is what the sites this crawls sit
    /// behind Cloudflare to refuse. The registration form sends no block by design.
    #[test]
    fn a_registration_without_a_politeness_block_keeps_the_server_defaults() {
        let bare: CreateProvider = serde_json::from_str(
            r#"{"slug":"s","name":"n","base_url":"https://b.invalid","adapter":"custom"}"#,
        )
        .expect("the block is optional");
        assert_eq!(resolve_politeness(bare.politeness), Politeness::default());

        let empty: CreateProvider = serde_json::from_str(
            r#"{"slug":"s","name":"n","base_url":"https://b.invalid","adapter":"custom","politeness":{}}"#,
        )
        .expect("an empty block is a complete one");
        assert!(resolve_politeness(empty.politeness).emulation.is_none());
    }

    /// Tuning a crawl budget must work on a managed provider without unlocking it.
    ///
    /// The lock exists to keep *layout* in step with the build; a rate limit answers to the
    /// operator's own infrastructure and legal position. A guard that compared whole request
    /// bodies would refuse this and push operators to unlock — permanently detaching the
    /// provider from the fixes the lock exists to deliver, to change a number the lock was
    /// never about.
    #[test]
    fn politeness_is_editable_while_the_preset_is_locked() {
        let managed = provider(true, true);
        let mut body = edit(&managed);
        body.politeness = Some(PolitenessInput {
            rps: 0.5,
            concurrency: 1,
            ..PolitenessInput::default()
        });
        assert!(refuse_locked_edit(&managed, &body).is_ok());
    }

    /// Each preset-owned field is refused while locked, and named in the message.
    ///
    /// Named because the console's inputs are disabled: reaching this means a direct API call
    /// or a stale tab, and "something you sent is managed" is not enough to act on.
    #[test]
    fn a_preset_owned_edit_is_refused_and_says_which_field() {
        let managed = provider(true, true);

        for (label, mutate) in [
            (
                "name",
                (|b: &mut UpdateProvider| {
                    b.name = "Mine".to_owned();
                }) as fn(&mut UpdateProvider),
            ),
            ("base_url", |b: &mut UpdateProvider| {
                b.base_url = "https://mine.invalid".to_owned();
            }),
            ("config", |b: &mut UpdateProvider| {
                b.config = serde_json::json!({ "latest": { "item": "div.mine" } });
            }),
        ] {
            let mut body = edit(&managed);
            mutate(&mut body);
            let Err(ApiError::Conflict(message)) = refuse_locked_edit(&managed, &body) else {
                panic!("editing {label} on a locked provider must be refused");
            };
            assert!(
                message.contains(label),
                "the message names {label}: {message}"
            );
        }
    }

    /// Unlocked and never-linked providers are ordinary rows.
    #[test]
    fn an_unlocked_or_unlinked_provider_edits_freely() {
        for subject in [provider(false, true), provider(false, false)] {
            let mut body = edit(&subject);
            body.name = "Mine".to_owned();
            body.config = serde_json::json!({ "mine": true });
            assert!(refuse_locked_edit(&subject, &body).is_ok());
        }
    }
}
