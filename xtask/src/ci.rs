//! `xtask ci` — every offline gate CI runs, in CI's order, stopping at the first failure. Covers
//! only what needs no Docker, no network and no database; CI alone still runs `integration`,
//! the `sqlx` offline-cache check, `coverage`, `deny`/`audit`/`supply-chain`, and the
//! Docker/Node/promtool/gitleaks jobs.

use std::process::Command;

/// How a gate runs.
enum Step {
    /// A `cargo` subcommand, in `dir` relative to the workspace root (`""` is the root).
    Cargo {
        dir: &'static str,
        args: &'static [&'static str],
    },
    /// Called in-process: cargo rebuilds the xtask binary before running it, and on Windows
    /// the running `xtask.exe` is locked, so a gate that shelled back out to it would die with
    /// an access-denied error and report it as a drift failure, which it is not.
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
    // `--all-targets` silently excludes doc tests; a separate invocation is the only way to run
    // them.
    Gate {
        name: "doc tests",
        step: Step::Cargo {
            dir: "",
            args: &["test", "--workspace", "--doc"],
        },
    },
    // `cargo test --doc` runs the examples and says nothing about whether a `[`Foo`]` link
    // resolves; only rustdoc's own pass does. `--no-deps` because a dependency's broken link is
    // not ours to fix.
    Gate {
        name: "rustdoc (intra-doc links)",
        step: Step::Cargo {
            dir: "",
            args: &["doc", "--workspace", "--no-deps", "--all-features"],
        },
    },
    Gate {
        name: "openapi drift",
        step: Step::InProcess(|| crate::openapi(true)),
    },
    Gate {
        name: "repo invariants",
        step: Step::InProcess(|| crate::repo_lint::run(crate::workspace_root())),
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
    // `CARGO` pins the gates to the toolchain running us, not whatever `cargo` resolves to.
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

    /// `cargo test --all-targets` silently excludes doc tests; folding the two gates into one
    /// invocation to save a build would reintroduce that silently.
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

    /// `cargo test --doc` executes examples and never resolves a `[`Foo`]` link; `cargo doc`
    /// resolves links and runs no example. Neither subsumes the other, so merging them would
    /// silently drop one half.
    #[test]
    fn link_checking_and_doc_tests_are_separate_gates() {
        let doc_gate = GATES.iter().find(|g| match &g.step {
            Step::Cargo { args, .. } => args.first() == Some(&"doc"),
            Step::InProcess(_) => false,
        });
        let args = match &doc_gate.expect("the rustdoc gate exists").step {
            Step::Cargo { args, .. } => *args,
            Step::InProcess(_) => unreachable!("matched as Cargo above"),
        };
        assert!(
            args.contains(&"--no-deps"),
            "a dependency's broken link is not ours to fix; the gate must stay `--no-deps`"
        );
        assert!(
            !args.contains(&"--doc"),
            "`cargo doc` and `cargo test --doc` are different commands checking different things"
        );
    }

    /// Not style — it doesn't work. Cargo rebuilds the xtask binary before running it, and on
    /// Windows the running `xtask.exe` is locked, so a gate that shelled back into it would die
    /// with an access-denied error.
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

    /// Runs on a developer's `PowerShell` and gates a pipeline's `sh`; a shell metacharacter in
    /// an arg would make the gate behave differently per platform.
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

    /// `web/frontend` is excluded from the host workspace, so `cargo test --workspace` at the
    /// root reaches none of it — it once ran nowhere at all for exactly that reason.
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
