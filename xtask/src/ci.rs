//! `xtask ci` — the offline gates, in CI's order, from one command.
//!
//! # Why this exists
//!
//! The workflow runs seventeen jobs. A contributor wanting to know whether a change will pass
//! had to read `ci.yml` and replicate them by hand, which is how `BUILD_AND_OPS` §2.1 happened:
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

/// One gate: what to run, where, and what to call it when it fails.
struct Gate {
    /// Shown before the command runs, so a long clippy pass is not silent.
    name: &'static str,
    /// The working directory, relative to the workspace root. `""` is the root itself.
    dir: &'static str,
    args: &'static [&'static str],
}

/// The offline gates, in the order CI runs them.
const GATES: &[Gate] = &[
    Gate {
        name: "fmt",
        dir: "",
        args: &["fmt", "--all", "--check"],
    },
    Gate {
        name: "clippy (pedantic, all features)",
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
    Gate {
        name: "test (offline)",
        dir: "",
        args: &["test", "--workspace"],
    },
    // `--all-targets` silently *excludes* doc tests, which is why they ran nowhere for so long
    // (TESTING F-11). A separate invocation is the only way to run them.
    Gate {
        name: "doc tests",
        dir: "",
        args: &["test", "--workspace", "--doc"],
    },
    Gate {
        name: "openapi drift",
        dir: "",
        args: &["run", "-p", "xtask", "--", "openapi", "--check"],
    },
    // `web/frontend` is outside the host workspace, so none of the above touches it.
    Gate {
        name: "frontend test",
        dir: "web/frontend",
        args: &["test"],
    },
    Gate {
        name: "frontend clippy",
        dir: "web/frontend",
        args: &["clippy", "--all-targets", "--", "-D", "warnings"],
    },
    Gate {
        name: "frontend wasm",
        dir: "web/frontend",
        args: &["check", "--target", "wasm32-unknown-unknown"],
    },
];

/// Run every gate, stopping at the first failure.
///
/// # Errors
/// The first gate that exits non-zero, named.
pub(crate) fn run(workspace_root: &std::path::Path) -> anyhow::Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());

    for (i, gate) in GATES.iter().enumerate() {
        println!("[{}/{}] {}", i + 1, GATES.len(), gate.name);
        let status = Command::new(&cargo)
            .args(gate.args)
            .current_dir(workspace_root.join(gate.dir))
            .status()
            .map_err(|e| anyhow::anyhow!("could not run `cargo {}`: {e}", gate.args.join(" ")))?;
        if !status.success() {
            anyhow::bail!(
                "gate `{}` failed: cargo {}\n\n\
                 Run it directly to see the output above in isolation. The gates are ordered as \
                 CI orders them, so this is almost certainly the one to fix first.",
                gate.name,
                gate.args.join(" ")
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
    use super::GATES;

    /// The doc-test gate must be its own invocation.
    ///
    /// `cargo test --all-targets` silently *excludes* doc tests — the defect TESTING F-11 found,
    /// where every documented example compiled nowhere. Folding the two into one command to save
    /// a build would quietly reintroduce it, so this asserts they stay separate.
    #[test]
    fn doc_tests_are_a_gate_of_their_own() {
        let doc = GATES
            .iter()
            .find(|g| g.args.contains(&"--doc"))
            .expect("the doc-test gate exists");
        assert!(
            !doc.args.contains(&"--all-targets"),
            "`--all-targets` excludes doc tests; the two cannot share an invocation"
        );
    }

    /// Every gate is a `cargo` subcommand with no shell in it.
    ///
    /// Not style: this runs on the developer's machine, and a gate assembled as a shell string
    /// would behave differently under `PowerShell`, `cmd` and `sh` — which is the platform this
    /// project is developed on and the platform CI runs on, disagreeing about the one command whose
    /// whole purpose is to make them agree.
    #[test]
    fn no_gate_smuggles_a_shell() {
        for gate in GATES {
            for arg in gate.args {
                assert!(
                    !arg.contains('|') && !arg.contains('&') && !arg.contains('>'),
                    "`{}` passes `{arg}` — shell metacharacters mean this gate behaves \
                     differently per platform",
                    gate.name
                );
            }
        }
    }

    /// The frontend gates run in `web/frontend`, because it is excluded from the host workspace
    /// and `cargo test --workspace` at the root reaches none of it. That exclusion is exactly
    /// why its 54 tests and its pedantic clippy set once executed nowhere at all (`FRONTEND` F2).
    #[test]
    fn the_frontend_gates_run_in_the_frontend() {
        let frontend: Vec<_> = GATES.iter().filter(|g| g.dir == "web/frontend").collect();
        assert_eq!(
            frontend.len(),
            3,
            "the frontend needs its own test, clippy and wasm gates"
        );
    }
}
