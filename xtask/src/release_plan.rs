//! `xtask release-plan` — which images a release actually has to rebuild.
//!
//! A release used to publish all nine images unconditionally. This decides the set instead, from
//! the workspace dependency graph plus a table of the paths that are inputs to a build without
//! belonging to any package. Everything it does not recognise counts as a change to everything:
//! the cost of an unnecessary rebuild is runner minutes, and the cost of a missed one is a
//! service shipping stale code under a version that claims otherwise.
//!
//! Each image is diffed from **the tag it is currently published at**, not from the previous
//! release. A leg that failed at `v1.0.1` is therefore picked up at `v1.0.2` instead of being
//! skipped for good.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use crate::repo_lint;

/// The value a normalised version line is rewritten to. Any constant works; it only has to be
/// the same on both sides of the comparison.
const MASKED_VERSION: &str = "version = \"0.0.0-masked\"";

/// What one changed path can reach.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Impact {
    /// Nothing a published image is built from.
    Inert,
    /// Every image.
    Every,
    /// Whatever is built from this workspace package.
    Package(String),
    /// The SPA's own sources: the `frontend` image alone.
    Spa,
    /// Recognised by nothing. Treated as [`Impact::Every`] and reported, because the alternative
    /// is a silent miss. `repo-lint`'s `build-inputs-are-classified` rule exists to keep this
    /// from ever firing in practice.
    Unclassified,
}

