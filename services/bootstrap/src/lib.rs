//! The installation steps a deployment has to run outside any service: apply the schema,
//! provision the first administrator, install the built-in provider presets.
//!
//! A library as well as a binary because `xtask` performs the same three steps for a developer's
//! own database. Two implementations of "create the first admin" would be two chances to grant a
//! different permission set.

pub mod config;

use secrecy::{SecretSlice, SecretString};
use tankovault_db::PgPool;
use tankovault_domain::{AdapterKind, PresetLink, ProviderId};

/// Apply every pending migration.
///
/// # Errors
/// The migration itself failing, including a checksum mismatch against an already-applied one.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    tankovault_db::migrate(pool).await?;
    Ok(())
}

/// Who the first administrator is, and what the password is hashed with.
pub struct AdminSeed<'a> {
    pub email: &'a str,
    pub username: &'a str,
    pub password: &'a SecretString,
    /// **Must be the pepper the api runs with.** A hash is peppered at rest, so seeding with
    /// one value and serving with another strands the account: the password is correct and the
    /// login still fails, with nothing in the logs naming the cause.
    pub pepper: &'a SecretSlice<u8>,
}

/// What [`seed_admin`] did, so a caller can report it without re-querying.
pub enum AdminOutcome {
    /// The account was created and granted every permission.
    Created {
        username: String,
        /// Whether it also became the deployment's super user — false when the database
        /// already held other accounts, or a super user.
        super_user: bool,
    },
    /// An account with that address or name already existed; nothing was changed.
    AlreadyPresent,
}

/// Everything a run of [`seed_admin`] has to report: what happened to the configured account,
/// and whether the deployment gained an owner along the way.
pub struct AdminReport {
    pub outcome: AdminOutcome,
    /// The account this run promoted to super user because the deployment had none — by
    /// username, since that is what an install log can be read against.
    ///
    /// `None` in the ordinary case, including the fresh install where
    /// [`AdminOutcome::Created`] already claimed ownership for the account it made.
    pub promoted_owner: Option<String>,
}

/// Create the first administrator, with every grantable permission — plus the super user grant
/// if this is the deployment's first account — and leave the deployment with an owner either way.
///
/// Idempotent: an existing account is left exactly as it is, permissions included, so re-running
/// an install job cannot re-grant something an operator has deliberately revoked. Running it a
/// second time under a different address creates an ordinary administrator: the super user grant
/// is claimed only while no other account exists, and the database refuses a second one.
///
/// The ownership reconciliation is the exception to "leave an existing installation untouched",
/// and a deliberate one: the super user grant cannot be revoked through the API — the permission
/// editor filters it out of both sides of an edit — so there is no operator decision to
/// overwrite, only the absence of one. See
/// [`tankovault_db::repo::permissions::ensure_super_user`] for who it promotes and why that is
/// nobody who could not already promote themselves.
///
/// Registration mints no privilege anywhere else in the system — this is the one deliberate
/// exception, and without it no account could ever grant `users.permissions` to another, so a
/// fresh installation would have no way in.
///
/// # Errors
/// Hashing the password, the ownership reconciliation, or any of the inserts other than the
/// "already present" conflict.
pub async fn seed_admin(pool: &PgPool, seed: &AdminSeed<'_>) -> anyhow::Result<AdminReport> {
    let hash = tankovault_auth::hash_password(seed.password, seed.pepper)
        .map_err(|e| anyhow::anyhow!("hash failed: {e}"))?;
    let outcome = match tankovault_db::repo::users::create(pool, seed.email, seed.username, &hash)
        .await
    {
        Ok(user) => {
            // Provisioned by the operator rather than through the email-confirmation flow, so
            // the address is marked verified — otherwise the login gate locks the account out
            // as soon as a mailer is configured.
            tankovault_db::repo::users::mark_email_verified(pool, user.id).await?;
            for permission in tankovault_domain::Permission::grantable() {
                tankovault_db::repo::permissions::grant(pool, user.id, permission, None).await?;
            }
            // The enumerated grants above age: a capability added by a later release reaches
            // this account only if someone re-grants it. The super user grant does not.
            let super_user =
                tankovault_db::repo::permissions::claim_super_user(pool, user.id).await?;
            AdminOutcome::Created {
                username: user.username,
                super_user,
            }
        }
        Err(e) if e.is_unique_violation() || matches!(e, tankovault_db::DbError::Conflict(_)) => {
            AdminOutcome::AlreadyPresent
        }
        Err(e) => return Err(e.into()),
    };

    // The claim above only fires for the first account of an empty database, which is not every
    // installation that ends up running this job. Reconcile the rest here so the install cannot
    // finish reporting success on a deployment that has no owner at all.
    let promoted_owner = match tankovault_db::repo::permissions::ensure_super_user(pool).await? {
        Some(user_id) => Some(
            tankovault_db::repo::users::get(pool, user_id)
                .await?
                .username,
        ),
        None => None,
    };

    Ok(AdminReport {
        outcome,
        promoted_owner,
    })
}

