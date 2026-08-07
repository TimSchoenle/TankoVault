//! The installation steps a deployment has to run outside any service: apply the schema,
//! provision the first administrator, install the built-in provider presets.
//!
//! A library as well as a binary because `xtask` performs the same three steps for a developer's
//! own database. Two implementations of "create the first admin" would be two chances to grant a
//! different permission set.

use secrecy::{SecretSlice, SecretString};
use tankovault_db::PgPool;

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

/// Create the first administrator, with every grantable permission — plus the super user grant
/// if this is the deployment's first account.
///
/// Idempotent: an existing account is left exactly as it is, permissions included, so re-running
/// an install job cannot re-grant something an operator has deliberately revoked. Running it a
/// second time under a different address creates an ordinary administrator: the super user grant
/// is claimed only while no other account exists, and the database refuses a second one.
///
/// Registration mints no privilege anywhere else in the system — this is the one deliberate
/// exception, and without it no account could ever grant `users.permissions` to another, so a
/// fresh installation would have no way in.
///
/// # Errors
/// Hashing the password, or any of the inserts other than the "already present" conflict.
pub async fn seed_admin(pool: &PgPool, seed: &AdminSeed<'_>) -> anyhow::Result<AdminOutcome> {
    let hash = tankovault_auth::hash_password(seed.password, seed.pepper)
        .map_err(|e| anyhow::anyhow!("hash failed: {e}"))?;
    match tankovault_db::repo::users::create(pool, seed.email, seed.username, &hash).await {
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
            Ok(AdminOutcome::Created {
                username: user.username,
                super_user,
            })
        }
        Err(e) if e.is_unique_violation() || matches!(e, tankovault_db::DbError::Conflict(_)) => {
            Ok(AdminOutcome::AlreadyPresent)
        }
        Err(e) => Err(e.into()),
    }
}

/// One provider preset's fate, for reporting.
pub struct ProviderOutcome {
    pub slug: &'static str,
    pub created: bool,
}

/// Install the built-in provider presets, skipping any already present.
///
/// Operators are responsible for the legality of crawling; every preset can be disabled or
/// retargeted from the admin console afterwards.
///
/// # Errors
/// Any insert failing for a reason other than the preset already existing.
pub async fn seed_providers(pool: &PgPool) -> anyhow::Result<Vec<ProviderOutcome>> {
    let mut outcomes = Vec::new();
    for preset in tankovault_adapters::builtin_presets() {
        let created = match tankovault_db::repo::providers::create(
            pool,
            tankovault_db::repo::providers::NewProvider {
                slug: preset.slug.to_owned(),
                name: preset.name.to_owned(),
                base_url: preset.base_url.to_owned(),
                adapter: preset.adapter,
                config: preset.config,
                politeness: preset.politeness,
            },
        )
        .await
        {
            Ok(_) => true,
            Err(tankovault_db::DbError::Conflict(_)) => false,
            Err(e) => return Err(e.into()),
        };
        outcomes.push(ProviderOutcome {
            slug: preset.slug,
            created,
        });
    }
    Ok(outcomes)
}