/// A prefix rule. Directory prefixes end in `/`; anything else matches the whole path.
enum Rule {
    Inert,
    Every,
    Package(&'static str),
    Spa,
}

/// Paths that are build inputs without belonging to a workspace package, and paths that look like
/// inputs but reach no image. Longest prefix wins, so `deploy/docker/` overrides `deploy/`.
///
/// `crates/`, `services/` and `xtask/` are deliberately absent: those fall through to package
/// ownership, which is derived from `cargo metadata` rather than listed here.
const RULES: &[(&str, Rule)] = &[
    // The build definition itself, and the files every runtime stage copies.
    ("deploy/docker/", Rule::Every),
    ("LICENSE", Rule::Every),
    ("THIRD-PARTY-NOTICES", Rule::Every),
    (".dockerignore", Rule::Every),
    // The directory default stays `Every` even though nothing in it currently affects a build: a
    // `.cargo/config.toml` added later carries rustflags and linker settings, and the failure of
    // guessing wrong there is silent. The two files that are gate configuration are carved out by
    // name, because they change often and rebuilding nine images for a dated advisory exception
    // is the whole cost this command exists to avoid.
    (".cargo/", Rule::Every),
    (".cargo/audit.toml", Rule::Inert),
    (".cargo/mutants.toml", Rule::Inert),
    ("rust-toolchain.toml", Rule::Every),
    // Both carry a workspace-wide version that release-please rewrites on every release commit,
    // so both go through `version_only_change` before this verdict is believed.
    ("Cargo.toml", Rule::Every),
    ("Cargo.lock", Rule::Every),
    // The generator that writes the configuration contract every runtime stage copies in. Package
    // ownership would say the opposite of the truth here: nothing depends on this crate — it
    // depends on all nine services — so the graph maps it to no image at all, while its output is
    // inside every one of them. A changed `External` declaration or a renamed service is a change
    // to every published image.
    ("crates/config-contract/", Rule::Every),
    // Root-level inputs that belong to a package without living inside it.
    ("migrations/", Rule::Package("tankovault-db")),
    (".sqlx/", Rule::Package("tankovault-db")),
    ("openapi.json", Rule::Package("tankovault-api-client")),
    // The SPA is its own workspace; `spa_roots` connects it back to this one.
    ("web/", Rule::Spa),
    // Inputs to a gate, a document or an editor — never to an image. A path git *ignores* needs no
    // entry: it cannot reach the diff this classifies, and `build-inputs-are-classified` exempts
    // it — which is why `.idea/` and `.vscode/` are deliberately absent rather than overlooked.
    // The build-output entries below predate that exemption and stay for its no-git fallback.
    ("deploy/", Rule::Inert),
    (".github/", Rule::Inert),
    ("docs/", Rule::Inert),
    (".claude/", Rule::Inert),
    (".junie/", Rule::Inert),
    (".serena/", Rule::Inert),
    ("fuzz/", Rule::Inert),
    ("target/", Rule::Inert),
    ("mutants.out/", Rule::Inert),
    ("mutants.out.old/", Rule::Inert),
    (".gitignore", Rule::Inert),
    (".gitattributes", Rule::Inert),
    ("about.toml", Rule::Inert),
    ("deny.toml", Rule::Inert),
    ("clippy.toml", Rule::Inert),
    ("rustfmt.toml", Rule::Inert),
    ("renovate.json", Rule::Inert),
    ("release-please-config.json", Rule::Inert),
    (".release-please-manifest.json", Rule::Inert),
];

/// Run the planner and write its outputs.
///
/// `bases` is a JSON object mapping each deployable binary to the git ref its published image was
/// built from; a binary that is absent, or maps to an empty string, has never been published and
/// is always rebuilt. With `all`, every deployable binary is selected and `bases` is not read.
///
/// # Errors
/// A missing Dockerfile or root manifest, an unreadable or malformed `bases` document, or a `git`
/// invocation that fails.
pub(crate) fn run(root: &Path, bases: Option<&Path>, all: bool) -> anyhow::Result<()> {
    let deployable = deployable_bins(root)?;
    let selected = if all {
        eprintln!("release-plan: rebuilding every image (--all)");
        deployable.clone()
    } else {
        let path = bases.ok_or_else(|| {
            anyhow::anyhow!("release-plan: a bases document is required unless --all is given")
        })?;
        select(root, &deployable, path)?
    };

    // By construction, but the assertion is what keeps a future edit from publishing a binary the
    // deploy blacklist excludes.
    if let Some(stray) = selected.iter().find(|bin| !deployable.contains(*bin)) {
        anyhow::bail!(
            "release-plan: selected `{stray}`, which is not a deployable binary; the Dockerfile's \
             SERVICE_BINS and the deploy blacklist decide that set"
        );
    }

    emit(&selected)
}

/// Decide the rebuild set from each binary's own base ref.
fn select(root: &Path, deployable: &[String], bases: &Path) -> anyhow::Result<Vec<String>> {
    let workspace = Workspace::load(root)?;
    let document = std::fs::read_to_string(bases).map_err(|error| {
        anyhow::anyhow!("release-plan: cannot read {}: {error}", bases.display())
    })?;
    let bases: BTreeMap<String, String> = serde_json::from_str(&document).map_err(|error| {
        anyhow::anyhow!(
            "release-plan: {} is not a JSON object of bins to refs: {error}",
            document.trim()
        )
    })?;

    let mut selected = BTreeSet::new();
    let mut by_base: BTreeMap<&str, Vec<&String>> = BTreeMap::new();
    for bin in deployable {
        match bases.get(bin).map(String::as_str) {
            Some(base) if !base.is_empty() => by_base.entry(base).or_default().push(bin),
            _ => {
                eprintln!(
                    "release-plan: {bin} has no published image to compare against; rebuilding"
                );
                selected.insert(bin.clone());
            }
        }
    }

    for (base, bins) in by_base {
        let impacts = impacts_since(root, base, &workspace)?;
        for bin in bins {
            if let Some(reason) = workspace.hit(bin, &impacts) {
                eprintln!("release-plan: {bin} changed since {base} ({reason})");
                selected.insert(bin.clone());
            } else {
                eprintln!("release-plan: {bin} unchanged since {base}");
            }
        }
    }

    Ok(selected.into_iter().collect())
}

/// Write `images`, `service_bins` and `any` to `$GITHUB_OUTPUT` when it is set, and to stdout
/// always.
fn emit(selected: &[String]) -> anyhow::Result<()> {
    let images = serde_json::to_string(selected)?;
    let service_bins = selected.join(" ");
    let any = if selected.is_empty() { "false" } else { "true" };
    let block = format!("images={images}\nservice_bins={service_bins}\nany={any}\n");

    print!("{block}");
    if let Some(path) = std::env::var_os("GITHUB_OUTPUT") {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        file.write_all(block.as_bytes())?;
    }
    Ok(())
}

/// The binaries an image is published for: the Dockerfile's `SERVICE_BINS` minus the deploy
/// blacklist. Read rather than listed, so the release workflow can no longer name a set that has
/// drifted from either.
fn deployable_bins(root: &Path) -> anyhow::Result<Vec<String>> {
    let dockerfile = root.join("deploy/docker/Dockerfile");
    let text = std::fs::read_to_string(&dockerfile).map_err(|error| {
        anyhow::anyhow!(
            "release-plan: cannot read {}: {error}",
            dockerfile.display()
        )
    })?;
    let Some((_, built)) = repo_lint::service_bins(&text) else {
        anyhow::bail!(
            "release-plan: {} declares no `ARG SERVICE_BINS=\"…\"`, so there is no set to publish \
             from",
            dockerfile.display()
        );
    };

    let manifest_path = root.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path).map_err(|error| {
        anyhow::anyhow!(
            "release-plan: cannot read {}: {error}",
            manifest_path.display()
        )
    })?;
    let excluded = repo_lint::deploy_exclusions(&manifest);

