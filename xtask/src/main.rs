//! Developer/ops tasks.
//!
//! - `xtask migrate` — apply all pending migrations (used as the deploy migration step).
//! - `xtask reset`   — drop & recreate the `public` schema, then re-migrate (DESTRUCTIVE,
//!   local dev only; guarded by `TANKOVAULT_CONFIRM_RESET=1`).
//! - `xtask seed`    — insert a demo admin user and the built-in provider presets.
//! - `xtask openapi` — regenerate `openapi.json` (the canonical spec) and the typed Rust
//!   API client (`crates/api-client/src/lib.rs`) from the api service's `utoipa` schemas via
//!   `progenitor`. No database needed.
//! - `xtask ci` — run every offline gate CI runs, in CI's order, stopping at the first
//!   failure. No database, no Docker, no network; see `ci.rs` for what it deliberately omits.
//! - `xtask repo-lint` — the repository invariants no compiler or linter can see (a CSP and
//!   the HTML it governs, a published secret and the code that refuses it). Runs as part of
//!   `xtask ci`; see `repo_lint.rs` for the rules and why each one exists.
//! - `xtask config-docs [--check]` — print the `TANKOVAULT_*` surface derived from the config
//!   structs, or (with `--check`) fail if `docs/CONFIGURATION.md` no longer matches it. No
//!   database.
//! - `xtask coverage-ratchet [report.json]` — fail if line coverage has dropped below the
//!   floor committed in `.github/coverage-floor.txt`. Reads a `cargo llvm-cov report --json`
//!   document (default `target/llvm-cov/coverage.json`); runs no tests itself. No database.
//! - `xtask sqlx-prepare [--check]` — regenerate (or verify, with `--check`) the committed
//!   sqlx offline query cache (`.sqlx/`) so the compile-time-checked query macros in
//!   `tankovault-db` build without a live database. Wraps `cargo sqlx prepare` (sqlx-cli).
//!
//! `migrate`/`reset`/`seed`/`sqlx-prepare` read `DATABASE_URL` from the environment;
//! `openapi` does not.

mod ci;
mod config_docs;
mod coverage;
mod repo_lint;

use progenitor_impl::{GenerationSettings, Generator, InterfaceStyle, TypePatch};
use secrecy::{ExposeSecret as _, SecretSlice, SecretString};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_default();

    if cmd == "install-hooks" {
        return install_hooks();
    }

    // Every offline gate CI runs, in CI's order. No database, no Docker, no network.
    if cmd == "ci" {
        return ci::run(workspace_root());
    }

    // The invariants no compiler sees: two artefacts that must agree, with nothing else
    // connecting them. Reads source and deployment files; no database, no network.
    if cmd == "repo-lint" {
        return repo_lint::run(workspace_root());
    }

    // The coverage ratchet. Reads the report `cargo llvm-cov` just wrote and compares it
    // against the committed floor; needs no database and no network.
    if cmd == "coverage-ratchet" {
        let report = std::env::args()
            .nth(2)
            .unwrap_or_else(|| "target/llvm-cov/coverage.json".to_owned());
        return coverage::run(workspace_root(), std::path::Path::new(&report));
    }

    if cmd == "openapi" {
        let check = std::env::args().nth(2).as_deref() == Some("--check");
        return openapi(check);
    }

    // Does `docs/CONFIGURATION.md` still describe the keys the config structs read? Reads
    // source and one markdown file; no database, no network.
    if cmd == "config-docs" {
        let check = std::env::args().nth(2).as_deref() == Some("--check");
        return config_docs::run(workspace_root(), check);
    }

    // Regenerate the committed sqlx offline query cache (`.sqlx/`). Shells out to `sqlx-cli`,
    // which manages its own `DATABASE_URL` connection, so this runs before the pool below.
    if cmd == "sqlx-prepare" {
        let check = std::env::args().nth(2).as_deref() == Some("--check");
        return sqlx_prepare(check);
    }

    // Wrapped straight out of the environment: a DSN carries its password, and `connect`
    // takes the wrapper so no caller can hold it as a bare `String` on the way there.
    let url = SecretString::from(
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL must be set"))?,
    );
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
                 <migrate|reset|seed|openapi [--check]|config-docs [--check]|\
                 sqlx-prepare [--check]>"
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

