//! Developer/ops tasks.
//!
//! - `xtask migrate` — apply all pending migrations (used as the deploy migration step).
//! - `xtask reset`   — drop & recreate the `public` schema, then re-migrate (DESTRUCTIVE,
//!   local dev only; guarded by `TANKOVAULT_CONFIRM_RESET=1`).
//! - `xtask seed`    — insert a demo admin user and the built-in provider presets.
//! - `xtask openapi` — regenerate `web/frontend/wire_schema.json` from the api service's
//!   `utoipa` schemas (the frontend's `typify` codegen input). No database needed.
//!
//! `migrate`/`reset`/`seed` read `DATABASE_URL` from the environment; `openapi` does not.

use tankovault_domain::{Politeness, UserRole};
use utoipa::OpenApi as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_default();

    if cmd == "openapi" {
        let check = std::env::args().nth(2).as_deref() == Some("--check");
        return openapi(check);
    }

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
            eprintln!(
                "unknown command {other:?}; usage: xtask <migrate|reset|seed|openapi [--check]>"
            );
            std::process::exit(2);
        }
    }
    Ok(())
}

/// Dump the api service's `utoipa` component schemas as a plain JSON Schema document (no
/// `OpenAPI` envelope — just `$defs`), so `typify` can consume it directly on the frontend
/// side. `#/components/schemas/X` refs (`OpenAPI` convention) are rewritten to `#/$defs/X`
/// (JSON Schema 2020-12 convention, what `typify` resolves).
///
/// With `check = true`, nothing is written: the freshly generated document is compared
/// against the file on disk and the command exits non-zero if they differ. Used by CI
/// (`.github/workflows/ci.yml`) and the pre-commit hook (`hooks/pre-commit`) instead of a
/// generate-then-`git diff` dance.
fn openapi(check: bool) -> anyhow::Result<()> {
    let doc = tankovault_api::openapi::ApiDoc::openapi();
    let mut schemas = serde_json::to_value(
        doc.components
            .ok_or_else(|| anyhow::anyhow!("ApiDoc has no components"))?
            .schemas,
    )?;
    rewrite_refs(&mut schemas);

    let wrapped = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": schemas,
    });
    let rendered = serde_json::to_string_pretty(&wrapped)? + "\n";

    let out_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../web/frontend/wire_schema.json");

    if check {
        let current = std::fs::read_to_string(&out_path).unwrap_or_default();
        if current != rendered {
            anyhow::bail!(
                "{} is out of date; run `cargo run -p xtask -- openapi` and commit the result",
                out_path.display()
            );
        }
        println!("{} is up to date", out_path.display());
        return Ok(());
    }

    std::fs::write(&out_path, rendered)?;
    println!("wrote {}", out_path.display());
    Ok(())
}

/// Recursively rewrite every `"#/components/schemas/X"` `$ref` to `"#/$defs/X"` in place.
fn rewrite_refs(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(s)) = map.get_mut("$ref") {
                if let Some(name) = s.strip_prefix("#/components/schemas/") {
                    *s = format!("#/$defs/{name}");
                }
            }
            for v in map.values_mut() {
                rewrite_refs(v);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                rewrite_refs(v);
            }
        }
        _ => {}
    }
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
