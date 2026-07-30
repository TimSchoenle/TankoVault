//! `xtask config-docs [--check]` — keep `docs/CONFIGURATION.md` honest about the code.
//!
//! # Why this exists
//!
//! `docs/CONFIGURATION.md` is hand-written and describes ~80 environment keys across eight
//! services. Nothing connected it to the structs those keys are read into, so it could drift in
//! both directions with no gate anywhere noticing — and an unknown `TANKOVAULT_*` key is
//! *ignored*, not rejected, so the drift never surfaces at runtime either. An operator setting a
//! key the document promises and the code no longer reads gets silence, which is the same
//! failure the document's own §8 exists to warn about.
//!
//! This is the shape `BUILD_AND_OPS` §10.3 asked for and the same shape as OPS-2.4 and OPS-1.5:
//! where the codebase records a decision in prose, give it something that notices when the prose
//! stops being true.
//!
//! # What it compares
//!
//! | Direction | Meaning |
//! | --- | --- |
//! | in the code, not in the document | An operator cannot discover the key at all. |
//! | in the document, not in the code | Setting it does nothing, silently. |
//! | in the document's §8, back in the code | A retired name has been reused while the document still says it is dead. |
//!
//! The surface is derived twice over, from [`surface::walk`] (the layered config structs) and
//! [`surface::direct_env_keys`] (`std::env::var` call sites), because the codebase reads
//! configuration both ways and only the first is visible in a struct.
//!
//! # Where the gate lives
//!
//! In [`tests::the_document_matches_the_code`], not in a CI step and not in `xtask ci`. Unlike
//! `openapi --check` there is no write half — the document is prose, so nothing regenerates it —
//! which leaves only the comparison, and a comparison between two committed artefacts is a test
//! in this repository (`crates/api-client/tests/workspace_lints.rs`, the `openapi.json` readers
//! in `services/api`). `cargo test --workspace` therefore runs it, and so does `xtask ci` by
//! running that. The command itself exists for the *other* half of the job: printing the derived
//! surface, which is what makes a failure fixable.
//!
//! # What it deliberately does not cover
//!
//! Non-`TANKOVAULT_` keys — `RUST_LOG`, `DATABASE_URL`, `SQLX_OFFLINE`. They are third-party or
//! tooling spellings this repository does not own, and the prefix is what makes a key derivable
//! in the first place. They stay documented by hand, in §3.

mod markdown;
mod surface;

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context as _, Result, bail};

/// One service: the tree holding its config structs, and the root struct to descend from.
struct Service {
    name: &'static str,
    src: &'static str,
    root: &'static str,
}

/// Every service that loads a config, and where its root struct lives.
///
/// Hand-written because a service is a deployment decision, not something derivable — but a
/// missing entry cannot hide: its keys would then be absent from the derived surface and the
/// document's rows for them would fail as stale.
const SERVICES: &[Service] = &[
    Service {
        name: "api",
        src: "services/api/src",
        root: "Config",
    },
    Service {
        name: "control-plane",
        src: "services/control-plane/src",
        root: "Config",
    },
    Service {
        name: "worker",
        src: "services/worker/src",
        root: "Config",
    },
    Service {
        name: "notifier",
        src: "services/notifier/src",
        root: "Config",
    },
    Service {
        name: "sync",
        src: "services/sync/src",
        root: "Config",
    },
    Service {
        name: "render",
        src: "services/render/src",
        root: "Config",
    },
    Service {
        name: "challenge-solver",
        src: "services/challenge-solver/src",
        root: "Config",
    },
    Service {
        name: "frontend",
        src: "services/frontend/src",
        root: "Config",
    },
];

/// Trees scanned for `std::env::var("TANKOVAULT_…")`, which bypasses the layered config.
const DIRECT_ENV_ROOTS: &[&str] = &["crates", "services", "xtask/src"];

/// The document this gate reads.
const DOC: &str = "docs/CONFIGURATION.md";

