//! The configuration surface, read out of the config structs themselves.
//!
//! Two sources, because the codebase has two ways of reading configuration and both can drift
//! from the document:
//!
//! 1. [`Table`] parses `#[derive(Deserialize)]` structs and [`walk`] descends from each
//!    service's root `Config` into the shared blocks it composes, emitting one environment key
//!    per leaf field. This is the layered surface — everything `tankovault_config::load` reads.
//! 2. [`direct_env_keys`] finds `std::env::var("TANKOVAULT_…")` call sites. Those bypass the
//!    layering entirely (`TANKOVAULT_PROFILE`, `TANKOVAULT_CONFIRM_RESET`), so no amount of
//!    struct walking would ever see them — and `TANKOVAULT_CONFIRM_RESET` being undocumented is
//!    precisely what `BUILD_AND_OPS` §10.3 found by hand.
//!
//! The walker models the subset of `serde` this repository actually uses and **refuses** the
//! rest rather than guessing: a `#[serde(flatten)]` or a struct-level `rename_all` silently
//! rewrites the key path, so meeting one is a hard error telling the reader to teach the walker
//! first. A gate that quietly mis-derives is worse than no gate, because the document it blesses
//! is then wrong with a green tick next to it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context as _, Result, bail};

/// One struct the walker can descend into.
struct Block {
    /// Where it was parsed from. Only used to name both sides of a collision.
    origin: String,
    fields: Vec<Field>,
}

/// One field of a [`Block`], reduced to what key derivation needs.
struct Field {
    /// The key figment matches: the field name, or its `#[serde(rename = "…")]`.
    key: String,
    /// The declared type, `Option`/`Box` already unwrapped. `None` for a type that cannot
    /// name a struct at all (a tuple, a reference), which is always a leaf.
    ty: Option<TypeRef>,
}

/// A field's type path, reduced to the qualifier and the name.
struct TypeRef {
    /// The segment before the name, when the type was written qualified
    /// (`tankovault_config` in `tankovault_config::SecurityConfig`).
    qualifier: Option<String>,
    /// The final segment.
    name: String,
}

/// Every `#[derive(Deserialize)]` struct parsed from a set of source files, by name.
///
/// A name can map to more than one definition — two services may each declare a `Config`, and
/// nothing stops an unrelated DTO from colliding with a config block. The ambiguity is only an
/// error if a walk actually reaches it, so it is recorded rather than rejected on sight.
#[derive(Default)]
pub(super) struct Table {
    blocks: BTreeMap<String, Vec<Block>>,
}

impl Table {
    /// Parse every `.rs` file under `dir`, recursively.
    ///
    /// # Errors
    /// An unreadable file, a file that does not parse as Rust, or a `serde` attribute the
    /// walker cannot model.
    pub(super) fn parse_dir(&mut self, dir: &Path) -> Result<()> {
        for path in rust_sources(dir)? {
            self.parse_file(&path)?;
        }
        Ok(())
    }

    /// Parse one `.rs` file.
    ///
    /// # Errors
    /// As [`Self::parse_dir`].
    pub(super) fn parse_file(&mut self, path: &Path) -> Result<()> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let file = syn::parse_file(&text)
            .with_context(|| format!("parsing {} as Rust", path.display()))?;
        let origin = path.display().to_string();
        collect(&file.items, &origin, &mut self.blocks)
            .with_context(|| format!("in {}", path.display()))
    }

    fn get(&self, name: &str) -> Option<&[Block]> {
        self.blocks.get(name).map(Vec::as_slice)
    }
}

/// Every environment key a service reads through [`tankovault_config::load`], derived by
/// descending from its root config struct.
///
/// `local` is that service's own source; `shared` is `crates/config` plus the one domain type a
/// config struct names. A bare type name resolves against `local` first and `shared` second,
/// which is how `DatabaseConfig` (imported) and `AuthConfig` (declared next to `Config`) can sit
/// in the same struct — but a name present in *both* is an error rather than a coin toss.
///
/// # Errors
/// An unresolvable ambiguity, a `serde` attribute the walker cannot model, or a cycle.
pub(super) fn walk(local: &Table, shared: &Table, root: &str) -> Result<BTreeSet<String>> {
    let mut keys = BTreeSet::new();
    let mut path = Vec::new();
    let mut stack = Vec::new();
    descend(local, shared, root, None, &mut path, &mut stack, &mut keys)?;
    Ok(keys)
}