/// What the reconcile did to one provider, for reporting.
pub enum ProviderOutcome {
    /// No row carried this slug; it was installed, locked to the preset.
    Installed,
    /// A locked row was re-written from the preset.
    Synced,
    /// A row that predates the preset link, and still matched it exactly, now follows it.
    Adopted,
    /// A row that predates the preset link and carries operator edits: linked for reference,
    /// left exactly as it is.
    AdoptedCustomised,
    /// The row named this preset but has been unlocked; nothing was touched.
    Unlocked,
}

impl ProviderOutcome {
    /// A stable token for the install log — the whole output of this job is those few lines.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Synced => "synced from preset",
            Self::Adopted => "adopted; now follows the preset",
            Self::AdoptedCustomised => "customised; left as it is",
            Self::Unlocked => "unlocked by an operator; left as it is",
        }
    }
}

/// Everything one reconcile run has to report.
pub struct ProviderReport {
    /// One line per shipped preset, in catalogue order.
    pub providers: Vec<(String, ProviderOutcome)>,
    /// Catalogue entries this build no longer ships. Providers installed from them keep
    /// running; only the preset definition goes.
    pub retired: Vec<String>,
}

/// What the reconcile should do with one shipped preset, given the provider row that carries
/// its slug (if any).
///
/// Split out as a pure function because it is the whole feature: everything around it is a
/// database call, and every interesting case here is one an operator would only discover in
/// production. See the tests at the foot of this file for the four of them.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Action {
    Install,
    /// Write the preset-owned fields of an existing row.
    Sync(ProviderId, ProviderOutcomeKind),
    /// Link an existing row for reference without rewriting it.
    LinkOnly(ProviderId),
    Leave,
}

/// The reporting half of [`Action::Sync`], so the caller can name what it did.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ProviderOutcomeKind {
    /// A row that was already following the preset.
    Synced,
    /// A row that predates the preset link and is joining it now.
    Adopted,
}

/// Decide one preset's fate.
///
/// The rule that matters is the middle one: a row that predates the preset link is **adopted
/// but not locked** unless it still equals the shipped definition byte for byte. An upgrade
/// must never silently overwrite a config an operator hand-tuned before this feature existed —
/// so equality is the only evidence accepted that there is nothing of theirs to lose.
fn plan(preset: &PresetView<'_>, existing: Option<&ExistingProvider<'_>>) -> Action {
    match existing {
        None => Action::Install,
        Some(row) => match &row.preset {
            // Already governed by this preset: keep it in step.
            Some(link) if link.locked => Action::Sync(row.id, ProviderOutcomeKind::Synced),
            // Linked and deliberately unlocked. The operator owns it now.
            Some(_) => Action::Leave,
            // Predates the link.
            None => {
                if preset.matches(row) {
                    Action::Sync(row.id, ProviderOutcomeKind::Adopted)
                } else {
                    Action::LinkOnly(row.id)
                }
            }
        },
    }
}

