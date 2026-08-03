//! One-shot installation steps for a deployed `TankoVault`, as a shipped image: `migrate` before
//! a rollout, `seed-admin` and `seed-providers` once at install.
//!
//! Separate from `xtask` on purpose. `xtask` performs the same steps against a developer's own
//! database, but it also carries `reset`, which DROPs the schema, and it is on the deploy
//! blacklist (`[workspace.metadata.deploy.exclude]`) for exactly that reason. This binary is
//! what a cluster runs, so it can do nothing destructive: there is no `reset` here, and both
//! seed steps leave an existing installation untouched.
//!
//! Configuration comes from the same layered loader every service uses, so a deployment sets
//! `TANKOVAULT_DATABASE__URL` here as it does everywhere else. Output is plain stdout rather
//! than structured logs: a `Job`'s whole purpose is the few lines it prints.

use secrecy::{ExposeSecret as _, SecretSlice, SecretString};
use serde::Deserialize;
use tankovault_bootstrap::{AdminOutcome, AdminSeed};
use tankovault_config::DatabaseConfig;

#[derive(Debug, Deserialize)]
struct Config {
    database: DatabaseConfig,
    #[serde(default)]
    auth: AuthConfig,
    #[serde(default = "default_admin_email")]
    seed_admin_email: String,
    #[serde(default = "default_admin_username")]
    seed_admin_username: String,
    /// No default, unlike `xtask seed`: a published image must not be able to create an account
    /// whose password is written down in this repository.
    seed_admin_password: Option<SecretString>,
}

/// The one `auth` key this binary needs. Deliberately not the API service's own `AuthConfig`
/// (`services/api/src/main.rs`), which requires the JWT secret a migration job has no business
/// holding. Not a link: that type is private to another binary crate.
#[derive(Debug, Default, Deserialize)]
struct AuthConfig {
    password_pepper: Option<SecretString>,
}

fn default_admin_email() -> String {
    "admin@tankovault.local".to_owned()
}

fn default_admin_username() -> String {
    "admin".to_owned()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    if !matches!(cmd.as_str(), "migrate" | "seed-admin" | "seed-providers") {
        eprintln!("usage: bootstrap <migrate|seed-admin|seed-providers>");
        std::process::exit(2);
    }

    let cfg: Config = tankovault_config::load()?;
    let pool = tankovault_db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.acquire_timeout_secs,
    )
    .await?;

    match cmd.as_str() {
        "migrate" => {
            tankovault_bootstrap::migrate(&pool).await?;
            println!("migrations applied");
        }
        "seed-admin" => seed_admin(&pool, &cfg).await?,
        // The dispatch above admits nothing else.
        _ => seed_providers(&pool).await?,
    }
    Ok(())
}

/// Provision the first administrator from the configured credentials.
async fn seed_admin(pool: &tankovault_db::PgPool, cfg: &Config) -> anyhow::Result<()> {
    let Some(password) = cfg.seed_admin_password.as_ref() else {
        anyhow::bail!(
            "TANKOVAULT_SEED_ADMIN_PASSWORD must be set to provision the first administrator"
        );
    };
    let pepper = SecretSlice::from(resolve_pepper(
        cfg.auth.password_pepper.as_ref(),
        tankovault_config::is_production(),
    )?);

    let seed = AdminSeed {
        email: &cfg.seed_admin_email,
        username: &cfg.seed_admin_username,
        password,
        pepper: &pepper,
    };
    match tankovault_bootstrap::seed_admin(pool, &seed).await? {
        // The password is not echoed, unlike `xtask seed`: whoever runs this already set it,
        // and a `Job`'s logs outlive the shell that started it.
        AdminOutcome::Created(username) => println!(
            "administrator {username} created with all {} permissions",
            tankovault_domain::Permission::all().len()
        ),
        AdminOutcome::AlreadyPresent => println!("administrator already present; nothing changed"),
    }
    Ok(())
}

/// The pepper bytes to hash the admin password with.
///
/// Un-peppered hashes are a deliberate local-development shape and never a deployed one: the
/// api refuses to *start* that way under the production profile, so seeding here would
/// provision an account into a service that will not run. Split out from [`seed_admin`] so that
/// refusal can be tested without a database.
///
/// # Errors
/// No pepper configured under `TANKOVAULT_PROFILE=production`.
fn resolve_pepper(configured: Option<&SecretString>, production: bool) -> anyhow::Result<Vec<u8>> {
    let pepper = configured.map_or_else(Vec::new, |p| p.expose_secret().as_bytes().to_vec());
    if pepper.is_empty() && production {
        anyhow::bail!(
            "TANKOVAULT_AUTH__PASSWORD_PEPPER is empty in a production profile; set the same \
             value the api runs with, or the seeded account could never log in"
        );
    }
    Ok(pepper)
}

/// Install the built-in provider presets.
async fn seed_providers(pool: &tankovault_db::PgPool) -> anyhow::Result<()> {
    for outcome in tankovault_bootstrap::seed_providers(pool).await? {
        if outcome.created {
            println!("provider '{}' installed", outcome.slug);
        } else {
            println!("provider '{}' already present; skipping", outcome.slug);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pepper is what binds a seeded account to the api that will serve it. Getting this
    /// wrong is silent in the worst way: the account exists, the password is right, and login
    /// fails with nothing in the logs pointing at the seed step.
    #[test]
    fn an_empty_pepper_is_local_only() {
        let configured = SecretString::from("a-real-pepper");

        assert_eq!(
            resolve_pepper(Some(&configured), true).expect("configured under production"),
            b"a-real-pepper"
        );
        // Local development: un-peppered is the shape the api itself allows outside production.
        assert!(
            resolve_pepper(None, false)
                .expect("unset outside production")
                .is_empty()
        );

        assert!(
            resolve_pepper(None, true).is_err(),
            "unset under production"
        );
        // Set-but-empty is the same hole with a variable in front of it, and the likelier one:
        // `TANKOVAULT_AUTH__PASSWORD_PEPPER=` in an env file reads as configured.
        assert!(
            resolve_pepper(Some(&SecretString::from("")), true).is_err(),
            "empty under production"
        );
    }
}