fn descend(
    local: &Table,
    shared: &Table,
    name: &str,
    qualifier: Option<&str>,
    path: &mut Vec<String>,
    stack: &mut Vec<String>,
    keys: &mut BTreeSet<String>,
) -> Result<()> {
    if stack.iter().any(|s| s == name) {
        bail!(
            "config struct `{name}` contains itself: {} -> {name}",
            stack.join(" -> ")
        );
    }
    let block = resolve(local, shared, name, qualifier)?
        .ok_or_else(|| anyhow::anyhow!("no `#[derive(Deserialize)]` struct named `{name}`"))?;

    stack.push(name.to_owned());
    for field in &block.fields {
        path.push(field.key.clone());
        // A field whose type names a struct in either table is a nested block; anything else
        // — a scalar, a `Vec`, an enum — is a value figment parses directly.
        let nested = match field.ty.as_ref() {
            Some(ty) => resolve(local, shared, &ty.name, ty.qualifier.as_deref())?.and(Some(ty)),
            None => None,
        };
        if let Some(ty) = nested {
            descend(
                local,
                shared,
                &ty.name,
                ty.qualifier.as_deref(),
                path,
                stack,
                keys,
            )?;
        } else {
            keys.insert(format!("TANKOVAULT_{}", path.join("__").to_uppercase()));
        }
        path.pop();
    }
    stack.pop();
    Ok(())
}

/// Look a type name up in the two tables.
///
/// `Ok(None)` means "not a config block" — a `String`, a `Vec`, an enum — which is the common
/// case and is what makes a field a leaf.
fn resolve<'a>(
    local: &'a Table,
    shared: &'a Table,
    name: &str,
    qualifier: Option<&str>,
) -> Result<Option<&'a Block>> {
    // A type written `tankovault_config::X` names the shared crate and nothing else, so a
    // service-local struct of the same name cannot shadow it.
    if qualifier.is_some_and(|q| q.starts_with("tankovault_")) {
        return one(shared.get(name), name);
    }
    match (local.get(name), shared.get(name)) {
        (Some(_), Some(_)) => bail!(
            "`{name}` is declared both in this service and in the shared config crate; \
             write the field's type as a qualified path so which one is meant is not a guess"
        ),
        (Some(blocks), None) | (None, Some(blocks)) => one(Some(blocks), name),
        (None, None) => Ok(None),
    }
}

