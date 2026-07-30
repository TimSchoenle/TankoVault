//! `xtask ci` — the offline gates, in CI's order, from one command.
//!
//! # Why this exists
//!
//! The workflow runs eighteen jobs. A contributor wanting to know whether a change will pass had
//! to read `ci.yml` and replicate them by hand, which is how `BUILD_AND_OPS` §2.1 happened:
//! `cargo fmt --all --check` was red on `main` and stayed red, because nothing anybody ran
//! locally included it.
//!
//! # What it does and does not cover
//!
//! Only the gates that need no Docker, no network and no database — which is most of them, and
//! all of the ones a change breaks by accident. Named explicitly rather than "everything":
//!
//! | Included | Excluded, and why |
//! | --- | --- |
//! | `fmt --check` | `integration` — needs Docker and ~15 minutes of migrations |
//! | `clippy --all-targets --all-features -D warnings` | `sqlx` offline-cache check — needs a live, migrated Postgres |
//! | `test --workspace` + `--doc` | `coverage` — needs `cargo-llvm-cov` and a full instrumented build |
//! | `openapi --check` | `docker`, `css`, `observability`, `secrets` — need Docker, Node, promtool, gitleaks |
//! | the `web/frontend` gates | `msrv`, `deny`, `audit`, `supply-chain` — need another toolchain or a network fetch |
//!
//! The excluded set is not a shortfall to fix. A local command that takes twenty minutes and
//! needs four extra tools is a local command nobody runs, which puts it back where §2.1 found
//! it. What is here runs on a checkout with nothing but the pinned toolchain.
//!
//! Stops at the first failure, deliberately: the gates are ordered as CI orders them, cheapest
//! and most-likely-to-fail first, so the first red one is almost always the one to fix.

use std::process::Command;

/// How a gate runs.
enum Step {
    /// A `cargo` subcommand, in `dir` relative to the workspace root (`""` is the root).
    Cargo {
        dir: &'static str,
        args: &'static [&'static str],
    },
    /// Called in this process.
    ///
    /// The `OpenAPI` check has to be one. `cargo run -p xtask -- openapi --check` is what CI runs,
    /// and it cannot be a gate *of* `xtask ci`: cargo rebuilds the binary before running it, and
    /// on Windows the currently-executing `xtask.exe` is locked, so the command dies with
    /// `failed to remove file … Access is denied` and reports it as a drift failure, which it is
    /// not. Calling the function directly is also simply better — no rebuild, no subprocess, the
    /// same code path.
    InProcess(fn() -> anyhow::Result<()>),
}

/// One gate: what to run and what to call it when it fails.
struct Gate {
    /// Shown before the step runs, so a long clippy pass is not silent.
    name: &'static str,
    step: Step,
}

/// The offline gates, in the order CI runs them.
const GATES: &[Gate] = &[
    Gate {
        name: "fmt",
        step: Step::Cargo {
            dir: "",
            args: &["fmt", "--all", "--check"],
        },
    },
    Gate {
        name: "clippy (pedantic, all features)",
        step: Step::Cargo {
            dir: "",
            args: &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
        },
    },
    Gate {
        name: "test (offline)",
        step: Step::Cargo {
            dir: "",
            args: &["test", "--workspace"],
        },
    },
    // `--all-targets` silently *excludes* doc tests, which is why they ran nowhere for so long
    // (TESTING F-11). A separate invocation is the only way to run them.
    Gate {
        name: "doc tests",
        step: Step::Cargo {
            dir: "",
            args: &["test", "--workspace", "--doc"],
        },
    },
    Gate {
        name: "openapi drift",
        step: Step::InProcess(|| crate::openapi(true)),
    },
    // `web/frontend` is outside the host workspace, so none of the above touches it.
    Gate {
        name: "frontend test",
        step: Step::Cargo {
            dir: "web/frontend",
            args: &["test"],
        },
    },
    Gate {
        name: "frontend clippy",
        step: Step::Cargo {
            dir: "web/frontend",
            args: &["clippy", "--all-targets", "--", "-D", "warnings"],
        },
    },
    Gate {
        name: "frontend wasm",
        step: Step::Cargo {
            dir: "web/frontend",
            args: &["check", "--target", "wasm32-unknown-unknown"],
        },
    },
];