/// Derive the configuration surface and, with `check`, compare it against the document.
///
/// Without `check` the derived keys are printed, which is what makes a failure fixable: the
/// output is the list the document's tables have to account for.
///
/// # Errors
/// A source file that does not parse, a `serde` attribute the walker cannot model, or — under
/// `check` — any disagreement between the code and the document.
pub(crate) fn run(root: &Path, check: bool) -> Result<()> {
    let implemented = derive(root)?;

    if !check {
        for key in &implemented {
            println!("{key}");
        }
        println!(
            "\n{} keys across {} services",
            implemented.len(),
            SERVICES.len()
        );
        return Ok(());
    }

    let path = root.join(DOC);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let documented = markdown::parse(&text);

    let undocumented: Vec<_> = implemented.difference(&documented.live).collect();
    let stale: Vec<_> = documented.live.difference(&implemented).collect();
    let resurrected: Vec<_> = documented.removed.intersection(&implemented).collect();

    if undocumented.is_empty() && stale.is_empty() && resurrected.is_empty() {
        println!(
            "{DOC} matches the {} keys the code reads",
            implemented.len()
        );
        return Ok(());
    }

    let mut report = String::new();
    section(
        &mut report,
        &undocumented,
        "read by the code, missing from the document — an operator cannot discover these",
    );
    section(
        &mut report,
        &stale,
        "promised by the document, read by nothing — setting one of these does nothing, silently",
    );
    section(
        &mut report,
        &resurrected,
        "listed under `## 8. Removed keys` but read again — the document says these are dead",
    );
    bail!(
        "{DOC} and the config structs disagree:\n{report}\n\
         `cargo run -p xtask -- config-docs` prints the full derived surface. Keys are read from \
         the leftmost cell of a table row only; a mention in a Notes cell is explanation, not a \
         claim."
    );
}

/// Every `TANKOVAULT_*` key the code actually reads.
fn derive(root: &Path) -> Result<BTreeSet<String>> {
    let mut shared = surface::Table::default();
    shared.parse_dir(&root.join("crates/config/src"))?;
    // The one shared block that does not live in `crates/config`: the metadata priority policy
    // is domain policy and moved to `tankovault-domain` with ARCH-8, but `sync`'s config still
    // composes it, so it is still part of the surface.
    shared.parse_file(&root.join("crates/domain/src/metadata_priority.rs"))?;

    let mut keys = BTreeSet::new();
    for service in SERVICES {
        let mut local = surface::Table::default();
        local.parse_dir(&root.join(service.src))?;
        keys.extend(
            surface::walk(&local, &shared, service.root)
                .with_context(|| format!("deriving the config surface of `{}`", service.name))?,
        );
    }

    let roots: Vec<_> = DIRECT_ENV_ROOTS.iter().map(|d| root.join(d)).collect();
    keys.extend(surface::direct_env_keys(&roots)?);
    Ok(keys)
}

fn section(report: &mut String, keys: &[&String], headline: &str) {
    use std::fmt::Write as _;
    if keys.is_empty() {
        return;
    }
    let _ = writeln!(report, "\n{} {headline}:", keys.len());
    for key in keys {
        let _ = writeln!(report, "  {key}");
    }
}

#[cfg(test)]
mod tests {
    use super::{DIRECT_ENV_ROOTS, DOC, SERVICES, derive};
    use crate::workspace_root;

    /// The gate reads the real tree, so the paths it names must exist. A renamed service
    /// directory would otherwise make the walk silently cover one service fewer, and the only
    /// symptom would be its keys reported as stale documentation.
    #[test]
    fn every_configured_path_exists() {
        let root = workspace_root();
        let root = std::path::Path::new(root);
        for service in SERVICES {
            assert!(
                root.join(service.src).is_dir(),
                "`{}` names {}, which does not exist",
                service.name,
                service.src
            );
        }
        for dir in DIRECT_ENV_ROOTS {
            assert!(root.join(dir).is_dir(), "{dir} does not exist");
        }
        assert!(root.join(DOC).is_file(), "{DOC} does not exist");
    }

    /// The whole gate, run against the repository it ships in.
    ///
    /// This is the check itself rather than a test *of* the check: `xtask config-docs --check`
    /// is a CI job, and a unit test that only exercised synthetic sources would leave the real
    /// document ungated on any machine that did not run the job.
    #[test]
    fn the_document_matches_the_code() {
        super::run(workspace_root(), true).expect("docs/CONFIGURATION.md is up to date");
    }

    /// A sanity floor on the derived surface. If the walk ever silently stops descending — a
    /// `serde` attribute mis-modelled, a service tree that moved — the comparison would still
    /// pass the day the document was trimmed to match, so the count is asserted to be in the
    /// right order of magnitude rather than merely non-empty.
    #[test]
    fn the_derived_surface_is_the_whole_surface() {
        let keys = derive(workspace_root()).expect("the surface derives");
        assert!(
            keys.len() > 60,
            "only {} keys derived; the walk is not reaching the whole tree",
            keys.len()
        );
        for expected in [
            // A leaf on a root struct, a nested block, a doubly nested one, and a key read
            // straight from the environment — one of each way a key can reach the surface.
            "TANKOVAULT_BIND_ADDR",
            "TANKOVAULT_DATABASE__URL",
            "TANKOVAULT_RATE_LIMIT__GLOBAL__BURST",
            "TANKOVAULT_PROFILE",
        ] {
            assert!(
                keys.contains(expected),
                "{expected} is missing from the derived surface"
            );
        }
    }
}
