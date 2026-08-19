//! `config-contract` — the configuration contract one service image publishes.
//!
//! Three renderings, and a container build consumes all three in one pass:
//!
//! ```text
//! config-contract --service api --format contract    > contract.json
//! config-contract --service api --format labels      > contract.labels
//! config-contract --service api --format dockerfile  # paste into deploy/docker/Dockerfile
//! ```
//!
//! `contract` is the document copied into the image and attached to its digest; `labels` is the
//! same three label values the image must carry, one `NAME=value` per line, which is what the
//! build checks the *built image* against; `dockerfile` is that block, markers included, as the
//! `LABEL` instruction the Dockerfile carries by hand. All three come from one run so the
//! document and the labels cannot disagree.
//!
//! # What is left in this file
//!
//! The `--format` vocabulary, the argument syntax behind it, the dispatch across the nine
//! renderings and the stamping of a build identity onto the contract are
//! [`Cli`](terrace_config::schema::cli::Cli). They were the same two hundred lines in every
//! repository that had a generator, which is how three of them ended up disagreeing about how to
//! cut a `LABEL` block back out of a Dockerfile.
//!
//! What is this repository's own is the table of services below, the [`App`] identity built from
//! a service name, and the [`External`] surface no derive can see.
//!
//! # One binary, one contract
//!
//! `--service` is not a convenience, and it is the one flag this generator adds to the ones
//! `terrace-config` defines. A contract is a claim about what *one image's binary* loads, so each
//! of the nine roots is described on its own: describing the union would have `api`'s document
//! assert that its image reads `anilist.client_secret`, and a chart believing that is a chart
//! being told to mount a secret nothing in that pod consumes.
//!
//! It is also why the argument list is split here rather than handed to
//! [`Request::parse`](terrace_config::schema::cli::Request::parse) whole: that parser refuses an
//! argument it does not know, deliberately, and `--service` is one. What it does not recognise is
//! removed and the remainder — every flag `terrace-config` owns — is parsed by the crate that
//! owns it.
//!
//! # What this document does not carry
//!
//! **Compiled-in default values.** [`Schema::with_defaults_from`] reads them off a serialised
//! value, which needs `Serialize` on every config struct in the workspace — including the ones
//! holding a `SecretString`, which deliberately does not implement it. Every other column the
//! derive can see is published, `required` included; what an operator gets when they omit a key
//! stays in `docs/CONFIGURATION.md`.
//!
//! **The image's own `ENV` block.** `frontend` and `render` bake three configuration keys into
//! their images (`TANKOVAULT_FRONTEND__STATIC_DIR`, `TANKOVAULT_RENDER__CHROME_PATH`,
//! `TANKOVAULT_RENDER__NO_SANDBOX`), and no derive can see a Dockerfile. Those keys are in the
//! document as keys; that the image supplies them on every run is not.
//!
//! **A JSON Schema `$id`.** `--format json-schema` renders, and renders without one: an `$id` is
//! a URL under this repository that an editor will try to *resolve*, and nine of them would have
//! to be nine published files. A wrong `$id` is worse than none.
//!
//! [`Schema::with_defaults_from`]: terrace_config::schema::Schema::with_defaults_from

use std::process::ExitCode;

use terrace_config::schema::cli::{Cli, Request};
use terrace_config::schema::{App, External, ExternalVar, Schema, Unknown};

/// Every service that publishes an image, and the config root its binary deserialises.
///
/// Hand-written because which binaries become images is a deployment decision — the same one
/// `SERVICE_BINS` and `[workspace.metadata.deploy.exclude]` encode. A missing entry cannot hide:
/// the image build asks this tool for the service named by its `BIN` argument and fails on a
/// name it does not know.
macro_rules! services {
    ($($name:literal => $root:path),* $(,)?) => {
        /// The names `--service` accepts, in the order `--services` lists them.
        const SERVICES: &[&str] = &[$($name),*];

        /// The schema of one service's root, or `None` for a name that is not a service.
        fn schema_for(service: &str) -> Option<Schema> {
            match service {
                $($name => Some(tankovault_config::terrace().schema::<$root>()),)*
                _ => None,
            }
        }
    };
}