/// The preset-owned fields of a shipped preset, for the equality test in [`plan`].
struct PresetView<'a> {
    name: &'a str,
    base_url: &'a str,
    adapter: AdapterKind,
    config: &'a serde_json::Value,
}

/// The same fields of a provider row, plus its identity and current link.
struct ExistingProvider<'a> {
    id: ProviderId,
    name: &'a str,
    base_url: &'a str,
    adapter: AdapterKind,
    config: &'a serde_json::Value,
    preset: Option<PresetLink>,
}

impl PresetView<'_> {
    /// Whether every preset-owned field of `row` already equals this preset. Politeness and
    /// state are excluded because they are not preset-owned — an operator who lowered a rate
    /// limit has not customised the *preset*, and must not be dropped out of sync for it.
    fn matches(&self, row: &ExistingProvider<'_>) -> bool {
        self.name == row.name
            && self.base_url == row.base_url
            && self.adapter == row.adapter
            && self.config == row.config
    }
}

/// Record the shipped preset catalogue and reconcile every provider against it.
///
/// Idempotent, and safe on every rollout — which is how it is meant to run. Providers an
/// operator registered by hand are never touched; providers installed from a preset follow it
/// until the operator unlocks them in the console.
///
/// Operators remain responsible for the legality of crawling; every preset can be paused or
/// retargeted from the admin console, and neither is undone by a later run.
///
/// # Errors
/// Any of the catalogue writes or provider writes failing.
pub async fn seed_providers(pool: &PgPool) -> anyhow::Result<ProviderReport> {
    use tankovault_db::repo::{provider_presets, providers};

    let shipped = tankovault_adapters::builtin_presets();

    // The catalogue mirror first: the reconcile below re-reads its own writes, and a console
    // that showed a provider linked to a preset it cannot display would be worse than a slow
    // rollout.
    for preset in &shipped {
        provider_presets::upsert(
            pool,
            &provider_presets::NewPreset {
                slug: preset.slug.to_owned(),
                name: preset.name.to_owned(),
                base_url: preset.base_url.to_owned(),
                adapter: preset.adapter,
                config: preset.config.clone(),
                politeness: preset.politeness.clone(),
            },
        )
        .await?;
    }
    let shipped_slugs: Vec<String> = shipped.iter().map(|p| p.slug.to_owned()).collect();
    let retired = provider_presets::retire_missing(pool, &shipped_slugs).await?;

    let mut outcomes = Vec::with_capacity(shipped.len());
    for preset in &shipped {
        let existing = match providers::get_by_slug(pool, preset.slug).await {
            Ok(provider) => Some(provider),
            Err(tankovault_db::DbError::NotFound) => None,
            Err(e) => return Err(e.into()),
        };
        let view = PresetView {
            name: preset.name,
            base_url: preset.base_url,
            adapter: preset.adapter,
            config: &preset.config,
        };
        let row = existing.as_ref().map(|p| ExistingProvider {
            id: p.id,
            name: &p.name,
            base_url: &p.base_url,
            adapter: p.adapter,
            config: &p.config,
            preset: p.preset.clone(),
        });

        let outcome = match plan(&view, row.as_ref()) {
            Action::Install => {
                providers::create(
                    pool,
                    providers::NewProvider {
                        slug: preset.slug.to_owned(),
                        name: preset.name.to_owned(),
                        base_url: preset.base_url.to_owned(),
                        adapter: preset.adapter,
                        config: preset.config.clone(),
                        politeness: preset.politeness.clone(),
                        preset_slug: Some(preset.slug.to_owned()),
                    },
                )
                .await?;
                ProviderOutcome::Installed
            }
            Action::Sync(id, kind) => {
                // The stored catalogue row, not a value rebuilt from the preset: the sync then
                // writes exactly what the console later diffs against.
                let Some(stored) = provider_presets::get(pool, preset.slug).await? else {
                    continue;
                };
                providers::apply_preset(pool, id, &stored).await?;
                match kind {
                    ProviderOutcomeKind::Adopted => ProviderOutcome::Adopted,
                    ProviderOutcomeKind::Synced => ProviderOutcome::Synced,
                }
            }
            Action::LinkOnly(id) => {
                providers::adopt_preset(pool, id, preset.slug, false).await?;
                ProviderOutcome::AdoptedCustomised
            }
            Action::Leave => ProviderOutcome::Unlocked,
        };
        outcomes.push((preset.slug.to_owned(), outcome));
    }

    Ok(ProviderReport {
        providers: outcomes,
        retired,
    })
}

