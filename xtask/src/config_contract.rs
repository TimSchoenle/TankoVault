//! `xtask config-contract` — hold the committed configuration contracts and the Dockerfile's
//! `LABEL` regions to what the generator now produces.
//!
//! The contract each image publishes is generated inside the container build, from the binary's
//! own config root (`crates/config-contract`). Nothing in that build is reviewable, so the same
//! documents are committed under `docs/contracts/` and this gate diffs them: a renamed key, a
//! removed one or a changed type shows up as a diff in the pull request that caused it, beside
//! the code that caused it.
//!
//! It also holds the one thing the build cannot derive. Three `LABEL` values are what a
//! deployment pipeline discovers the contract by, and a `LABEL` key can be interpolated from
//! nothing — so `deploy/docker/Dockerfile` carries them in a marked region per runtime stage.
//! Every region is compared, one step earlier than the built-image check in CI, and with a
//! message that names the fix.
//!
//! # This checks; `just` writes
//!
//! There is no `--check`, because there is no other mode. Rewriting the committed documents and
//! the Dockerfile regions is `just regenerate`, which runs the same generator with the same
//! arguments and is the command the README, `auto-fix.yaml` and a developer all quote. A gate
//! that can also write is a gate carrying a second implementation of the thing it is checking,
//! and the two drift in the direction nobody looks.
//!
//! The built-image half of the same question is `.github/scripts/verify-config-contract.sh`. It
//! needs an image and that build's own export, so it can live neither here nor in the `justfile`.

use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, Result, bail};

/// Where the committed copies live, relative to the repository root.
const DIR: &str = "docs/contracts";

/// The Dockerfile carrying the hand-written `LABEL` regions.
const DOCKERFILE: &str = "deploy/docker/Dockerfile";

/// What to run once this gate has named a stale artefact.
const FIX: &str = "just regenerate";

/// Verify every committed contract and every Dockerfile `LABEL` region.
///
/// # Errors
/// When the generator cannot be run or refuses to build a contract, when the Dockerfile cannot be
/// read or carries no usable region, or when a committed document or a `LABEL` region disagrees
/// with what the code now produces.
pub(crate) fn run(root: &Path) -> Result<()> {
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
    let mut stale = Vec::new();
    for service in &services {
        let rendered = generate(root, &["--service", service, "--format", "contract"])?;
        let path = dir.join(format!("{service}.json"));
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        if normalise(&committed) != normalise(&rendered) {
            stale.push(format!("{DIR}/{service}.json"));
        }
    }

    check_dockerfile_labels(root, services[0], &mut stale)?;

    if !stale.is_empty() {
        bail!(
            "out of date, and every consumer of these images reads them: {}\nRun `{FIX}`.",
            stale.join(", ")
        );
    }
    println!(
        "config contracts are up to date ({} services)",
        services.len()
    );
    Ok(())
}

/// Hold every marked region in the Dockerfile to what the generator prints.
///
/// One service is enough and any of them will do: all three values are constants of the
/// *deployment*, not of the binary — the envelope version, the in-image path, and the loader's
/// prefix, which every service shares because they share a loader. That is what lets one block
/// serve nine images, and it is also why this compares against one service rather than asserting
/// nine identical blocks.
///
/// Every region is compared rather than the first. This file has three runtime stages and each
/// needs the labels; a check that stopped at the first would pass a Dockerfile whose other two
/// stages ship images nothing can discover the contract of.
fn check_dockerfile_labels(root: &Path, service: &str, stale: &mut Vec<String>) -> Result<()> {
    let rendered = generate(root, &["--service", service, "--format", "dockerfile"])?;
    let expected = normalise(rendered.trim_end());

    // The markers are read off the generated block rather than restated here. They belong to
    // `terrace-config`, which this crate deliberately does not link — it shells out, so that
    // `xtask` does not compile the nine service crates — and a copy of a constant in a crate that
    // cannot see the original is a constant that goes stale silently.
    let (Some(begin), Some(end)) = (expected.lines().next(), expected.lines().last()) else {
        bail!("`config-contract --format dockerfile` printed nothing to compare against");
    };
    if !begin.starts_with('#') || !end.starts_with('#') {
        bail!(
            "`config-contract --format dockerfile` no longer emits a marked region, so there is \
             nothing to cut {DOCKERFILE} at. It printed:\n{expected}"
        );
    }

    let path = root.join(DOCKERFILE);
    let dockerfile =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let dockerfile = normalise(&dockerfile);
    let regions = label_regions(&dockerfile, begin, end)?;

    if regions.is_empty() {
        bail!(
            "{DOCKERFILE} carries no `{begin}` … `{end}` region, so nothing can discover the \
             contract these images embed and the generated block has nowhere to go. Paste this \
             in once, markers included, and `{FIX}` keeps it current afterwards:\n{expected}"
        );
    }

    for (index, region) in regions.iter().enumerate() {
        if region != &expected {
            // Deliberately not rewritten here: this gate checks. `just dockerfile-labels` writes,
            // and keeping the two apart is what stops a red gate from being a gate that quietly
            // edited the file it was asked to judge.
            stale.push(format!(
                "{DOCKERFILE}'s LABEL region {} of {} (it reads\n{region}\nand the generator \
                 prints\n{expected})",
                index + 1,
                regions.len(),
            ));
        }
    }
    Ok(())
}