fn one<'a>(blocks: Option<&'a [Block]>, name: &str) -> Result<Option<&'a Block>> {
    match blocks {
        None | Some([]) => Ok(None),
        Some([only]) => Ok(Some(only)),
        Some(many) => bail!(
            "`{name}` is declared {} times ({}); the walker cannot tell which one a field means",
            many.len(),
            many.iter()
                .map(|b| b.origin.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// --- parsing -------------------------------------------------------------------------------

fn collect(
    items: &[syn::Item],
    origin: &str,
    out: &mut BTreeMap<String, Vec<Block>>,
) -> Result<()> {
    for item in items {
        match item {
            // Inline `mod x { … }`; a `mod x;` is reached as its own file by `parse_dir`.
            syn::Item::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    collect(inner, origin, out)?;
                }
            }
            syn::Item::Struct(s) => {
                if let Some(block) =
                    block_from(s, origin).with_context(|| format!("struct `{}`", s.ident))?
                {
                    out.entry(s.ident.to_string()).or_default().push(block);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// A struct becomes a [`Block`] only if it derives `Deserialize` and has named fields.
///
/// The derive requirement is what keeps the hundreds of unrelated structs in a service's tree
/// out of the table, and it is not merely a filter: a struct that does not derive `Deserialize`
/// cannot be a configuration block in the first place.
fn block_from(item: &syn::ItemStruct, origin: &str) -> Result<Option<Block>> {
    let syn::Fields::Named(named) = &item.fields else {
        return Ok(None);
    };
    if !derives_deserialize(&item.attrs) {
        return Ok(None);
    }
    reject_container_renames(&item.attrs)?;

    let mut fields = Vec::with_capacity(named.named.len());
    for field in &named.named {
        let ident = field
            .ident
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("named fields have idents"))?;
        let meta = field_serde(&field.attrs).with_context(|| format!("field `{ident}`"))?;
        if meta.skip {
            continue;
        }
        fields.push(Field {
            key: meta.rename.unwrap_or_else(|| ident.to_string()),
            ty: type_ref(&field.ty),
        });
    }
    Ok(Some(Block {
        origin: origin.to_owned(),
        fields,
    }))
}

fn derives_deserialize(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let mut found = false;
        attr.path().is_ident("derive")
            && attr
                .parse_nested_meta(|meta| {
                    if meta
                        .path
                        .segments
                        .last()
                        .is_some_and(|s| s.ident == "Deserialize")
                    {
                        found = true;
                    }
                    Ok(())
                })
                .is_ok()
            && found
    })
}

/// Container-level `serde` attributes that would rewrite every key beneath them.
fn reject_container_renames(attrs: &[syn::Attribute]) -> Result<()> {
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        let mut offender = None;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") || meta.path.is_ident("rename_all_fields") {
                offender = meta.path.get_ident().map(ToString::to_string);
            }
            skip_value(&meta)
        })?;
        if let Some(name) = offender {
            bail!(
                "`#[serde({name} = …)]` rewrites every key in this struct; teach \
                 `config_docs::surface` how before using it, or the derived surface is wrong"
            );
        }
    }
    Ok(())
}

#[derive(Default)]
struct FieldMeta {
    rename: Option<String>,
    skip: bool,
}

fn field_serde(attrs: &[syn::Attribute]) -> Result<FieldMeta> {
    let mut out = FieldMeta::default();
    let mut unsupported = None;
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                out.rename = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("skip") || meta.path.is_ident("skip_deserializing") {
                out.skip = true;
            } else if meta.path.is_ident("flatten") || meta.path.is_ident("alias") {
                unsupported = meta.path.get_ident().map(ToString::to_string);
                skip_value(&meta)?;
            } else {
                skip_value(&meta)?;
            }
            Ok(())
        })?;
    }
    if let Some(name) = unsupported {
        bail!(
            "`#[serde({name})]` changes which keys this field answers to; teach \
             `config_docs::surface` how before using it, or the derived surface is wrong"
        );
    }
    Ok(out)
}

/// Consume a `= value` or `(…)` payload so `parse_nested_meta` can reach the next entry.
///
/// Without this, an attribute the walker does not care about (`default = "…"`,
/// `deserialize_with = "…"`) is a parse error rather than something ignored.
fn skip_value(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(syn::Token![=]) {
        meta.value()?.parse::<syn::Expr>()?;
    } else if meta.input.peek(syn::token::Paren) {
        meta.parse_nested_meta(|nested| skip_value(&nested))?;
    }
    Ok(())
}

/// Reduce a declared type to the name a lookup can use.
///
/// `Option<T>` and `Box<T>` are transparent to figment, so they unwrap — `Option<NatsConfig>`
/// on the API's `Config` still contributes `TANKOVAULT_NATS__URL`. `Vec<T>` and `HashMap<K, V>`
/// deliberately do not: a list is one value figment parses as JSON, not a nested block.
fn type_ref(ty: &syn::Type) -> Option<TypeRef> {
    let syn::Type::Path(path) = ty else {
        return None;
    };
    let segments = &path.path.segments;
    let last = segments.last()?;
    if last.ident == "Option" || last.ident == "Box" {
        let syn::PathArguments::AngleBracketed(args) = &last.arguments else {
            return None;
        };
        return args.args.iter().find_map(|arg| match arg {
            syn::GenericArgument::Type(inner) => type_ref(inner),
            _ => None,
        });
    }
    Some(TypeRef {
        qualifier: segments
            .len()
            .checked_sub(2)
            .map(|i| segments[i].ident.to_string()),
        name: last.ident.to_string(),
    })
}