/// Install `hooks/pre-commit` into `.git/hooks/pre-commit`.
///
/// This used to run from `xtask/build.rs`, i.e. on **every** `cargo build --workspace`. That is
/// a build script mutating the developer's git configuration: a side effect outside `OUT_DIR`
/// and outside the build sandbox, applied without consent, which also breaks hermetic build
/// environments. The guards were well written; the design was the problem.
///
/// It is an explicit command now. The CI gate (`xtask openapi --check`) is what actually
/// enforces the invariant — this hook only moves the discovery earlier, which is a
/// convenience the developer should opt into.
///
/// # Errors
/// When `.git/hooks` cannot be created or written.
fn install_hooks() -> anyhow::Result<()> {
    use std::fs;
    use std::path::Path;

    const MANAGED_MARKER: &str = "tankovault: managed by xtask";

    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ has a parent directory")
        .to_path_buf();
    let git_dir = repo_root.join(".git");
    if !git_dir.exists() {
        anyhow::bail!("{} is not a git checkout", repo_root.display());
    }

    let hooks_dir = git_dir.join("hooks");
    let hook_path = hooks_dir.join("pre-commit");
    let template = include_str!("../hooks/pre-commit");

    // Never clobber a hook we did not install ourselves.
    if let Ok(existing) = fs::read_to_string(&hook_path) {
        if existing == template {
            println!("pre-commit hook is already up to date");
            return Ok(());
        }
        if !existing.contains(MANAGED_MARKER) {
            anyhow::bail!(
                "{} exists and was not installed by xtask; move it aside first",
                hook_path.display()
            );
        }
    }

    fs::create_dir_all(&hooks_dir)?;
    fs::write(&hook_path, template)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755))?;
    }

    println!("installed {}", hook_path.display());
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
    // Formatted here rather than excluded from formatting: `rustfmt.toml`'s `ignore` key is
    // nightly-only, so on stable it printed a warning and formatted the file anyway, which
    // made `cargo fmt --check` permanently red and gated every job downstream of it.
    // Emitting rustfmt's own output keeps `cargo fmt --check` and `xtask openapi --check`
    // agreeing on one canonical form.
    let rendered_client = "// Generated by xtask. DO NOT EDIT.\n".to_owned()
        + &rustfmt(&progenitor_impl::space_out_items(tokens.to_string())?)?;
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

