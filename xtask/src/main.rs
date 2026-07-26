//! Developer/ops tasks.
//!
//! - `xtask migrate` — apply all pending migrations (used as the deploy migration step).
//! - `xtask reset`   — drop & recreate the `public` schema, then re-migrate (DESTRUCTIVE,
//!   local dev only; guarded by `TANKOVAULT_CONFIRM_RESET=1`).
//! - `xtask seed`    — insert a demo admin user and the built-in provider presets.
//! - `xtask openapi` — regenerate `openapi.json` (the canonical spec) and the typed Rust
//!   API client (`crates/api-client/src/lib.rs`) from the api service's `utoipa` schemas via
//!   `progenitor`. No database needed.
//! - `xtask sqlx-prepare [--check]` — regenerate (or verify, with `--check`) the committed
//!   sqlx offline query cache (`.sqlx/`) so the compile-time-checked query macros in
//!   `tankovault-db` build without a live database. Wraps `cargo sqlx prepare` (sqlx-cli).
//!
//! `migrate`/`reset`/`seed`/`sqlx-prepare` read `DATABASE_URL` from the environment;
//! `openapi` does not.

use progenitor_impl::{GenerationSettings, Generator, InterfaceStyle, TypePatch};
use tankovault_domain::Politeness;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_default();

    if cmd == "openapi" {
        let check = std::env::args().nth(2).as_deref() == Some("--check");
        return openapi(check);
    }

    // Regenerate the committed sqlx offline query cache (`.sqlx/`). Shells out to `sqlx-cli`,
    // which manages its own `DATABASE_URL` connection, so this runs before the pool below.
    if cmd == "sqlx-prepare" {
        let check = std::env::args().nth(2).as_deref() == Some("--check");
        return sqlx_prepare(check);
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
                "unknown command {other:?}; usage: xtask \
                 <migrate|reset|seed|openapi [--check]|sqlx-prepare [--check]>"
            );
            std::process::exit(2);
        }
    }
    Ok(())
}

/// Regenerate (or, with `check = true`, verify) the committed sqlx offline query cache in
/// `.sqlx/`. The repository queries in `tankovault-db` are the compile-time-checked
/// `query!`/`query_as!` macros; this cache lets `cargo build`/CI/Docker resolve them without
/// a live database (`SQLX_OFFLINE=true`).
///
/// Shells out to `sqlx-cli` (`cargo sqlx prepare`), which connects using `DATABASE_URL` and
/// must run from the workspace root against a migrated database. Install it once with
/// `cargo install sqlx-cli --no-default-features --features rustls,postgres`.
///
/// With `check = true` (used by CI) nothing is written; the command fails if the cache is
/// out of date relative to the current queries + schema.
fn sqlx_prepare(check: bool) -> anyhow::Result<()> {
    if std::env::var_os("DATABASE_URL").is_none() {
        anyhow::bail!("DATABASE_URL must be set (point it at a migrated database)");
    }

    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| anyhow::anyhow!("xtask/ has a parent directory"))?;

    let mut cmd = std::process::Command::new("cargo");
    cmd.current_dir(workspace_root)
        .arg("sqlx")
        .arg("prepare")
        .arg("--workspace");
    if check {
        cmd.arg("--check");
    }

    let status = cmd.status().map_err(|e| {
        anyhow::anyhow!(
            "failed to run `cargo sqlx prepare` ({e}); install sqlx-cli with \
             `cargo install sqlx-cli --no-default-features --features rustls,postgres`"
        )
    })?;
    if !status.success() {
        anyhow::bail!(
            "`cargo sqlx prepare{}` failed; the offline query cache is stale — \
             run `cargo run -p xtask -- sqlx-prepare` against a migrated database",
            if check { " --check" } else { "" }
        );
    }
    println!(
        "sqlx offline query cache {}",
        if check {
            "is up to date"
        } else {
            "written to .sqlx/"
        }
    );
    Ok(())
}

