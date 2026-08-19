//! `xtask config-contract [--check]` — regenerate (or verify) the committed configuration
//! contracts and the Dockerfile's `LABEL` block.
//!
//! The contract each image publishes is generated inside the container build, from the binary's
//! own config root (`crates/config-contract`). Nothing in that build is reviewable, so the same
//! documents are committed under `docs/contracts/` and this gate diffs them: a renamed key, a
//! removed one or a changed type shows up as a diff in the pull request that caused it, beside
//! the code that caused it.
//!
//! It also holds the one thing the build cannot derive. Three `LABEL` values are what a
//! deployment pipeline discovers the contract by, and a `LABEL` key can be interpolated from
//! nothing — so `deploy/docker/Dockerfile` carries them by hand. The block is checked here
//! against what the generator prints, one step earlier than the built-image check in CI, and
//! with a message that names the fix.

use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, Result, bail};

/// Where the committed copies live, relative to the repository root.
const DIR: &str = "docs/contracts";

/// The Dockerfile carrying the hand-written `LABEL` block.
const DOCKERFILE: &str = "deploy/docker/Dockerfile";

/// Regenerate every committed contract and the Dockerfile block, or verify them under `check`.
///
/// # Errors
/// When the generator cannot be run or refuses to build a contract, when a file cannot be
/// written, or — under `check` — when a committed document or the `LABEL` block disagrees with
/// what the code now produces.
pub(crate) fn run(root: &Path, check: bool) -> Result<()> {
    let services = generate(root, &["--services"])?;
    let services: Vec<&str> = services
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if services.is_empty() {
        bail!("`config-contract --services` named none; nothing publishes a contract");
    }

    let dir = root.join(DIR);
    if !check {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    let mut stale = Vec::new();
    for service in &services {
        let rendered = generate(root, &["--service", service, "--format", "contract"])?;
        let path = dir.join(format!("{service}.json"));
        if check {
            let committed = std::fs::read_to_string(&path).unwrap_or_default();
            if normalise(&committed) != normalise(&rendered) {
                stale.push(format!("{DIR}/{service}.json"));
            }
        } else {
            std::fs::write(&path, &rendered)
                .with_context(|| format!("writing {}", path.display()))?;
        }
    }

    check_dockerfile_labels(root, services[0], check, &mut stale)?;

    if !stale.is_empty() {
        bail!(
            "out of date, and every consumer of these images reads them: {}\nRun \
             `cargo run -p xtask -- config-contract`.",
            stale.join(", ")
        );
    }
    if check {
        println!(
            "config contracts are up to date ({} services)",
            services.len()
        );
    } else {
        println!("wrote {} contracts under {DIR}", services.len());
    }
    Ok(())
}

/// Hold the Dockerfile's hand-written `LABEL` block to what the generator prints.
///
/// One service is enough and any of them will do: all three values are constants of the
/// *deployment*, not of the binary — the envelope version, the in-image path, and the loader's
/// prefix, which every service shares because they share a loader. That is what lets one `LABEL`
/// block serve nine images, and it is also why this compares against one service rather than
/// asserting nine identical blocks.
fn check_dockerfile_labels(
    root: &Path,
    service: &str,
    check: bool,
    stale: &mut Vec<String>,
) -> Result<()> {
    let expected = generate(root, &["--service", service, "--format", "dockerfile"])?;
    let path = root.join(DOCKERFILE);
    let dockerfile =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

    let Some(found) = label_block(&dockerfile) else {
        bail!(
            "{DOCKERFILE} carries no `LABEL dev.terrace.config.*` block, so nothing can discover \
             the contract these images embed. `config-contract --format dockerfile` prints the \
             block to paste:\n{}",
            expected.trim_end()
        );
    };

    // Trimmed at the end on both sides: the generator's stdout carries the newline `println!`
    // added, and the block read out of the Dockerfile stops at its last character.
    if normalise(found.trim_end()) != normalise(expected.trim_end()) {
        if check {
            stale.push(format!(
                "{DOCKERFILE}'s LABEL block (it reads\n{}\nand the generator prints\n{})",
                found.trim_end(),
                expected.trim_end()
            ));
        } else {
            // Deliberately not rewritten: the block is three lines a human pasted, and a task
            // runner editing a Dockerfile in place is a change nobody reviewed. Naming the diff
            // is the whole fix.
            bail!(
                "{DOCKERFILE}'s LABEL block does not match the generator. Replace it with:\n{}",
                expected.trim_end()
            );
        }
    }
    Ok(())
}

/// The `LABEL dev.terrace.config.*` instruction, continuation lines included.
///
/// Read out of the file rather than matched line by line because the instruction spans three
/// lines joined by `\`, and a rule that only saw the first would pass a Dockerfile whose second
/// and third labels had been deleted.
fn label_block(dockerfile: &str) -> Option<String> {
    let mut lines = dockerfile.lines();
    let first = lines.find(|line| line.trim_start().starts_with("LABEL dev.terrace.config."))?;
    let mut block = String::from(first);
    let mut open = first.trim_end().ends_with('\\');
    while open {
        let Some(next) = lines.next() else { break };
        block.push('\n');
        block.push_str(next);
        open = next.trim_end().ends_with('\\');
    }
    Some(block)
}

/// Strip `\r` so a CRLF working copy and an LF render compare equal.
///
/// The same belt-and-braces as `notices`: `.gitattributes` pins these artefacts to LF, and this
/// repository is developed on Windows.
fn normalise(text: &str) -> String {
    text.replace('\r', "")
}

/// Run the generator and return its stdout.
///
/// Shelled out rather than called in process: the generator depends on all nine service crates
/// to reach the config root each binary deserialises, and putting that dependency on `xtask`
/// would make every `xtask` command compile the whole service tree.
fn generate(root: &Path, args: &[&str]) -> Result<String> {
    // `CARGO` pins the child to the toolchain running us, as `ci.rs` and `notices.rs` do.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let out = Command::new(cargo)
        .current_dir(root)
        .args(["run", "--quiet", "-p", "tankovault-config-contract", "--"])
        .args(args)
        .output()
        .map_err(|e| {
            anyhow::anyhow!("failed to run `cargo run -p tankovault-config-contract`: {e}")
        })?;
    if !out.status.success() {
        bail!(
            "config-contract {} failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout).context("config-contract wrote invalid UTF-8")
}

#[cfg(test)]
mod tests {
    use super::label_block;

    /// The bug this pins: a block matched line by line passes a Dockerfile whose second and
    /// third `LABEL` values were deleted, because the first line still matches on its own. The
    /// continuation is what makes the comparison whole.
    #[test]
    fn the_block_carries_its_continuation_lines() {
        let dockerfile = "FROM scratch\nLABEL dev.terrace.config.contract.version=\"1\" \\\n      \
                          dev.terrace.config.contract.path=\"/config/contract.json\" \\\n      \
                          dev.terrace.config.prefix=\"TANKOVAULT_\"\nUSER 1001:1001\n";
        let block = label_block(dockerfile).expect("the block");
        assert_eq!(block.lines().count(), 3);
        assert!(block.ends_with("dev.terrace.config.prefix=\"TANKOVAULT_\""));
        assert!(!block.contains("USER"));
    }

    #[test]
    fn a_dockerfile_without_the_block_reports_none() {
        assert!(label_block("FROM scratch\nUSER 1001:1001\n").is_none());
    }
}