#[cfg(test)]
mod tests {
    use super::{Action, ExistingProvider, PresetView, ProviderOutcomeKind, plan};
    use serde_json::json;
    use tankovault_domain::{AdapterKind, PresetLink, ProviderId};

    fn preset(config: &serde_json::Value) -> PresetView<'_> {
        PresetView {
            name: "KunManga",
            base_url: "https://www.kunmanga.co.uk",
            adapter: AdapterKind::Custom,
            config,
        }
    }

    fn row(config: &serde_json::Value, preset: Option<PresetLink>) -> ExistingProvider<'_> {
        ExistingProvider {
            id: ProviderId::new(),
            name: "KunManga",
            base_url: "https://www.kunmanga.co.uk",
            adapter: AdapterKind::Custom,
            config,
            preset,
        }
    }

    fn link(locked: bool) -> PresetLink {
        PresetLink {
            slug: "kunmanga".to_owned(),
            locked,
            synced_at: None,
        }
    }

    /// A deployment that has never seen this preset gets the provider.
    #[test]
    fn a_missing_provider_is_installed() {
        let config = json!({});
        assert_eq!(plan(&preset(&config), None), Action::Install);
    }

    /// The point of the whole feature: a locked row is re-written on every run, which is how a
    /// selector fix reaches a deployment that already carries the provider. Before the preset
    /// link existed, seeding was create-only and a fix reached new installations only.
    #[test]
    fn a_locked_provider_is_rewritten_from_the_preset() {
        let config = json!({ "latest": { "item": "div.old" } });
        let target = row(&config, Some(link(true)));
        assert_eq!(
            plan(&preset(&config), Some(&target)),
            Action::Sync(target.id, ProviderOutcomeKind::Synced)
        );
    }

    /// Unlocking is permanent until the operator asks for the opposite. A run that "helpfully"
    /// re-synced an unlocked row would silently discard the edit they unlocked it to make.
    #[test]
    fn an_unlocked_provider_is_never_rewritten() {
        let config = json!({ "latest": { "item": "div.mine" } });
        assert_eq!(
            plan(&preset(&config), Some(&row(&config, Some(link(false))))),
            Action::Leave
        );
    }

    /// Adoption of rows that predate the link is the upgrade path, and the dangerous one: an
    /// untouched row may be locked, but a row carrying operator edits must only be *labelled*,
    /// never overwritten — that data has no other copy.
    #[test]
    fn adoption_locks_only_a_row_that_still_matches_the_preset() {
        let shipped = json!({ "latest": { "item": "div.manga-item" } });
        let edited = json!({ "latest": { "item": "div.operator-fixed-this" } });
        let untouched = row(&shipped, None);
        let customised = row(&edited, None);

        assert_eq!(
            plan(&preset(&shipped), Some(&untouched)),
            Action::Sync(untouched.id, ProviderOutcomeKind::Adopted)
        );
        assert_eq!(
            plan(&preset(&shipped), Some(&customised)),
            Action::LinkOnly(customised.id)
        );
    }

    /// Key order is a JSON serialisation detail, not an operator edit. Comparing the rendered
    /// text instead of the parsed value would mark every adopted row as customised the first
    /// time Postgres handed `jsonb` back in its own key order.
    #[test]
    fn adoption_compares_parsed_json_not_its_spelling() {
        let shipped = json!({ "a": 1, "b": 2 });
        let reordered = json!({ "b": 2, "a": 1 });
        let target = row(&reordered, None);
        assert_eq!(
            plan(&preset(&shipped), Some(&target)),
            Action::Sync(target.id, ProviderOutcomeKind::Adopted)
        );
    }
}
