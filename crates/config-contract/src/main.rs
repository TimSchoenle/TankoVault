//! Renders the configuration contract one service image publishes.
//!
//! A container build consumes three renderings in one pass:
//!
//! ```text
//! config-contract --service api --format contract    > contract.json
//! config-contract --service api --format labels      > contract.labels
//! config-contract --service api --format dockerfile  # paste into deploy/docker/Dockerfile
//! ```
//!
//! `contract` is the document copied into the image and attached to its digest. `labels` is the
//! three label values the image must carry, one `NAME=value` per line, and the build checks the
//! *built image* against it. `dockerfile` is the same block, markers included, as the `LABEL`
//! instruction the Dockerfile carries by hand. All three come from one run, so the document and
//! the labels cannot disagree.
//!
//! # What is left in this file
//!
//! The `--format` vocabulary, the argument syntax behind it, the dispatch across the nine
//! renderings and the stamping of a build identity onto the contract are
//! [`terrace_config::schema::cli::Cli`], shared by every repository that has a generator.
//!
//! What is this repository's own is the table of services below, the [`App`] identity built from
//! a service name, and the [`External`] surface no derive can see.
//!
//! # One binary, one contract
//!
//! `--service` is the one flag this generator adds to the ones `terrace-config` defines. A
//! contract is a claim about what *one image's binary* loads, so each of the nine roots is
//! described on its own. Describing the union would have `api`'s document assert that its image
//! reads `anilist.client_secret`, and a chart that believes it mounts a secret nothing in that pod
//! consumes.
//!
//! [`Request::parse`](terrace_config::schema::cli::Request::parse) refuses an argument it does not
//! know, and `--service` is one, so this file takes `--service` out and hands every remaining flag
//! to that parser.
//!
//! # What this document does not carry
//!
//! **Compiled-in default values.** [`Schema::with_defaults_from`] reads them off a serialised
//! value, which needs `Serialize` on every config struct in the workspace. The ones holding a
//! `SecretString` cannot have it, because `SecretString` does not implement it. Every other column
//! the derive can see is published, `required` included. What an operator gets when they omit a
//! key is in `docs/CONFIGURATION.md`.
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

/// Every service that publishes an image, and how its binary's schema is built.
///
/// Which binaries become images is a deployment decision, the same one `SERVICE_BINS` and
/// `[workspace.metadata.deploy.exclude]` encode, so the table is written by hand. A missing entry
/// fails the image build, which asks this tool for the service its `BIN` argument names.
///
/// An entry names the config root the binary deserialises. An optional `=> builder` follows for a
/// binary whose runtime checks more than its types say and refines the schema to match.
macro_rules! services {
    ($($name:literal => $root:path $(=> $build:path)?),* $(,)?) => {
        /// The names `--service` accepts, in the order `--services` lists them.
        const SERVICES: &[&str] = &[$($name),*];

        /// Returns the schema of one service's root, or `None` for a name that is not a service.
        fn schema_for(service: &str) -> Option<Result<Schema, terrace_config::Error>> {
            match service {
                $($name => Some(services!(@schema $root $(=> $build)?)),)*
                _ => None,
            }
        }
    };
    (@schema $root:path => $build:path) => { $build() };
    (@schema $root:path) => { Ok(tankovault_config::terrace().schema::<$root>()) };
}