    let mut bins: Vec<String> = built
        .into_iter()
        .filter(|bin| !excluded.iter().any(|entry| &entry.bin == bin))
        .collect();
    bins.sort();
    Ok(bins)
}

// ---------------------------------------------------------------------------------------
// The workspace graph
// ---------------------------------------------------------------------------------------

/// One workspace package, reduced to what the planner needs.
struct Package {
    name: String,
    /// Repo-relative, `/`-separated, no trailing slash.
    dir: String,
    bins: Vec<String>,
    /// Workspace-internal, non-dev dependencies. Dev edges are dropped on purpose: a
    /// `dev-dependency` is compiled for tests, never for `cargo build --bin`, so a change to
    /// `crates/test-support` cannot reach a published image.
    deps: Vec<String>,
}

struct Workspace {
    packages: Vec<Package>,
    /// Host-workspace packages the SPA depends on. The SPA is a separate workspace, so no
    /// `cargo metadata` run covers both; these are read from its manifest instead.
    spa_roots: Vec<String>,
}

impl Workspace {
    fn load(root: &Path) -> anyhow::Result<Self> {
        let packages = cargo_metadata(root)?;
        let spa_roots = spa_roots(root, &packages);
        Ok(Self {
            packages,
            spa_roots,
        })
    }

    fn by_name(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|package| package.name == name)
    }

    fn owning_bin(&self, bin: &str) -> Option<&Package> {
        self.packages
            .iter()
            .find(|package| package.bins.iter().any(|target| target == bin))
    }

    /// Every package `start` is transitively built from, including itself.
    fn dependency_closure(&self, start: &str, into: &mut BTreeSet<String>) {
        if !into.insert(start.to_owned()) {
            return;
        }
        let Some(package) = self.by_name(start) else {
            return;
        };
        for dep in &package.deps {
            self.dependency_closure(dep, into);
        }
    }

    /// The packages one image is built from. The `frontend` image is the only one with two roots:
    /// the server binary that serves the SPA, and the SPA itself, whose wasm half pulls
    /// `api-client` and `domain` out of this workspace.
    fn reach(&self, bin: &str) -> BTreeSet<String> {
        let mut reach = BTreeSet::new();
        if let Some(package) = self.owning_bin(bin) {
            self.dependency_closure(&package.name, &mut reach);
        }
        if bin == "frontend" {
            for spa_root in &self.spa_roots {
                self.dependency_closure(spa_root, &mut reach);
            }
        }
        reach
    }

    /// Why `bin` has to be rebuilt, or `None` if nothing that reaches it changed.
    fn hit(&self, bin: &str, impacts: &[(String, Impact)]) -> Option<String> {
        let reach = self.reach(bin);
        for (path, impact) in impacts {
            let hit = match impact {
                Impact::Inert => false,
                Impact::Every | Impact::Unclassified => true,
                Impact::Spa => bin == "frontend",
                Impact::Package(name) => reach.contains(name),
            };
            if hit {
                return Some(path.clone());
            }
        }
        None
    }
}