// --- direct environment reads --------------------------------------------------------------

/// `TANKOVAULT_*` keys read straight from the environment rather than through the layered
/// config, found by their `std::env::var` call site.
///
/// Textual on purpose. These are `env::var("LITERAL")` calls, so the literal *is* the key and
/// a parser would add nothing; what matters is that the list is derived rather than kept by
/// hand, which is how `TANKOVAULT_CONFIRM_RESET` went undocumented until someone read for it.
///
/// # Errors
/// An unreadable source file.
pub(super) fn direct_env_keys(dirs: &[std::path::PathBuf]) -> Result<BTreeSet<String>> {
    const CALLS: [&str; 2] = ["env::var(\"", "env::var_os(\""];
    let mut keys = BTreeSet::new();
    for dir in dirs {
        for path in rust_sources(dir)? {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            for call in CALLS {
                for (offset, _) in text.match_indices(call) {
                    let rest = &text[offset + call.len()..];
                    let Some(end) = rest.find('"') else { continue };
                    let literal = &rest[..end];
                    // A key, not prose about one. Without this the scan reads its own
                    // documentation — `env::var("TANKOVAULT_…")` above is a literal too.
                    if literal.starts_with("TANKOVAULT_") && is_key_text(literal) {
                        keys.insert(literal.to_owned());
                    }
                }
            }
        }
    }
    Ok(keys)
}