services! {
    "api" => tankovault_api::config::Config,
    "bootstrap" => tankovault_bootstrap::config::Config,
    "challenge-solver" => tankovault_challenge_solver::config::Config,
    "control-plane" => tankovault_control_plane::config::Config,
    "frontend" => tankovault_frontend::config::Config,
    "notifier" => tankovault_notifier::config::Config,
    "render" => tankovault_render::config::Config,
    "sync" => tankovault_sync::config::Config,
    "worker" => tankovault_worker::config::Config,
}

fn main() -> ExitCode {
    match run() {
        Ok(rendered) => {
            println!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// The whole program: split off `--service`, parse the rest, render.
fn run() -> Result<String, String> {
    let (service, rest) = split_service(std::env::args().skip(1))?;
    let Some(service) = service else {
        return Ok(SERVICES.join("\n"));
    };

    // Seeded with this repository's own default rather than `terrace-config`'s. `Request`
    // defaults to `json`; every caller here — the container build, `xtask config-contract` and
    // the `justfile` — is after the contract, and a bare `--service api` printed one before this
    // file delegated its parsing. A `--format` in `rest` is parsed after this pair and wins.
    let request = Request::parse(
        ["--format".to_owned(), "contract".to_owned()]
            .into_iter()
            .chain(rest),
    )
    .map_err(|error| error.to_string())?;

    // Spelled as a `terrace_config::Error` rather than as a bare string: every other failure this
    // program reports comes back from that crate, and an unknown `--service` is the same class of
    // fault as an unknown `--format`. One voice on stderr, and the same bytes as before.
    let Some(schema) = schema_for(&service) else {
        return Err(format!(
            "error: {}",
            terrace_config::Error::Invalid(format!(
                "`{service}` is not a service that publishes an image; `--services` lists the \
                 ones that do"
            ))
        ));
    };

    Cli::new(app(&service))
        .contract_with(&|builder| builder.external(external()))
        .render(&request, schema)
        .map_err(|error| format!("error: {error}"))
}

/// Take `--service` out of the argument list, and leave every flag `terrace-config` owns in it.
///
/// `--services` clears the selection rather than setting one, so the two flags are one piece of
/// state and the last one written wins — which is what a caller assembling a command line from a
/// template gets right by accident.
fn split_service(
    mut args: impl Iterator<Item = String>,
) -> Result<(Option<String>, Vec<String>), String> {
    let mut service = None;
    let mut rest = Vec::new();

    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--service" => {
                service = Some(
                    args.next()
                        .ok_or_else(|| format!("--service takes a name; {USAGE}"))?,
                );
            }
            "--services" => service = None,
            _ => rest.push(flag),
        }
    }

    Ok((service, rest))
}

/// The identity one service's image publishes.
///
/// The version is spelled as the image tag spells it: release-please tags `vX.Y.Z` and
/// `docker/metadata-action` applies that tag verbatim, while `CARGO_PKG_VERSION` alone yields the
/// form without the `v`. The field exists to be compared against a tag. `--version` overrides it;
/// `--revision` and `--created` are the other two things that legitimately differ between builds
/// of one source tree, and all three are arguments rather than environment reads so that a
/// documentation job and a container build produce the same bytes.
fn app(service: &str) -> App {
    App::new(format!("tankovault-{service}"))
        .version(concat!("v", env!("CARGO_PKG_VERSION")))
        .source("https://github.com/TimSchoenle/TankoVault")
}

/// The environment these images read that the loader does not own.
///
/// One declaration for all nine, because it is true of all nine: they are the same static musl
/// binary over the same `tankovault-service` runtime, and the one variable outside the
/// `TANKOVAULT_` namespace any of them reads is `RUST_LOG`.
///
/// [`Unknown::Reject`] is the default and it is kept: a variable this image neither reads nor
/// ignores is a rename nobody finished. What that costs is the two `ignore` patterns below —
/// names a Kubernetes pod carries that no image asked for and no image owns.
fn external() -> External {
    External::new()
        .var(
            ExternalVar::new("RUST_LOG")
                .owner("tracing")
                .ty("String")
                .docs(
                    "Verbosity, as `tracing` directives — `info`, `tankovault_api=debug,info`. \
                     Read by `EnvFilter::try_from_default_env`, which is consulted *before* \
                     `telemetry.log_filter` and wins over it when set.",
                ),
        )
        .unknown(Unknown::Reject)
        .ignore("KUBERNETES_*")
        .ignore("HOSTNAME")
}