/// The workspace's packages, via `cargo metadata --no-deps`.
fn cargo_metadata(root: &Path) -> anyhow::Result<Vec<Package>> {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|error| anyhow::anyhow!("release-plan: cannot run `cargo metadata`: {error}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "release-plan: `cargo metadata` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let root_dir = metadata["workspace_root"].as_str().ok_or_else(|| {
        anyhow::anyhow!("release-plan: `cargo metadata` reported no workspace_root")
    })?;
    let root_dir = normalise(root_dir);

    let entries = metadata["packages"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("release-plan: `cargo metadata` reported no packages"))?;

    let mut packages = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = entry["name"].as_str().unwrap_or_default().to_owned();
        let manifest = normalise(entry["manifest_path"].as_str().unwrap_or_default());
        let dir = manifest
            .strip_prefix(&format!("{root_dir}/"))
            .and_then(|relative| relative.strip_suffix("/Cargo.toml"))
            .unwrap_or_default()
            .to_owned();

        let bins = entry["targets"]
            .as_array()
            .map(|targets| {
                targets
                    .iter()
                    .filter(|target| {
                        target["kind"]
                            .as_array()
                            .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
                    })
                    .filter_map(|target| target["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let deps = entry["dependencies"]
            .as_array()
            .map(|dependencies| {
                dependencies
                    .iter()
                    .filter(|dependency| dependency["path"].is_string())
                    .filter(|dependency| dependency["kind"].as_str() != Some("dev"))
                    .filter_map(|dependency| dependency["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        packages.push(Package {
            name,
            dir,
            bins,
            deps,
        });
    }
    Ok(packages)
}

/// The host-workspace packages `web/frontend` path-depends on.
///
/// Its manifest is read directly rather than through `cargo metadata`: it is an excluded member
/// (`exclude = ["web/frontend", …]`), so it is its own workspace on its own toolchain, and
/// resolving it needs a wasm target this command has no reason to require.
fn spa_roots(root: &Path, packages: &[Package]) -> Vec<String> {
    let manifest_path = root.join("web/frontend/Cargo.toml");
    let Ok(manifest) = std::fs::read_to_string(&manifest_path) else {
        return Vec::new();
    };

    let mut roots = Vec::new();
    for relative in path_dependencies(&manifest) {
        let Some(dir) = join_relative("web/frontend", &relative) else {
            continue;
        };
        if let Some(package) = packages.iter().find(|package| package.dir == dir) {
            roots.push(package.name.clone());
        }
    }
    roots
}

/// Every `path = "…"` value in one manifest.
fn path_dependencies(manifest: &str) -> Vec<String> {
    manifest
        .lines()
        .filter(|line| !is_comment(line))
        .filter_map(|line| line.split_once("path = \""))
        .filter_map(|(_, rest)| rest.split_once('"'))
        .map(|(value, _)| value.to_owned())
        .collect()
}

/// `base` joined with a `../`-relative path, normalised. `None` if it escapes the repository.
fn join_relative(base: &str, relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

fn normalise(path: &str) -> String {
    path.replace('\\', "/")
}

fn is_markdown(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// Whether `line` is a comment. The `RULES` table has to be describable in prose without a
/// manifest scan reading the description as a declaration.
fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('#') || trimmed.starts_with("//")
}

// ---------------------------------------------------------------------------------------
// The diff
// ---------------------------------------------------------------------------------------

/// Every path that changed between `base` and `HEAD`, with what it can reach.
fn impacts_since(
    root: &Path,
    base: &str,
    workspace: &Workspace,
) -> anyhow::Result<Vec<(String, Impact)>> {
    let paths = changed_paths(root, base)?;
    let members: BTreeSet<String> = workspace
        .packages
        .iter()
        .map(|package| package.name.clone())
        .collect();

    let mut impacts = Vec::with_capacity(paths.len());
    for path in paths {
        let mut impact = classify(&path, &workspace.packages);
        // Both files carry a workspace-wide version that release-please rewrites on the release
        // commit itself. Believed literally, that marks every image changed on every release and
        // this command becomes a no-op. The rewrite is safe to ignore because no binary reads
        // `CARGO_PKG_VERSION`; the version reaches an image only through `cargo auditable`'s
        // dependency-list section, which is audit metadata rather than behaviour.
        if matches!(path.as_str(), "Cargo.toml" | "Cargo.lock")
            && version_only_change(root, base, &path, &members)?
        {
            impact = Impact::Inert;
        }
        if impact == Impact::Unclassified {
            eprintln!(
                "release-plan: `{path}` matches no rule and belongs to no package; treating it as \
                 a change to every image. Add it to RULES (xtask/src/release_plan.rs)."
            );
        }
        impacts.push((path, impact));
    }
    Ok(impacts)
}

/// What one path can reach.
fn classify(path: &str, packages: &[Package]) -> Impact {
    // No runtime stage copies a document, and `.dockerignore`'s `**/*.md` keeps most of them out
    // of the build context entirely. `CHANGELOG.md` is the one that matters: release-please
    // rewrites it on the release commit.
    if is_markdown(path) {
        return Impact::Inert;
    }
    if let Some(rule) = longest_rule(path) {
        return match rule {
            Rule::Inert => Impact::Inert,
            Rule::Every => Impact::Every,
            Rule::Package(name) => Impact::Package((*name).to_owned()),
            Rule::Spa => Impact::Spa,
        };
    }
    // Package ownership, longest manifest directory first, so `services/api/test-support` wins
    // over `services/api`.
    packages
        .iter()
        .filter(|package| !package.dir.is_empty())
        .filter(|package| path.starts_with(&format!("{}/", package.dir)))
        .max_by_key(|package| package.dir.len())
        .map_or(Impact::Unclassified, |package| {
            Impact::Package(package.name.clone())
        })
}

/// The longest matching entry of [`RULES`].
fn longest_rule(path: &str) -> Option<&'static Rule> {
    RULES
        .iter()
        .filter(|(prefix, _)| {
            if let Some(directory) = prefix.strip_suffix('/') {
                path == directory || path.starts_with(prefix)
            } else {
                path == *prefix
            }
        })
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, rule)| rule)
}

/// Paths changed between `base` and `HEAD`.
///
/// `--no-renames` on purpose: with rename detection a file moved out of a crate is reported only
/// under its new path, so the crate it left would not be seen to have changed.
fn changed_paths(root: &Path, base: &str) -> anyhow::Result<Vec<String>> {
    let output = Command::new("git")
        .current_dir(root)
        .args([
            "diff",
            "--name-only",
            "--no-renames",
            &format!("{base}..HEAD"),
        ])
        .output()
        .map_err(|error| anyhow::anyhow!("release-plan: cannot run `git diff`: {error}"))?;
    if !output.status.success() {
        anyhow::bail!(
            "release-plan: `git diff {base}..HEAD` failed: {}. The base ref has to be fetched — \
             the release workflow checks out with `fetch-depth: 0` for this reason.",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .filter(|line| !line.is_empty())
        .collect())
}

/// Whether the only difference in `path` between `base` and the working tree is the workspace
/// version release-please rewrites.
fn version_only_change(
    root: &Path,
    base: &str,
    path: &str,
    members: &BTreeSet<String>,
) -> anyhow::Result<bool> {
    let Some(before) = show(root, base, path)? else {
        return Ok(false);
    };
    let Ok(after) = std::fs::read_to_string(root.join(path)) else {
        return Ok(false);
    };
    let mask = |text: &str| {
        if path == "Cargo.lock" {
            mask_lockfile_versions(text, members)
        } else {
            mask_workspace_version(text)
        }
    };
    Ok(mask(&before) == mask(&after))
}

/// One file's contents at `reference`, or `None` if it did not exist there.
fn show(root: &Path, reference: &str, path: &str) -> anyhow::Result<Option<String>> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["show", &format!("{reference}:{path}")])
        .output()
        .map_err(|error| anyhow::anyhow!("release-plan: cannot run `git show`: {error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// `Cargo.toml` with `[workspace.package] version` masked.
fn mask_workspace_version(manifest: &str) -> String {
    let mut out = String::with_capacity(manifest.len());
    let mut in_workspace_package = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_workspace_package = trimmed == "[workspace.package]";
        }
        if in_workspace_package && is_version_assignment(trimmed) {
            out.push_str(MASKED_VERSION);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// `Cargo.lock` with every workspace member's recorded version masked. Only members: a changed
/// third-party version is a real change to every image built from it.
fn mask_lockfile_versions(lockfile: &str, members: &BTreeSet<String>) -> String {
    let mut out = String::with_capacity(lockfile.len());
    let mut current_is_member = false;
    for line in lockfile.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            current_is_member = false;
        } else if let Some(name) = trimmed.strip_prefix("name = ") {
            current_is_member = members.contains(name.trim_matches('"'));
        }
        if current_is_member && is_version_assignment(trimmed) {
            out.push_str(MASKED_VERSION);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn is_version_assignment(trimmed: &str) -> bool {
    trimmed
        .strip_prefix("version")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

// ---------------------------------------------------------------------------------------
// Shared with repo-lint
// ---------------------------------------------------------------------------------------

/// Whether a top-level repository entry is something the planner can classify.
///
/// `member_roots` is the set of first path components of the workspace's members — the
/// directories package ownership covers. `repo-lint` calls this over the real tree so a new
/// top-level entry has to be given a verdict here before it can reach `main`.
pub(crate) fn top_level_is_classified(entry: &str, member_roots: &BTreeSet<String>) -> bool {
    if member_roots.contains(entry) || is_markdown(entry) {
        return true;
    }
    // Probed as a directory as well: a rule for `docs/` must classify the entry `docs`.
    longest_rule(entry).is_some() || longest_rule(&format!("{entry}/x")).is_some()
}

/// The first path component of every `members = [...]` entry in the root manifest.
///
/// A line read rather than `cargo metadata`, so `repo-lint` stays a set of file scans.
pub(crate) fn member_roots(manifest: &str) -> BTreeSet<String> {
    let mut roots = BTreeSet::new();
    let mut in_members = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if is_comment(trimmed) {
            continue;
        }
        if trimmed.starts_with("members") && trimmed.contains('[') {
            in_members = true;
            continue;
        }
        if !in_members {
            continue;
        }
        if trimmed.starts_with(']') {
            break;
        }
        let entry = trimmed.trim_end_matches(',').trim_matches('"');
        if let Some((root, _)) = entry.split_once('/') {
            roots.insert(root.to_owned());
        } else if !entry.is_empty() {
            roots.insert(entry.to_owned());
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packages() -> Vec<Package> {
        vec![
            Package {
                name: "tankovault-domain".to_owned(),
                dir: "crates/domain".to_owned(),
                bins: Vec::new(),
                deps: Vec::new(),
            },
            Package {
                name: "tankovault-db".to_owned(),
                dir: "crates/db".to_owned(),
                bins: Vec::new(),
                deps: vec!["tankovault-domain".to_owned()],
            },
            Package {
                name: "tankovault-api-client".to_owned(),
                dir: "crates/api-client".to_owned(),
                bins: Vec::new(),
                deps: vec!["tankovault-domain".to_owned()],
            },
            Package {
                name: "tankovault-api".to_owned(),
                dir: "services/api".to_owned(),
                bins: vec!["api".to_owned()],
                deps: vec!["tankovault-db".to_owned()],
            },
            Package {
                name: "tankovault-api-test-support".to_owned(),
                dir: "services/api/test-support".to_owned(),
                bins: Vec::new(),
                deps: Vec::new(),
            },
            Package {
                name: "tankovault-frontend".to_owned(),
                dir: "services/frontend".to_owned(),
                bins: vec!["frontend".to_owned()],
                deps: Vec::new(),
            },
            Package {
                name: "tankovault-render".to_owned(),
                dir: "services/render".to_owned(),
                bins: vec!["render".to_owned()],
                deps: Vec::new(),
            },
        ]
    }

    fn workspace() -> Workspace {
        Workspace {
            packages: packages(),
            spa_roots: vec!["tankovault-api-client".to_owned()],
        }
    }

    fn hit(bin: &str, path: &str) -> bool {
        let workspace = workspace();
        let impacts = vec![(path.to_owned(), classify(path, &workspace.packages))];
        workspace.hit(bin, &impacts).is_some()
    }

    #[test]
    fn a_shared_crate_reaches_every_image_that_depends_on_it() {
        assert!(hit("api", "crates/domain/src/lib.rs"));
        assert!(!hit("render", "crates/domain/src/lib.rs"));
    }

    #[test]
    fn a_service_reaches_only_its_own_image() {
        assert!(hit("api", "services/api/src/main.rs"));
        assert!(!hit("render", "services/api/src/main.rs"));
    }

    /// The nested member has to win the prefix match, or every change under
    /// `services/api/test-support` is attributed to `services/api` and rebuilds `api`.
    #[test]
    fn the_longest_manifest_directory_owns_a_path() {
        assert_eq!(
            classify("services/api/test-support/src/lib.rs", &packages()),
            Impact::Package("tankovault-api-test-support".to_owned())
        );
    }

    #[test]
    fn the_spa_reaches_the_frontend_image_alone() {
        assert!(hit("frontend", "web/frontend/src/main.rs"));
        assert!(!hit("api", "web/frontend/src/main.rs"));
    }

    /// `crates/api-client` is not a dependency of any *server* binary — the SPA is what uses it.
    /// Without `spa_roots`, a regenerated client would rebuild nothing at all.
    #[test]
    fn a_crate_only_the_spa_uses_still_reaches_the_frontend_image() {
        assert!(hit("frontend", "crates/api-client/src/lib.rs"));
        assert!(hit("frontend", "openapi.json"));
        assert!(!hit("render", "crates/api-client/src/lib.rs"));
    }

    #[test]
    fn migrations_are_owned_by_the_crate_that_embeds_them() {
        assert_eq!(
            classify("migrations/0001_init.up.sql", &packages()),
            Impact::Package("tankovault-db".to_owned())
        );
        assert!(hit("api", "migrations/0001_init.up.sql"));
        assert!(!hit("render", "migrations/0001_init.up.sql"));
    }

    /// `deploy/docker/` is the build definition and `deploy/` around it ships nothing, so the
    /// longer prefix has to win.
    #[test]
    fn the_dockerfile_reaches_everything_and_the_compose_stack_reaches_nothing() {
        assert_eq!(
            classify("deploy/docker/Dockerfile", &packages()),
            Impact::Every
        );
        assert_eq!(
            classify(
                "deploy/observability/prometheus/prometheus.yml",
                &packages()
            ),
            Impact::Inert
        );
    }

    /// `.cargo/` holds two gate configurations and, one day, possibly a `config.toml` that sets
    /// rustflags. The directory has to default to "rebuild everything" while the two files that
    /// change often — a dated advisory exception, a mutants filter — do not, or every such edit
    /// republishes nine images for nothing.
    #[test]
    fn cargo_gate_configuration_is_carved_out_of_the_cargo_directory() {
        assert_eq!(classify(".cargo/audit.toml", &packages()), Impact::Inert);
        assert_eq!(classify(".cargo/mutants.toml", &packages()), Impact::Inert);
        assert_eq!(classify(".cargo/config.toml", &packages()), Impact::Every);
    }

    #[test]
    fn documents_and_gate_configuration_reach_nothing() {
        for path in [
            "CHANGELOG.md",
            "docs/RELEASING.md",
            ".github/workflows/ci.yml",
            "clippy.toml",
            "renovate.json",
            ".release-please-manifest.json",
        ] {
            assert_eq!(classify(path, &packages()), Impact::Inert, "{path}");
        }
    }

    /// An unrecognised path is a change to everything. The alternative — assuming it is inert —
    /// is a service silently shipping stale code, which no later gate would catch.
    #[test]
    fn an_unrecognised_path_reaches_every_image() {
        assert_eq!(
            classify("newthing/config.yaml", &packages()),
            Impact::Unclassified
        );
        assert!(hit("render", "newthing/config.yaml"));
    }

    /// release-please rewrites `[workspace.package] version` on the release commit itself. Read
    /// literally that marks every image changed on every release, and the whole command becomes a
    /// no-op — so the version line is masked before the two revisions are compared.
    #[test]
    fn a_version_only_manifest_edit_masks_to_nothing() {
        let before = "[workspace.package]\nversion = \"0.4.1\"\nedition = \"2024\"\n";
        let after = "[workspace.package]\nversion = \"1.0.0\"\nedition = \"2024\"\n";
        assert_eq!(
            mask_workspace_version(before),
            mask_workspace_version(after)
        );
    }

    #[test]
    fn a_real_manifest_edit_survives_masking() {
        let before = "[workspace.package]\nversion = \"0.4.1\"\nedition = \"2024\"\n";
        let after = "[workspace.package]\nversion = \"1.0.0\"\nedition = \"2021\"\n";
        assert_ne!(
            mask_workspace_version(before),
            mask_workspace_version(after)
        );
    }

    /// A `version` key outside `[workspace.package]` is somebody else's: masking it would hide a
    /// real dependency bump in `[workspace.dependencies]`.
    #[test]
    fn only_the_workspace_version_is_masked() {
        let before = "[workspace.dependencies]\nversion = \"1\"\n";
        let after = "[workspace.dependencies]\nversion = \"2\"\n";
        assert_ne!(
            mask_workspace_version(before),
            mask_workspace_version(after)
        );
    }

    /// The release commit rewrites every workspace member's recorded version in `Cargo.lock`;
    /// a third-party bump in the same file is a real change and must not be masked with it.
    #[test]
    fn only_workspace_members_are_masked_in_the_lockfile() {
        let members = ["tankovault-api".to_owned()].into_iter().collect();
        let before = "[[package]]\nname = \"tankovault-api\"\nversion = \"0.4.1\"\n\n\
                      [[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n";
        let member_bump = "[[package]]\nname = \"tankovault-api\"\nversion = \"1.0.0\"\n\n\
                           [[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n";
        let third_party_bump = "[[package]]\nname = \"tankovault-api\"\nversion = \"1.0.0\"\n\n\
                                [[package]]\nname = \"serde\"\nversion = \"1.0.1\"\n";
        assert_eq!(
            mask_lockfile_versions(before, &members),
            mask_lockfile_versions(member_bump, &members)
        );
        assert_ne!(
            mask_lockfile_versions(before, &members),
            mask_lockfile_versions(third_party_bump, &members)
        );
    }

    #[test]
    fn member_roots_are_the_first_component_of_each_member() {
        let manifest = "[workspace]\nresolver = \"3\"\nmembers = [\n    \"crates/domain\",\n\
                        # \"crates/ignored\",\n    \"services/api\",\n    \"xtask\",\n]\n\
                        exclude = [\"web/frontend\"]\n";
        let roots = member_roots(manifest);
        assert_eq!(
            roots,
            [
                "crates".to_owned(),
                "services".to_owned(),
                "xtask".to_owned()
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn every_current_top_level_entry_is_classified() {
        let roots = [
            "crates".to_owned(),
            "services".to_owned(),
            "xtask".to_owned(),
        ]
        .into_iter()
        .collect();
        for entry in [
            "crates",
            "services",
            "xtask",
            "web",
            "deploy",
            "docs",
            "migrations",
            ".sqlx",
            ".github",
            ".cargo",
            "Cargo.toml",
            "Cargo.lock",
            "openapi.json",
            "LICENSE",
            "THIRD-PARTY-NOTICES",
            "README.md",
            "about.toml",
            "fuzz",
        ] {
            assert!(top_level_is_classified(entry, &roots), "{entry}");
        }
        assert!(!top_level_is_classified("newthing", &roots));
    }

    #[test]
    fn spa_path_dependencies_resolve_into_the_host_workspace() {
        let manifest = "[dependencies]\n\
                        tankovault-api-client = { path = \"../../crates/api-client\" }\n\
                        # tankovault-decoy = { path = \"../../crates/decoy\" }\n";
        let deps = path_dependencies(manifest);
        assert_eq!(deps, vec!["../../crates/api-client".to_owned()]);
        assert_eq!(
            join_relative("web/frontend", &deps[0]),
            Some("crates/api-client".to_owned())
        );
    }
}