/// Run every gate, stopping at the first failure.
///
/// # Errors
/// The first gate that fails, named.
pub(crate) fn run(workspace_root: &std::path::Path) -> anyhow::Result<()> {
    // `CARGO` is set when this was itself launched by cargo, which pins the gates to the same
    // toolchain running us — so `cargo +nightly run -p xtask -- ci` does not silently check half
    // the tree with a different compiler than the half it reports on.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());

    for (i, gate) in GATES.iter().enumerate() {
        println!("[{}/{}] {}", i + 1, GATES.len(), gate.name);
        let outcome = match &gate.step {
            Step::Cargo { dir, args } => Command::new(&cargo)
                .args(*args)
                .current_dir(workspace_root.join(dir))
                .status()
                .map_err(|e| anyhow::anyhow!("could not run `cargo {}`: {e}", args.join(" ")))
                .and_then(|status| {
                    if status.success() {
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!("cargo {}", args.join(" ")))
                    }
                }),
            Step::InProcess(f) => f(),
        };

        if let Err(e) = outcome {
            anyhow::bail!(
                "gate `{}` failed: {e}\n\n\
                 The gates run in CI's order, so this is almost certainly the one to fix first.",
                gate.name
            );
        }
    }

    println!(
        "\nall {} offline gates passed. Not covered here, and CI still runs them: integration \
         (Docker), sqlx offline-cache (a migrated Postgres), coverage, deny/audit/supply-chain \
         (network), docker, css (Node), observability (promtool), secrets (gitleaks).",
        GATES.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{GATES, Step};

    /// The doc-test gate must be its own invocation.
    ///
    /// `cargo test --all-targets` silently *excludes* doc tests — the defect TESTING F-11 found,
    /// where every documented example compiled nowhere. Folding the two into one command to save
    /// a build would quietly reintroduce it, so this asserts they stay separate.
    #[test]
    fn doc_tests_are_a_gate_of_their_own() {
        let doc = GATES
            .iter()
            .find_map(|g| match &g.step {
                Step::Cargo { args, .. } if args.contains(&"--doc") => Some(*args),
                _ => None,
            })
            .expect("the doc-test gate exists");
        assert!(
            !doc.contains(&"--all-targets"),
            "`--all-targets` excludes doc tests; the two cannot share an invocation"
        );
    }

    /// No gate re-invokes `xtask` as a subprocess.
    ///
    /// Not style — it does not work, and it was the first thing running this command found.
    /// Cargo rebuilds the binary before running it, and on Windows the currently-executing
    /// `xtask.exe` is locked, so `cargo run -p xtask -- openapi --check` as a gate *of* `xtask
    /// ci` dies with `failed to remove file … Access is denied` and reports it as an `OpenAPI`
    /// drift failure, which it is not. The check is a [`Step::InProcess`] for that reason.
    #[test]
    fn no_gate_re_invokes_this_binary() {
        for gate in GATES {
            if let Step::Cargo { args, .. } = &gate.step {
                assert!(
                    !args.contains(&"xtask"),
                    "gate `{}` shells out to xtask; cargo cannot rebuild a running executable",
                    gate.name
                );
            }
        }
    }

    /// Every cargo gate is a subcommand with no shell in it.
    ///
    /// This runs on a developer machine whose shell is `PowerShell` and gates a pipeline whose
    /// shell is `sh`. The one command whose whole purpose is to make the two agree must not
    /// itself depend on which is running.
    #[test]
    fn no_gate_smuggles_a_shell() {
        for gate in GATES {
            if let Step::Cargo { args, .. } = &gate.step {
                for arg in *args {
                    assert!(
                        !arg.contains('|') && !arg.contains('&') && !arg.contains('>'),
                        "`{}` passes `{arg}` — shell metacharacters mean this gate behaves \
                         differently per platform",
                        gate.name
                    );
                }
            }
        }
    }

    /// The frontend gates run in `web/frontend`, because it is excluded from the host workspace
    /// and `cargo test --workspace` at the root reaches none of it. That exclusion is exactly
    /// why its 54 tests and its pedantic clippy set once executed nowhere at all (`FRONTEND` F2).
    #[test]
    fn the_frontend_gates_run_in_the_frontend() {
        let frontend = GATES
            .iter()
            .filter(|g| matches!(&g.step, Step::Cargo { dir, .. } if *dir == "web/frontend"))
            .count();
        assert_eq!(
            frontend, 3,
            "the frontend needs its own test, clippy and wasm gates"
        );
    }
}