/// Regenerate the two committed `OpenAPI` artifacts from the api service's `utoipa` schemas:
///
/// 1. `openapi.json` — the canonical, pretty-printed `OpenAPI` 3.1 document.
/// 2. `crates/api-client/src/lib.rs` — the typed Rust client `progenitor` derives from it,
///    consumed directly by the Dioxus frontend.
///
/// With `check = true` nothing is written: each freshly rendered artifact is compared against
/// the copy on disk and the command exits non-zero on any difference. Used by CI
/// (`.github/workflows/ci.yml`) and the pre-commit hook (`hooks/pre-commit`) to catch a
/// backend DTO change that never got regenerated, without a generate-then-`git diff` dance.
fn openapi(check: bool) -> anyhow::Result<()> {
    let doc = tankovault_api::full_openapi();
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    // The canonical spec, serialised once and reused as the progenitor input below.
    let spec_value = serde_json::to_value(&doc)?;
    let rendered_spec = serde_json::to_string_pretty(&spec_value)?;
    let spec_path = manifest_dir.join("../openapi.json");

    // Generate the typed Rust client for crates/api-client.
    let mut settings = GenerationSettings::default();
    settings.with_interface(InterfaceStyle::Builder);
    // Every generated type crosses a Dioxus component boundary on the frontend (props,
    // signals, memos), all of which require `PartialEq`. progenitor only derives it when
    // asked, so add it globally here.
    settings.with_derive("PartialEq");

    // Map our domain IDs back to the real newtypes instead of generating generic Uuids.
    // This preserves our labels and logic in the frontend.
    for id_type in ID_TYPES {
        settings.with_patch(
            id_type,
            TypePatch::default()
                .with_derive("Copy")
                .with_derive("Eq")
                .with_derive("Hash")
                .with_derive("PartialEq"),
        );
    }

    let mut generator = Generator::new(&settings);

    // progenitor rides on the `openapiv3` crate, which only understands OpenAPI 3.0, so the
    // 3.1 document is downgraded first; `x-rust-type` hints then steer domain newtypes.
    let mut spec_json = spec_value.clone();
    downgrade_to_3_0(&mut spec_json);
    inject_rust_types(&mut spec_json);

    let spec: openapiv3::OpenAPI = serde_json::from_value(spec_json)
        .map_err(|e| anyhow::anyhow!("Failed to parse OpenAPI for progenitor: {e}"))?;

    let tokens = generator.generate_tokens(&spec)?;
    let rendered_client = "// Generated by xtask. DO NOT EDIT.\n".to_owned()
        + &progenitor_impl::space_out_items(tokens.to_string()).unwrap();
    let client_path = manifest_dir.join("../crates/api-client/src/lib.rs");

    let artifacts = [(spec_path, rendered_spec), (client_path, rendered_client)];

    if check {
        for (path, rendered) in &artifacts {
            let current = std::fs::read_to_string(path).unwrap_or_default();
            if &current != rendered {
                anyhow::bail!(
                    "{} is out of date; run `cargo run -p xtask -- openapi`",
                    path.display()
                );
            }
        }
        println!("OpenAPI artifacts are up to date");
        return Ok(());
    }

    for (path, rendered) in &artifacts {
        std::fs::write(path, rendered)?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

/// The domain typed-id newtypes, shared by the client `TypePatch`es (extra derives applied in
/// [`openapi`]) and the `x-rust-type` hints below, so the two lists can never drift.
const ID_TYPES: [&str; 10] = [
    "SeriesId",
    "ChapterId",
    "ProviderId",
    "ScanRunId",
    "ScanTaskId",
    "SeriesSourceId",
    "TagId",
    "UserId",
    "AuthorId",
    "NotificationId",
];

/// Domain enums re-mapped to their canonical Rust definitions on the frontend.
///
/// `Permission` and `Feature` are deliberately **absent**. Both are `#[non_exhaustive]` and
/// exist to be matched exhaustively against a specific build's registry; mapping them onto the
/// domain types would make the generated client refuse to deserialise a response from a server
/// that has a capability this build does not, turning a routine version skew into a hard
/// failure. The frontend treats them as their wire strings instead.
const ENUM_TYPES: [&str; 7] = [
    "ContentType",
    "SeriesStatus",
    "WatchStatus",
    "RunState",
    "ScanMode",
    "Politeness",
    "AccountStatus",
];

/// Inject `x-rust-type` into the id and enum schema components so `progenitor` emits our
/// domain newtypes/enums instead of freshly generated stand-ins.
fn inject_rust_types(value: &mut serde_json::Value) {
    let Some(map) = value
        .pointer_mut("/components/schemas")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    for name in ID_TYPES.iter().chain(ENUM_TYPES.iter()) {
        if let Some(schema) = map
            .get_mut(*name)
            .and_then(serde_json::Value::as_object_mut)
        {
            schema.insert(
                "x-rust-type".to_string(),
                serde_json::json!(format!("tankovault_domain::{name}")),
            );
        }
    }
}

/// Downgrade `OpenAPI` 3.1.0 (utoipa 5 default) to 3.0.3 (openapiv3 crate requirement).
/// This handles the `type: [string, null]` -> `type: string, nullable: true` conversion
/// and changes the version string.
fn downgrade_to_3_0(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(v) = map.get_mut("openapi") {
                if v == "3.1.0" {
                    *v = serde_json::json!("3.0.3");
                }
            }

            // Handle 'type' which can be a string or an array in 3.1
            if let Some(type_val) = map.remove("type") {
                match type_val {
                    serde_json::Value::String(s) => {
                        if s == "null" {
                            map.insert("nullable".to_string(), serde_json::json!(true));
                        } else {
                            map.insert("type".to_string(), serde_json::Value::String(s));
                        }
                    }
                    serde_json::Value::Array(types) => {
                        if types.len() == 2 && types.contains(&serde_json::json!("null")) {
                            let other_type = types
                                .iter()
                                .find(|t| *t != &serde_json::json!("null"))
                                .cloned();
                            if let Some(t) = other_type {
                                map.insert("type".to_string(), t);
                                map.insert("nullable".to_string(), serde_json::json!(true));
                            }
                        } else if !types.is_empty() {
                            // Pick the first non-null type if possible
                            let first_type = types
                                .iter()
                                .find(|t| *t != &serde_json::json!("null"))
                                .or_else(|| types.first());
                            if let Some(t) = first_type {
                                if t == "null" {
                                    map.insert("nullable".to_string(), serde_json::json!(true));
                                } else {
                                    map.insert("type".to_string(), t.clone());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Also handle 'examples' -> 'example' (OpenAPI 3.0 vs 3.1)
            if let Some(serde_json::Value::Array(arr)) = map.remove("examples") {
                if !arr.is_empty() {
                    map.insert("example".to_string(), arr[0].clone());
                }
            }

            for v in map.values_mut() {
                downgrade_to_3_0(v);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                downgrade_to_3_0(v);
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
    let password = std::env::var("TANKOVAULT_SEED_ADMIN_PASSWORD")
        .unwrap_or_else(|_| "changeme12345".to_owned());
    let hash = tankovault_auth::hash_password(&password)
        .map_err(|e| anyhow::anyhow!("hash failed: {e}"))?;
    match tankovault_db::repo::users::create(pool, "admin@tankovault.local", "admin", &hash).await {
        Ok(u) => {
            // The admin is provisioned by the operator, not through the email-confirmation
            // flow, so mark its address verified — otherwise the login gate would lock it out
            // whenever a mailer is configured.
            tankovault_db::repo::users::mark_email_verified(pool, u.id).await?;

            // Accounts are created with no permissions — the registration path must never be
            // able to mint privilege. The seed is the deliberate exception: a fresh deployment
            // needs one account that can reach the console, and above all one that holds
            // `users.permissions`, since without it nobody could ever grant anything.
            //
            // `granted_by` is `None`: nobody granted these, the installation did.
            for permission in tankovault_domain::Permission::all() {
                tankovault_db::repo::permissions::grant(pool, u.id, *permission, None).await?;
            }
            println!(
                "seeded admin user {} with all {} permissions (password: {password})",
                u.username,
                tankovault_domain::Permission::all().len(),
            );
        }
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