/// Whether a string literal is a bare environment key rather than prose mentioning one.
fn is_key_text(literal: &str) -> bool {
    literal
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Every `.rs` file under `dir`, recursively, in a stable order.
fn rust_sources(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut out = Vec::new();
    let mut queue = vec![dir.to_path_buf()];
    while let Some(current) = queue.pop() {
        let entries = std::fs::read_dir(&current)
            .with_context(|| format!("reading {}", current.display()))?;
        for entry in entries {
            let path = entry?.path();
            if path.is_dir() {
                queue.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{Table, walk};

    fn table(source: &str) -> Table {
        let mut table = Table::default();
        let file = syn::parse_file(source).expect("test source parses");
        super::collect(&file.items, "<test>", &mut table.blocks).expect("test source is modelled");
        table
    }

    fn keys(source: &str) -> Vec<String> {
        walk(&table(source), &Table::default(), "Config")
            .expect("walk succeeds")
            .into_iter()
            .collect()
    }

    /// The shape every service's root config has: scalars beside nested blocks.
    #[test]
    fn a_nested_block_becomes_a_double_underscore_path() {
        assert_eq!(
            keys(
                r"
                #[derive(Deserialize)]
                struct Config { bind_addr: String, database: DatabaseConfig }
                #[derive(Deserialize)]
                struct DatabaseConfig { url: String, max_connections: u32 }
            "
            ),
            [
                "TANKOVAULT_BIND_ADDR",
                "TANKOVAULT_DATABASE__MAX_CONNECTIONS",
                "TANKOVAULT_DATABASE__URL",
            ]
        );
    }

    /// `Option<T>` is transparent to figment, so the API's optional `nats` block still
    /// contributes its keys. A `Vec<T>` is not: it is one value, parsed as JSON.
    #[test]
    fn option_unwraps_and_vec_does_not() {
        assert_eq!(
            keys(
                r"
                #[derive(Deserialize)]
                struct Config { nats: Option<NatsConfig>, origins: Vec<NatsConfig> }
                #[derive(Deserialize)]
                struct NatsConfig { url: String }
            "
            ),
            ["TANKOVAULT_NATS__URL", "TANKOVAULT_ORIGINS"]
        );
    }

    /// A struct with no `Deserialize` cannot be a configuration block, and keeping it out of
    /// the table is what lets a service's whole `src/` tree be parsed without its DTOs
    /// colliding with the config blocks.
    #[test]
    fn a_struct_without_deserialize_is_a_leaf() {
        assert_eq!(
            keys(
                r"
                #[derive(Deserialize)]
                struct Config { render: RenderConfig }
                struct RenderConfig { chrome_path: String }
            "
            ),
            ["TANKOVAULT_RENDER"]
        );
    }

    /// `#[serde(rename)]` decides the key; `#[serde(skip)]` removes it. Both are what figment
    /// actually matches, so the derived surface must follow them rather than the field name.
    #[test]
    fn rename_and_skip_follow_serde_not_the_field_name() {
        assert_eq!(
            keys(
                r#"
                #[derive(Deserialize)]
                struct Config {
                    #[serde(rename = "bind_addr")] listen: String,
                    #[serde(skip)] runtime_only: String,
                    #[serde(default = "default_port")] port: u16,
                }
            "#
            ),
            ["TANKOVAULT_BIND_ADDR", "TANKOVAULT_PORT"]
        );
    }

    /// A `flatten` lifts its fields into the parent's key space, so every key beneath it would
    /// be derived one level too deep. Refusing is the point: a gate that mis-derives blesses a
    /// wrong document with a green tick.
    #[test]
    fn flatten_is_refused_rather_than_guessed_at() {
        let mut t = Table::default();
        let file = syn::parse_file(
            r"
                #[derive(Deserialize)]
                struct Config { #[serde(flatten)] inner: Inner }
            ",
        )
        .expect("parses");
        let err = super::collect(&file.items, "<test>", &mut t.blocks)
            .expect_err("flatten is not modelled");
        assert!(format!("{err:#}").contains("flatten"), "{err:#}");
    }

    /// Likewise for a container-level `rename_all`, which rewrites every key at once.
    #[test]
    fn container_rename_all_is_refused() {
        let mut t = Table::default();
        let file = syn::parse_file(
            r#"
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Config { bind_addr: String }
            "#,
        )
        .expect("parses");
        let err = super::collect(&file.items, "<test>", &mut t.blocks)
            .expect_err("rename_all is not modelled");
        assert!(format!("{err:#}").contains("rename_all"), "{err:#}");
    }

    /// A name in both tables is an error rather than a silent pick. Local-first resolution is
    /// what lets an imported `DatabaseConfig` and a locally declared `AuthConfig` share a
    /// struct; it must not become a way for an unrelated DTO to shadow a shared block.
    #[test]
    fn a_name_in_both_tables_is_an_error() {
        let local = table(
            r"
            #[derive(Deserialize)]
            struct Config { security: SecurityConfig }
            #[derive(Deserialize)]
            struct SecurityConfig { unrelated: String }
        ",
        );
        let shared = table(
            r"
            #[derive(Deserialize)]
            struct SecurityConfig { hsts: bool }
        ",
        );
        let err = walk(&local, &shared, "Config").expect_err("the collision is reported");
        assert!(format!("{err:#}").contains("qualified path"), "{err:#}");
    }

    /// A qualified `tankovault_config::X` names the shared crate, so it resolves there even
    /// when the service declares its own `X`.
    #[test]
    fn a_qualified_path_resolves_to_the_shared_crate() {
        let local = table(
            r"
            #[derive(Deserialize)]
            struct Config { security: tankovault_config::SecurityConfig }
            #[derive(Deserialize)]
            struct SecurityConfig { unrelated: String }
        ",
        );
        let shared = table(
            r"
            #[derive(Deserialize)]
            struct SecurityConfig { hsts: bool }
        ",
        );
        assert_eq!(
            walk(&local, &shared, "Config")
                .expect("resolves")
                .into_iter()
                .collect::<Vec<_>>(),
            ["TANKOVAULT_SECURITY__HSTS"]
        );
    }

    /// A config struct that contains itself would otherwise recurse forever.
    #[test]
    fn a_cycle_is_reported_not_followed() {
        let err = walk(
            &table(
                r"
                #[derive(Deserialize)]
                struct Config { inner: Inner }
                #[derive(Deserialize)]
                struct Inner { back: Config }
            ",
            ),
            &Table::default(),
            "Config",
        )
        .expect_err("the cycle is reported");
        assert!(format!("{err:#}").contains("contains itself"), "{err:#}");
    }
}
