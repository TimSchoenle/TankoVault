//! Developer/ops tasks.
//!
//! - `xtask migrate` — apply all pending migrations (used as the deploy migration step).
//! - `xtask reset`   — drop & recreate the `public` schema, then re-migrate (DESTRUCTIVE,
//!   local dev only; guarded by `TANKOVAULT_CONFIRM_RESET=1`).
//! - `xtask seed`    — insert a demo admin user and the built-in provider presets.
//!
//! Reads `DATABASE_URL` from the environment.

use tankovault_domain::{Politeness, UserRole};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL must be set"))?;
    let pool = tankovault_db::connect(&url, 5, 10).await?;

    match cmd.as_str() {
        "migrate" => {
            tankovault_db::migrate(&pool).await?;
            println!("migrations applied");
        }
        "reset" => reset(&pool).await?,
        "seed" => seed(&pool).await?,
        other => {
            eprintln!("unknown command {other:?}; usage: xtask <migrate|reset|seed>");
            std::process::exit(2);
        }
    }
    Ok(())
}

/// Drop the whole schema and re-migrate from scratch. Refuses to run unless
/// `TANKOVAULT_CONFIRM_RESET=1` is set, so it cannot wipe a database by accident
/// (e.g. a mis-pointed `DATABASE_URL` in a shell that also targets staging).
async fn reset(pool: &tankovault_db::PgPool) -> anyhow::Result<()> {
    if std::env::var("TANKOVAULT_CONFIRM_RESET").as_deref() != Ok("1") {
        anyhow::bail!(
            "refusing to reset: this DROPs the public schema and destroys all data. \
             Re-run with TANKOVAULT_CONFIRM_RESET=1 to confirm."
        );
    }
    tankovault_db::reset(pool).await?;
    println!("database reset: schema dropped and migrations re-applied");
    Ok(())
}

async fn seed(pool: &tankovault_db::PgPool) -> anyhow::Result<()> {
    // Admin user (idempotent: ignore if already present).
    let password =
        std::env::var("TANKOVAULT_SEED_ADMIN_PASSWORD").unwrap_or_else(|_| "changeme12345".to_owned());
    let hash =
        tankovault_auth::hash_password(&password).map_err(|e| anyhow::anyhow!("hash failed: {e}"))?;
    match tankovault_db::repo::users::create(
        pool,
        "admin@tankovault.local",
        "admin",
        &hash,
        UserRole::Admin,
    )
    .await
    {
        Ok(u) => println!("seeded admin user {} (password: {password})", u.username),
        Err(e) if e.is_unique_violation() || matches!(e, tankovault_db::DbError::Conflict(_)) => {
            println!("admin user already present; skipping");
        }
        Err(e) => return Err(e.into()),
    }

    // Built-in provider presets (design §7): the custom demonicscans adapter plus the
    // Madara-configured manhuaus and kunmanga. Operators are responsible for the legality
    // of crawling; disable or retarget any provider via the admin console.
    for preset in tankovault_adapters::builtin_presets() {
        match tankovault_db::repo::providers::create(
            pool,
            tankovault_db::repo::providers::NewProvider {
                slug: preset.slug.to_owned(),
                name: preset.name.to_owned(),
                base_url: preset.base_url.to_owned(),
                adapter: preset.adapter,
                config: preset.config,
                politeness: Politeness::default(),
            },
        )
        .await
        {
            Ok(p) => println!("seeded provider '{}' ({})", p.slug, p.id),
            Err(tankovault_db::DbError::Conflict(_)) => {
                println!("provider '{}' already present; skipping", preset.slug);
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}
