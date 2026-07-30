//! Pins this crate's hand-written `[lints.rust]` block against `[workspace.lints.rust]`.
//!
//! `crates/api-client` is the **only** workspace member that does not write
//! `[lints] workspace = true`, and it cannot: Cargo refuses a manifest that both inherits the
//! workspace lint table and declares its own, and this crate has to declare its own to silence
//! clippy over 780 KB of `progenitor` output (see the comment in `Cargo.toml`).
//!
//! The consequence the audit flagged (BUILD_AND_OPS §2.4) is that a lint added to
//! `[workspace.lints.rust]` silently skips this crate — and generated code is exactly where a
//! new rustc lint is most likely to fire and least likely to be noticed, because nobody reads
//! the file. The clippy half is a deliberate divergence and stays one; the **rustc** half is
//! supposed to be a copy, and nothing checked that it still was.
//!
//! This test is that check. It compares the two blocks textually rather than through a TOML
//! parser: the crate is a leaf whose entire purpose is to carry generated code, and adding a
//! dependency to it to read four lines of its own manifest is a worse trade than a scanner that
//! fails loudly if the manifest stops being formatted one-lint-per-line.

use std::path::{Path, PathBuf};

/// The lint declarations under `header`, one per entry, comments and blank lines dropped.
///
/// Stops at the next `[section]`, so it reads exactly one table. A missing header yields
/// `None`, which the assertions below treat as a failure rather than as an empty set — a
/// renamed or deleted table must not read as "the two agree, both empty".
fn lint_block(manifest: &str, header: &str) -> Option<Vec<String>> {
    let mut lines = manifest.lines().skip_while(|l| l.trim() != header);
    lines.next()?;

    let mut entries = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        entries.push(trimmed.to_owned());
    }
    entries.sort();
    Some(entries)
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/api-client is two levels below the workspace root")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Every rustc lint the workspace sets must be set identically here.
///
/// The failure this prevents: someone adds `unused_qualifications = "warn"` (or, worse,
/// `= "deny"`) to `[workspace.lints.rust]`, every member picks it up, and this one does not —
/// so the generated file is the single place in the repository where that lint does not hold,
/// and the first sign of it is a confusing diff the next time `xtask openapi` runs.
#[test]
fn the_rustc_lints_match_the_workspace() {
    let root = workspace_root();
    let workspace = read(&root.join("Cargo.toml"));
    let manifest = read(&root.join("crates/api-client/Cargo.toml"));

    let expected = lint_block(&workspace, "[workspace.lints.rust]")
        .expect("`[workspace.lints.rust]` is missing from the root manifest");
    let actual = lint_block(&manifest, "[lints.rust]")
        .expect("`[lints.rust]` is missing from crates/api-client/Cargo.toml");

    assert_eq!(
        actual, expected,
        "crates/api-client cannot write `[lints] workspace = true`, so its `[lints.rust]` block \
         is a hand-maintained copy of `[workspace.lints.rust]`. They have diverged: copy the \
         workspace's block across (only the *clippy* half is allowed to differ), or, if the new \
         lint genuinely must not apply to generated code, say so in the manifest comment and \
         update this test to record the exemption by name."
    );
}

/// The clippy opt-out is the divergence this crate is allowed, and it is allowed only in the
/// one direction: silencing. A `deny` here would be an escalation the workspace never asked
/// for, applied to a file nobody edits.
#[test]
fn the_clippy_block_only_ever_relaxes() {
    let root = workspace_root();
    let manifest = read(&root.join("crates/api-client/Cargo.toml"));

    let clippy = lint_block(&manifest, "[lints.clippy]")
        .expect("`[lints.clippy]` is missing from crates/api-client/Cargo.toml");

    assert!(
        !clippy.is_empty(),
        "the clippy opt-out is what makes this crate's manifest diverge at all; if it is gone, \
         write `[lints] workspace = true` instead and delete this file"
    );
    for entry in &clippy {
        assert!(
            entry.contains("\"allow\""),
            "crates/api-client's `[lints.clippy]` exists to silence lints over generated code, \
             not to raise them: `{entry}` sets a level other than `allow`"
        );
    }
}