/// The one line to print when an argument is refused.
///
/// Names every flag, not only `--service`: the reader is looking at a build log, and being told
/// about half a command line costs a second run to discover the other half.
const USAGE: &str = "usage: config-contract --service <name> \
                     [--format json|markdown|markdown-loader|markdown-keys|toml|json-schema|\
                     contract|labels|dockerfile] [--only <key-prefix>] [--path <in-image-path>] \
                     [--version <release>] [--revision <commit>] [--created <rfc3339>]\n\
                     \x20      config-contract --services";

#[cfg(test)]
mod tests {
    use terrace_config::schema::LABEL_PREFIX;
    use terrace_config::schema::cli::{Cli, Format, Request};

    use super::{SERVICES, app, external, schema_for};

    /// One rendering of one service, through the same `Cli` the binary uses.
    fn render(service: &str, format: Format) -> String {
        let schema = schema_for(service).unwrap_or_else(|| panic!("{service} has no schema"));
        Cli::new(app(service))
            .contract_with(&|builder| builder.external(external()))
            .render(&Request::new(format), schema)
            .unwrap_or_else(|error| panic!("{service} does not render {format}: {error}"))
    }

    /// Every name this tool advertises has to describe, or the image build asks for a service
    /// the list promised and gets a failure at `--format contract` instead.
    #[test]
    fn every_advertised_service_builds_a_contract() {
        for service in SERVICES {
            assert!(
                !render(service, Format::Contract).is_empty(),
                "{service} rendered nothing"
            );
        }
    }

    /// The one invariant the whole scheme rests on: the label a consumer discovers the image by
    /// and the namespace the document describes are the same string, because both come from one
    /// run of one generator. A build that generated them separately could publish an image
    /// labelled for one prefix carrying a contract for another, and the chart repo's refresh
    /// refuses exactly that pair.
    ///
    /// Asserted over the two *renderings* rather than over a `Contract` built here a second time:
    /// a second construction path is the one thing that could make this test pass while the
    /// documents the build publishes disagree.
    #[test]
    fn the_prefix_label_is_the_document_s_own_dialect() {
        for service in SERVICES {
            let document: serde_json::Value =
                serde_json::from_str(&render(service, Format::Contract))
                    .unwrap_or_else(|error| panic!("{service}'s contract is not JSON: {error}"));
            let dialect = document["schema"]["dialect"]["prefix"]
                .as_str()
                .unwrap_or_else(|| panic!("{service}'s contract carries no dialect prefix"));

            let labels = render(service, Format::Labels);
            let prefix = labels
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{LABEL_PREFIX}=")))
                .unwrap_or_else(|| panic!("{service} publishes no {LABEL_PREFIX} label"));

            assert_eq!(prefix, dialect, "{service}");
        }
    }

    /// `Unknown::Reject` means a variable that is neither a key nor declared here fails the
    /// chart's gate. `RUST_LOG` is read by every one of these binaries and carries no prefix, so
    /// leaving it out would fail a deployment that is doing nothing wrong.
    #[test]
    fn rust_log_is_declared_rather_than_ignored() {
        let external = external();
        assert!(
            external.env.iter().any(|var| var.name == "RUST_LOG"),
            "RUST_LOG must be a declared variable, not an ignore pattern: an ignore says \
             nothing about its type, and `RUST_LOG=7` is a deployment a declaration catches"
        );
    }

    /// `--service` is this repository's own flag and everything else belongs to `terrace-config`.
    /// The split is what lets both be true; a `--format` that ended up in the wrong half would
    /// render the default for every caller and nothing would say so.
    #[test]
    fn the_service_flag_is_the_only_one_taken_out_of_the_list() {
        let args = [
            "--format",
            "labels",
            "--service",
            "api",
            "--revision",
            "deadbeef",
        ]
        .map(str::to_owned);
        let (service, rest) = super::split_service(args.into_iter()).expect("a usable split");

        assert_eq!(service.as_deref(), Some("api"));
        assert_eq!(rest, ["--format", "labels", "--revision", "deadbeef"]);
    }

    /// `--services` and `--service` are one piece of state, and the contract that lists them is
    /// what `xtask config-contract` walks before it asks for a single document.
    #[test]
    fn services_clears_the_selection() {
        let args = ["--service", "api", "--services"].map(str::to_owned);
        let (service, rest) = super::split_service(args.into_iter()).expect("a usable split");

        assert_eq!(service, None);
        assert!(rest.is_empty());
    }
}