/// Every marked region of the Dockerfile, markers included, in the order they appear.
///
/// Cut at the markers rather than by finding the `LABEL` instruction and following its
/// continuation lines. That older rule read correctly right up until the block was written as one
/// `LABEL` per line, at which point it compared one line of three and passed.
///
/// # Errors
/// When a region is opened and never closed, or closed without being opened. Either is a file
/// somebody edited across a marker, and skipping it silently is how a region stops being checked.
fn label_regions(dockerfile: &str, begin: &str, end: &str) -> Result<Vec<String>> {
    let mut regions = Vec::new();
    let mut open: Option<Vec<&str>> = None;

    for (index, line) in dockerfile.lines().enumerate() {
        let number = index + 1;
        if line == begin {
            if open.is_some() {
                bail!("{DOCKERFILE}:{number}: `{begin}` inside a region that is still open");
            }
            open = Some(vec![line]);
        } else if line == end {
            let Some(mut region) = open.take() else {
                bail!("{DOCKERFILE}:{number}: `{end}` with no `{begin}` above it");
            };
            region.push(line);
            regions.push(region.join("\n"));
        } else if let Some(region) = open.as_mut() {
            region.push(line);
        }
    }

    if open.is_some() {
        bail!("{DOCKERFILE}: a `{begin}` region is never closed by `{end}`");
    }
    Ok(regions)
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
    use super::label_regions;

    const BEGIN: &str = "# terrace-config:labels:begin";
    const END: &str = "# terrace-config:labels:end";

    /// The reason this file has three runtime stages and one block: a check that stopped at the
    /// first region would pass a Dockerfile whose other two stages ship images no consumer can
    /// discover the contract of.
    #[test]
    fn every_region_is_cut_not_only_the_first() {
        let dockerfile = format!(
            "FROM scratch\n{BEGIN}\nLABEL a=\"1\"\n{END}\nUSER 1001:1001\n\
             FROM scratch AS second\n{BEGIN}\nLABEL a=\"2\"\n{END}\n"
        );
        let regions = label_regions(&dockerfile, BEGIN, END).expect("two regions");

        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0], format!("{BEGIN}\nLABEL a=\"1\"\n{END}"));
        assert_eq!(regions[1], format!("{BEGIN}\nLABEL a=\"2\"\n{END}"));
    }

    /// The bug the old rule pinned, still pinned: the block is written across continuation lines,
    /// and cutting at the markers carries them without knowing how many there are.
    #[test]
    fn a_region_carries_its_continuation_lines() {
        let dockerfile = format!(
            "FROM scratch\n{BEGIN}\nLABEL dev.terrace.config.contract.version=\"1\" \\\n      \
             dev.terrace.config.contract.path=\"/config/contract.json\" \\\n      \
             dev.terrace.config.prefix=\"TANKOVAULT_\"\n{END}\nUSER 1001:1001\n"
        );
        let regions = label_regions(&dockerfile, BEGIN, END).expect("one region");

        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].lines().count(), 5);
        assert!(regions[0].ends_with(END));
        assert!(!regions[0].contains("USER"));
    }

    /// A file with no region is not one that passed — it is one where the generated block has
    /// nowhere to go. The empty result is what the caller turns into that message.
    #[test]
    fn a_dockerfile_without_a_region_yields_none() {
        let regions =
            label_regions("FROM scratch\nUSER 1001:1001\n", BEGIN, END).expect("no regions");

        assert!(regions.is_empty());
    }

    /// An edit across a marker leaves a region that cannot be compared. Skipping it silently is
    /// how a stage stops being checked without anything saying so.
    #[test]
    fn an_unbalanced_region_is_refused() {
        let opened = format!("FROM scratch\n{BEGIN}\nLABEL a=\"1\"\n");
        assert!(label_regions(&opened, BEGIN, END).is_err());

        let closed = format!("FROM scratch\nLABEL a=\"1\"\n{END}\n");
        assert!(label_regions(&closed, BEGIN, END).is_err());
    }
}