services! {
    "api" => tankovault_api::config::Config => tankovault_api::config::published_schema,
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

/// Returns the whole program: split off `--service`, parse the rest, render.
fn run() -> Result<String, String> {
    let (service, rest) = split_service(std::env::args().skip(1))?;
    let Some(service) = service else {
        return Ok(SERVICES.join("\n"));
    };

    // `Request` defaults to `json`, and every caller here wants the contract: the container
    // build, `xtask config-contract` and the `justfile`. A `--format` in `rest` is parsed after
    // this pair and wins.
    let request = Request::parse(
        ["--format".to_owned(), "contract".to_owned()]
            .into_iter()
            .chain(rest),
    )
    .map_err(|error| error.to_string())?;

    // Every other failure this program reports is a `terrace_config::Error`, and an unknown
    // `--service` is the same fault as an unknown `--format`, so it is reported the same way.
    let Some(schema) = schema_for(&service) else {
        return Err(format!(
            "error: {}",
            terrace_config::Error::Invalid(format!(
                "`{service}` is not a service that publishes an image; `--services` lists the \
                 ones that do"
            ))
        ));
    };

    let schema = schema.map_err(|error| format!("error: {error}"))?;

    Cli::new(app(&service))
        .contract_with(&|builder| builder.external(external()))
        .render(&request, schema)
        .map_err(|error| format!("error: {error}"))
}

/// Takes `--service` out of the argument list, and leave every flag `terrace-config` owns in it.
///
/// `--services` clears the selection, so the two flags are one piece of state and the last one
/// on the line wins.
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

/// Returns the identity one service's image publishes.
///
/// The version is spelled as the image tag spells it, `vX.Y.Z`, because it is compared against
/// that tag. `--version`, `--revision` and `--created` override the build-specific fields. They
/// are arguments, not environment reads, so a documentation job and a container build produce
/// the same bytes.
fn app(service: &str) -> App {
    App::new(format!("tankovault-{service}"))
        .version(concat!("v", env!("CARGO_PKG_VERSION")))
        .source("https://github.com/TimSchoenle/TankoVault")
}

/// Returns the environment these images read that the loader does not own.
///
/// One declaration covers all nine images. They run the same `tankovault-service` runtime, and
/// the one variable outside the `TANKOVAULT_` namespace any of them reads is `RUST_LOG`.
///
/// [`Unknown::Reject`] stays on, because a variable an image neither reads nor ignores is an
/// unfinished rename. The two `ignore` patterns are names a Kubernetes pod carries that no image
/// reads.
fn external() -> External {
    External::new()
        .var(
            ExternalVar::new("RUST_LOG")
                .owner("tracing")
                .ty("String")
                .docs(
                    "Verbosity, as `tracing` directives such as `info` or \
                     `tankovault_api=debug,info`. \
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
/// Names every flag, because the reader is looking at a build log.
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
        let schema = schema_for(service)
            .unwrap_or_else(|| panic!("{service} has no schema"))
            .unwrap_or_else(|error| panic!("{service}'s schema does not build: {error}"));
        Cli::new(app(service))
            .contract_with(&|builder| builder.external(external()))
            .render(&Request::new(format), schema)
            .unwrap_or_else(|error| panic!("{service} does not render {format}: {error}"))
    }

    /// Every name `--services` lists renders a contract, since the image build asks for each one.
    #[test]
    fn every_advertised_service_builds_a_contract() {
        for service in SERVICES {
            assert!(
                !render(service, Format::Contract).is_empty(),
                "{service} rendered nothing"
            );
        }
    }

    /// The label a consumer discovers the image by and the namespace the document describes are
    /// the same string. The chart repository's refresh refuses an image whose two differ.
    ///
    /// Asserted over the two published renderings. A `Contract` built a second time here could
    /// agree with itself while the published documents disagree.
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

    /// Under `Unknown::Reject`, a variable that is neither a key nor declared fails the chart's
    /// gate, and every binary reads the unprefixed `RUST_LOG`.
    #[test]
    fn rust_log_is_declared_rather_than_ignored() {
        let external = external();
        assert!(
            external.env.iter().any(|var| var.name == "RUST_LOG"),
            "RUST_LOG must be a declared variable, not an ignore pattern: an ignore says \
             nothing about its type, and `RUST_LOG=7` is a deployment a declaration catches"
        );
    }

    /// A `--format` taken out with `--service` would silently render the default format.
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

    /// Each service's `metadata` keys are the ones its binary reads. The chart repository's
    /// binding gate demands a chart value for every declared key, so an extra key here is a value
    /// an operator must supply for nothing.
    ///
    /// The sets are written out per service: an expectation derived from the same types would
    /// keep passing if the sets merged.
    #[test]
    fn each_service_declares_only_the_metadata_keys_its_binary_reads() {
        const PRIORITY: [&str; 7] = [
            "metadata.priority.content_type",
            "metadata.priority.cover",
            "metadata.priority.default",
            "metadata.priority.description",
            "metadata.priority.release_year",
            "metadata.priority.status",
            "metadata.priority.title",
        ];
        let expected = |service: &str| -> Vec<String> {
            let mut keys: Vec<String> = match service {
                // Reads the classifier alone: it writes no metadata, and shares that one list
                // with the worker so the genres `GET /v1/tags` withholds are the genres that
                // close the gate.
                "api" => vec!["metadata.tags.adult_tags".to_owned()],
                // Writes metadata and refuses vocabulary, but classifies nothing: `is_adult`
                // comes from `AniList`.
                "sync" => PRIORITY
                    .iter()
                    .map(|key| (*key).to_owned())
                    .chain(
                        [
                            "metadata.enrich_batch",
                            "metadata.enrich_enabled",
                            "metadata.enrich_interval_secs",
                            "metadata.enrich_max_series",
                            "metadata.tags.blocklist",
                            "metadata.tags.use_defaults",
                        ]
                        .iter()
                        .map(|key| (*key).to_owned()),
                    )
                    .collect(),
                // The ingest path: it applies every rule in the section.
                "worker" => PRIORITY
                    .iter()
                    .map(|key| (*key).to_owned())
                    .chain(
                        [
                            "metadata.tags.adult_tags",
                            "metadata.tags.blocklist",
                            "metadata.tags.use_defaults",
                        ]
                        .iter()
                        .map(|key| (*key).to_owned()),
                    )
                    .collect(),
                _ => Vec::new(),
            };
            keys.sort();
            keys
        };

        for service in SERVICES {
            let document: serde_json::Value =
                serde_json::from_str(&render(service, Format::Contract))
                    .unwrap_or_else(|error| panic!("{service}'s contract is not JSON: {error}"));
            let keys = document["schema"]["keys"]
                .as_array()
                .unwrap_or_else(|| panic!("{service}'s contract carries no keys"));
            let mut declared: Vec<String> = keys
                .iter()
                .filter_map(|key| key["path"].as_str())
                .filter(|path| path.starts_with("metadata."))
                .map(str::to_owned)
                .collect();
            declared.sort();

            assert_eq!(declared, expected(service), "{service}");
        }
    }

    /// A later `--services` clears an earlier `--service`.
    #[test]
    fn services_clears_the_selection() {
        let args = ["--service", "api", "--services"].map(str::to_owned);
        let (service, rest) = super::split_service(args.into_iter()).expect("a usable split");

        assert_eq!(service, None);
        assert_eq!(rest, Vec::<String>::new());
    }
}
