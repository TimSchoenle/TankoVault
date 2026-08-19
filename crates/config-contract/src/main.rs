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
//! build checks the *built image* against; `dockerfile` is that block as the `LABEL` instruction
//! the Dockerfile carries by hand. All three come from one run so the document and the labels
//! cannot disagree.
//!
//! # One binary, one contract
//!
//! `--service` is not a convenience. A contract is a claim about what *one image's binary*
//! loads, so each of the nine roots is described on its own: describing the union would have
//! `api`'s document assert that its image reads `anilist.client_secret`, and a chart believing
//! that is a chart being told to mount a secret nothing in that pod consumes.
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
//! [`Schema::with_defaults_from`]: terrace_config::schema::Schema::with_defaults_from

use std::process::ExitCode;

use terrace_config::schema::{App, Contract, DEFAULT_PATH, External, ExternalVar, Schema, Unknown};

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
    let options = match Options::from_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    match render(&options) {
        Ok(rendered) => {
            println!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn render(options: &Options) -> Result<String, terrace_config::Error> {
    let Some(service) = options.service.as_deref() else {
        return Ok(SERVICES.join("\n"));
    };
    let contract = contract(service, options)?;
    match options.format {
        Format::Contract => contract.to_json(),
        Format::Labels => Ok(contract
            .labels(DEFAULT_PATH)
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("\n")),
        Format::Dockerfile => Ok(contract
            .to_dockerfile_labels(DEFAULT_PATH)
            .trim_end()
            .to_owned()),
    }
}

/// The whole contract one image publishes: every configuration key its binary reads, and
/// everything else it reads that is nobody's configuration key.
fn contract(service: &str, options: &Options) -> Result<Contract, terrace_config::Error> {
    let Some(schema) = schema_for(service) else {
        return Err(terrace_config::Error::Invalid(format!(
            "`{service}` is not a service that publishes an image; `--services` lists the ones \
             that do"
        )));
    };

    // Spelled as the image tag spells it: release-please tags `vX.Y.Z` and
    // `docker/metadata-action` applies that tag verbatim, while `CARGO_PKG_VERSION` alone yields
    // the form without the `v`. The field exists to be compared against a tag.
    let mut app = App::new(format!("tankovault-{service}"))
        .version(concat!("v", env!("CARGO_PKG_VERSION")))
        .source("https://github.com/TimSchoenle/TankoVault");

    // The two fields that legitimately differ between builds of one source tree, and the reason
    // they are flags rather than something read here: this generator reads nothing from its
    // environment, so a documentation job and a container build produce the same bytes. Passing
    // them makes that difference explicit and keeps `--format contract` reproducible — which is
    // what lets the committed copy be diffed (`xtask config-contract --check`).
    if let Some(revision) = &options.revision {
        app = app.revision(revision);
    }
    if let Some(created) = &options.created {
        app = app.created(created);
    }

    schema.into_contract(app).external(external()).build()
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

/// What to emit, and for which image.
struct Options {
    /// The service to describe. `None` lists the ones that can be described.
    service: Option<String>,
    format: Format,
    /// The commit this build is of.
    revision: Option<String>,
    /// When this build happened, RFC 3339.
    created: Option<String>,
}

/// Which rendering to emit.
#[derive(Clone, Copy)]
enum Format {
    /// The document a build embeds in its image and attaches to its digest.
    Contract,
    /// The image labels that make that document discoverable, one `NAME=value` per line.
    Labels,
    /// The same labels as the `LABEL` instruction the Dockerfile carries.
    Dockerfile,
}

impl Options {
    fn from_args() -> Result<Self, String> {
        let mut options = Self {
            service: None,
            format: Format::Contract,
            revision: None,
            created: None,
        };
        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--service" => {
                    options.service = Some(
                        args.next()
                            .ok_or_else(|| format!("--service takes a name; {USAGE}"))?,
                    );
                }
                "--services" => options.service = None,
                "--format" => {
                    options.format = match args.next().as_deref() {
                        Some("contract") => Format::Contract,
                        Some("labels") => Format::Labels,
                        Some("dockerfile") => Format::Dockerfile,
                        Some(other) => return Err(format!("unknown format `{other}`; {USAGE}")),
                        None => return Err(format!("--format takes a value; {USAGE}")),
                    };
                }
                "--revision" => {
                    options.revision = Some(
                        args.next()
                            .ok_or_else(|| format!("--revision takes a commit; {USAGE}"))?,
                    );
                }
                "--created" => {
                    options.created = Some(
                        args.next()
                            .ok_or_else(|| format!("--created takes a timestamp; {USAGE}"))?,
                    );
                }
                other => return Err(format!("unknown argument `{other}`; {USAGE}")),
            }
        }
        Ok(options)
    }
}

const USAGE: &str = "usage: config-contract --service <name> \
                     [--format contract|labels|dockerfile] [--revision <commit>] \
                     [--created <rfc3339>]\n\
                     \x20      config-contract --services";

#[cfg(test)]
mod tests {
    use super::{DEFAULT_PATH, SERVICES, contract, external, schema_for};

    fn options() -> super::Options {
        super::Options {
            service: None,
            format: super::Format::Contract,
            revision: None,
            created: None,
        }
    }

    /// Every name this tool advertises has to describe, or the image build asks for a service
    /// the list promised and gets a failure at `--format contract` instead.
    #[test]
    fn every_advertised_service_builds_a_contract() {
        for service in SERVICES {
            assert!(schema_for(service).is_some(), "{service} has no schema");
            contract(service, &options()).unwrap_or_else(|error| {
                panic!("{service} does not build a contract: {error}");
            });
        }
    }

    /// The one invariant the whole scheme rests on: the label a consumer discovers the image by
    /// and the namespace the document describes are the same string, because both come from one
    /// run of one generator. A build that generated them separately could publish an image
    /// labelled for one prefix carrying a contract for another, and the chart repo's refresh
    /// refuses exactly that pair.
    #[test]
    fn the_prefix_label_is_the_document_s_own_dialect() {
        for service in SERVICES {
            let contract = contract(service, &options()).expect("contract");
            let labels = contract.labels(DEFAULT_PATH);
            let (_, prefix) = labels
                .iter()
                .find(|(name, _)| *name == terrace_config::schema::LABEL_PREFIX)
                .expect("the prefix label");
            assert_eq!(prefix, &contract.schema.dialect.prefix, "{service}");
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
}