/// Format `src` with the toolchain's `rustfmt`, reading and writing over stdio.
///
/// The generated client must be byte-identical to what `cargo fmt` would produce, otherwise
/// the two check gates contradict each other. `rustfmt` is a rustup component that ships with
/// every toolchain that can build this workspace, so requiring it here adds no new dependency.
fn rustfmt(src: &str) -> anyhow::Result<String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to run rustfmt (is the component installed?): {e}"))?;

    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("rustfmt stdin was not piped"))?
        .write_all(src.as_bytes())?;

    let out = child.wait_with_output()?;
    if !out.status.success() {
        anyhow::bail!("rustfmt exited with {}", out.status);
    }
    Ok(String::from_utf8(out.stdout)?)
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
    let password = SecretString::from(
        std::env::var("TANKOVAULT_SEED_ADMIN_PASSWORD")
            .unwrap_or_else(|_| "changeme12345".to_owned()),
    );
    // Hash with the same pepper the API is configured with, or the seeded admin could never
    // log in. Absent (the common local-dev case) means no pepper, matching the API default.
    let pepper = SecretSlice::from(
        std::env::var("TANKOVAULT_AUTH__PASSWORD_PEPPER")
            .unwrap_or_default()
            .into_bytes(),
    );
    let hash = tankovault_auth::hash_password(&password, &pepper)
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
            // The seed password is printed on purpose: this is a local bootstrap command
            // whose whole output is "here is the account you can now log in with", and it is
            // the operator's own value or the published default. `expose_secret` is what
            // makes that deliberate rather than incidental.
            println!(
                "seeded admin user {} with all {} permissions (password: {})",
                u.username,
                tankovault_domain::Permission::all().len(),
                password.expose_secret(),
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
                politeness: preset.politeness,
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

/// The workspace root, derived from this crate's manifest directory.
///
/// `xtask` sits directly under it by construction (it is a workspace member at `xtask/`), so
/// this is exact rather than a search — and it is right regardless of the shell's working
/// directory, which is what lets `cargo run -p xtask -- ci` work from anywhere in the tree.
fn workspace_root() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits directly under the workspace root")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    /// Run the rewriter over `value` and hand back the result, so a test reads as one
    /// expression rather than three lines of `let mut`.
    fn downgraded(value: serde_json::Value) -> serde_json::Value {
        let mut value = value;
        downgrade_to_3_0(&mut value);
        value
    }

    /// JSON of bounded depth, biased towards the keys the rewriter actually reacts to.
    ///
    /// A uniform generator would essentially never produce a `type` or `examples` key, so the
    /// properties below would only ever exercise the pass-through path.
    fn any_document() -> impl Strategy<Value = serde_json::Value> {
        // A `type` member that is not a string — `[null]`, `[false]`, `[[]]` — is not something
        // OpenAPI can express, and the converter is provably not idempotent on it. That behaviour
        // is pinned explicitly by `a_non_string_type_member_is_not_idempotent`, so the generator
        // must stay inside well-formed documents; otherwise this property just re-derives the same
        // known edge on a random schedule.
        //
        // Restricting `leaf` to strings was not enough to achieve that, and this generator was
        // failing intermittently because of it: `prop_recursive` can hand a `type` key an *array*,
        // whose elements are then arbitrary sub-documents rather than leaves — `{"type": [[]]}`.
        // So `type` gets its own strategy (a string, or 3.1's array-of-strings union) and is
        // inserted separately from the recursive keys, which makes the invariant structural rather
        // than hopeful.
        let type_token = prop::sample::select(vec![
            "string", "integer", "boolean", "null", "object", "array",
        ]);
        let leaf = prop::sample::select(vec![
            "string", "integer", "boolean", "null", "object", "3.1.0", "3.0.3", "x", "",
        ])
        .prop_map(serde_json::Value::from);
        // Deliberately without `type`: it is added from `type_token` below.
        let key = prop::sample::select(vec![
            "examples".to_owned(),
            "openapi".to_owned(),
            "properties".to_owned(),
            "items".to_owned(),
            "description".to_owned(),
        ]);
        leaf.prop_recursive(4, 24, 4, move |inner| {
            let well_formed_type = prop_oneof![
                type_token.clone().prop_map(serde_json::Value::from),
                prop::collection::vec(type_token.clone(), 0..4).prop_map(serde_json::Value::from),
            ];
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::from),
                (
                    prop::collection::hash_map(key.clone(), inner, 0..4),
                    prop::option::of(well_formed_type),
                )
                    .prop_map(|(entries, type_value)| {
                        let mut map: serde_json::Map<_, _> = entries.into_iter().collect();
                        if let Some(type_value) = type_value {
                            map.insert("type".to_owned(), type_value);
                        }
                        serde_json::Value::from(map)
                    }),
            ]
        })
    }

    #[test]
    fn the_document_version_is_rewritten() {
        assert_eq!(
            downgraded(json!({ "openapi": "3.1.0" }))["openapi"],
            "3.0.3"
        );
        // Anything that is not the version this converter was written for is left alone, so a
        // future utoipa emitting 3.2 is a visible failure downstream rather than a silent
        // mislabelling of a document that was never converted.
        assert_eq!(
            downgraded(json!({ "openapi": "3.0.3" }))["openapi"],
            "3.0.3"
        );
    }

    #[test]
    fn a_nullable_union_becomes_a_type_plus_a_nullable_flag() {
        // The conversion this function exists for. `progenitor` reads the 3.0 spelling; getting
        // it wrong makes every optional field on the generated client either non-optional or
        // untyped, and the only signal is `openapi --check` comparing two artifacts that were
        // both produced by this same function.
        let out = downgraded(json!({ "type": ["string", "null"] }));
        assert_eq!(out["type"], "string");
        assert_eq!(out["nullable"], true);
    }

    #[test]
    fn a_plain_type_is_left_exactly_as_it_was() {
        let out = downgraded(json!({ "type": "string", "description": "a title" }));
        assert_eq!(out, json!({ "type": "string", "description": "a title" }));
    }

    #[test]
    fn a_bare_null_type_becomes_a_nullable_flag_with_no_type_at_all() {
        assert_eq!(
            downgraded(json!({ "type": "null" })),
            json!({ "nullable": true })
        );
    }

    /// Pins a **lossy** conversion so that changing it is a deliberate act.
    ///
    /// A union of three or more types collapses to the first non-null one and the rest are
    /// discarded — the generated client will simply not know about them. Our own document does
    /// not currently emit such a union, which is exactly why this would go unnoticed if it
    /// started to.
    #[test]
    fn a_wider_union_collapses_to_its_first_type_and_silently_loses_the_rest() {
        let out = downgraded(json!({ "type": ["string", "integer", "boolean"] }));
        assert_eq!(out["type"], "string");
        assert!(
            out.get("nullable").is_none(),
            "a union with no null member must not be marked nullable"
        );
    }

    /// A second lossy edge: a `type` that is neither a string nor an array is **dropped**,
    /// because it is removed from the map before the match and no arm puts it back.
    #[test]
    fn a_type_that_is_neither_a_string_nor_an_array_is_dropped() {
        assert_eq!(downgraded(json!({ "type": 42 })), json!({}));
    }

    /// **Found by the idempotence property below, and pinned rather than fixed.**
    ///
    /// The "pick the first non-null type" branch inserts whatever it found, without checking
    /// that it is a string. A `type` array holding a non-string member therefore survives the
    /// first pass as a non-string `type` and is *dropped entirely* by a second, because the
    /// match at the top of the function has no arm for it. `downgrade(downgrade(v))` is not
    /// `downgrade(v)`.
    ///
    /// Impact today is nil: `utoipa` writes the null type as the string `"null"`, the converter
    /// runs exactly once per document, and a document like this is not valid `OpenAPI` in the
    /// first place. It is recorded because the two passes *disagree about the same input*,
    /// which is the shape of a real bug the moment anything runs the converter twice or feeds
    /// it a hand-edited spec. The fix belongs in that branch: reject a non-string member rather
    /// than inserting it.
    #[test]
    fn a_non_string_type_member_is_not_idempotent() {
        for malformed in [json!({ "type": [null] }), json!({ "type": [false] })] {
            let once = downgraded(malformed);
            assert!(
                !once["type"].is_string(),
                "the first pass kept a non-string type: {once}"
            );
            assert_eq!(
                downgraded(once),
                json!({}),
                "a second pass drops what the first kept"
            );
        }
    }

    #[test]
    fn the_examples_array_collapses_to_its_first_entry() {
        let out = downgraded(json!({ "examples": ["first", "second"] }));
        assert_eq!(out["example"], "first");
        assert!(out.get("examples").is_none());
        // An empty array yields neither key rather than `example: null`.
        assert_eq!(downgraded(json!({ "examples": [] })), json!({}));
    }

    #[test]
    fn the_rewrite_reaches_schemas_nested_in_objects_and_arrays() {
        let out = downgraded(json!({
            "components": { "schemas": { "S": { "properties": {
                "a": { "type": ["string", "null"] },
            } } } },
            "anyOf": [{ "type": ["integer", "null"] }],
        }));
        assert_eq!(
            out["components"]["schemas"]["S"]["properties"]["a"]["nullable"],
            true
        );
        assert_eq!(out["anyOf"][0]["type"], "integer");
        assert_eq!(out["anyOf"][0]["nullable"], true);
    }

    proptest! {
        /// Idempotence. `openapi --check` compares a freshly generated artifact against the
        /// committed one, and both go through this function — so if it were not idempotent the
        /// check would still pass while the committed client drifted from the spec it claims to
        /// describe. Nothing else in the pipeline would notice.
        #[test]
        fn the_downgrade_is_idempotent(document in any_document()) {
            let once = downgraded(document);
            let twice = downgraded(once.clone());
            prop_assert_eq!(once, twice);
        }

        /// The rewriter touches only `openapi`, `type` and `examples`. Everything else in the
        /// document — descriptions, `$ref`s, `x-rust-type` hints, security schemes — must
        /// arrive at `progenitor` byte-identical.
        #[test]
        fn keys_the_rewriter_does_not_own_pass_through_unchanged(
            key in "[a-z$][a-zA-Z0-9_-]{0,10}",
            value in prop::sample::select(vec![
                json!("text"), json!(7), json!(true), json!(null),
                json!(["a", "b"]), json!({ "nested": "value" }),
            ]),
        ) {
            prop_assume!(!["type", "examples", "openapi"].contains(&key.as_str()));
            let document = json!({ key.clone(): value.clone() });
            prop_assert_eq!(downgraded(document.clone()), document);
        }
    }
}
