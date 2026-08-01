//! Developer/CI entry point: migrations, seeding, OpenAPI regeneration, the offline CI gate,
//! and the repo-invariant/config-docs/coverage checks. Run with no arguments for usage.

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

    if cmd == "ci" {
        return ci::run(workspace_root());
    }

    if cmd == "repo-lint" {
        return repo_lint::run(workspace_root());
    }

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

    if cmd == "config-docs" {
        let check = std::env::args().nth(2).as_deref() == Some("--check");
        return config_docs::run(workspace_root(), check);
    }

    // Shells out to `sqlx-cli`, which manages its own `DATABASE_URL` connection, so this runs
    // before the pool below is opened.
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
/// `.sqlx/` via `cargo sqlx prepare`. Needs `DATABASE_URL` pointing at a migrated database and
/// `sqlx-cli` installed.
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
/// An opt-in convenience, not the enforcement: `xtask openapi --check` in CI is what actually
/// catches a regenerated artefact that never got committed.
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

/// Regenerate `openapi.json` and the typed `crates/api-client/src/lib.rs` from the api
/// service's `utoipa` schemas. With `check = true`, verifies both against disk instead of
/// writing, and fails on any difference.
fn openapi(check: bool) -> anyhow::Result<()> {
    let doc = tankovault_api::full_openapi();
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    let spec_value = serde_json::to_value(&doc)?;
    let rendered_spec = serde_json::to_string_pretty(&spec_value)?;
    let spec_path = manifest_dir.join("../openapi.json");

    let mut settings = GenerationSettings::default();
    settings.with_interface(InterfaceStyle::Builder);
    // Every generated type crosses a Dioxus prop/signal boundary, which requires `PartialEq`.
    settings.with_derive("PartialEq");

    // Map domain IDs back to their real newtypes instead of generating generic Uuids.
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
    // Piped through rustfmt here (not excluded via `rustfmt.toml`'s nightly-only `ignore`) so
    // `cargo fmt --check` and `xtask openapi --check` agree on one canonical form.
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

/// Format `src` with the toolchain's `rustfmt` over stdio.
///
/// Must match `cargo fmt`'s output exactly, or `xtask openapi --check` and `cargo fmt --check`
/// would disagree on the same file.
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

/// The domain typed-id newtypes. Shared by the `TypePatch`es in [`openapi`] and the
/// `x-rust-type` hints below, so the two lists cannot drift apart.
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
/// `Permission` and `Feature` are absent on purpose: both are `#[non_exhaustive]`, and mapping
/// them would make the client reject a response naming a capability this build lacks. The
/// frontend keeps them as wire strings instead.
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
            if let Some(v) = map.get_mut("openapi")
                && v == "3.1.0"
            {
                *v = serde_json::json!("3.0.3");
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
            if let Some(serde_json::Value::Array(arr)) = map.remove("examples")
                && !arr.is_empty()
            {
                map.insert("example".to_string(), arr[0].clone());
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
    // Idempotent: ignore if the admin already exists.
    let password = SecretString::from(
        std::env::var("TANKOVAULT_SEED_ADMIN_PASSWORD")
            .unwrap_or_else(|_| "changeme12345".to_owned()),
    );
    // Must match the API's configured pepper, or the seeded admin could never log in.
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

            // Accounts otherwise get no permissions — registration must never mint privilege.
            // The seed is the deliberate exception: without one account holding
            // `users.permissions`, nobody could ever grant anything. `granted_by` is `None`:
            // the installation granted these, not another account.
            for permission in tankovault_domain::Permission::all() {
                tankovault_db::repo::permissions::grant(pool, u.id, *permission, None).await?;
            }
            // Printed on purpose: this bootstrap command's whole output is the account you can
            // now log in with, so `expose_secret` here is deliberate.
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

    // Built-in provider presets. Operators are responsible for the legality of crawling;
    // disable or retarget any provider via the admin console.
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

/// The workspace root, derived from this crate's manifest directory (`xtask` sits directly
/// under it), so this works regardless of the shell's current directory.
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
        // `type` gets its own strategy rather than falling out of the recursive generator: a
        // non-string `type` (e.g. `[null]`) is invalid OpenAPI the converter isn't idempotent
        // on (see `a_non_string_type_member_is_not_idempotent`), so this must stay well-formed.
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
        // Any other version is left alone, so a future utoipa emitting 3.2 fails visibly
        // instead of being silently mislabelled as converted.
        assert_eq!(
            downgraded(json!({ "openapi": "3.0.3" }))["openapi"],
            "3.0.3"
        );
    }

    #[test]
    fn a_nullable_union_becomes_a_type_plus_a_nullable_flag() {
        // The conversion this function exists for; getting it wrong makes every optional field
        // on the generated client either non-optional or untyped.
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

    /// Pins a **lossy** conversion: a union of three-plus types collapses to the first
    /// non-null one, silently discarding the rest. Deliberate, not a bug fix waiting to happen.
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

    /// Found by the idempotence property below; pinned rather than fixed.
    ///
    /// The "pick the first non-null type" branch inserts whatever it found without checking
    /// it's a string, so a non-string member survives one pass and is dropped by a second:
    /// `downgrade(downgrade(v))` != `downgrade(v)`. No impact today — `utoipa` never emits this
    /// shape and the converter runs once per document — but it is live the moment anything
    /// runs it twice or feeds it a hand-edited spec.
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
        /// Idempotence: `openapi --check` diffs a freshly generated artifact against the
        /// committed one through this same function, so non-idempotence would let the two
        /// silently drift apart with nothing else noticing.
        #[test]
        fn the_downgrade_is_idempotent(document in any_document()) {
            let once = downgraded(document);
            let twice = downgraded(once.clone());
            prop_assert_eq!(once, twice);
        }

        /// The rewriter touches only `openapi`, `type` and `examples`; everything else must
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
