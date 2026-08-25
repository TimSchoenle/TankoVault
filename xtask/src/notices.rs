//! `xtask notices [--check]` — generate (or verify) the committed `THIRD-PARTY-NOTICES`.
//!
//! Why the file exists: `deny.toml`'s `[licenses] allow` list is entirely permissive, and every
//! licence on it bar `CC0-1.0`/`0BSD` requires its notice text to accompany a *binary*
//! distribution. The images are exactly that, and the attested SBOM is not a substitute — it
//! records composition and carries no licence text at all. So the notices ship inside every
//! image beside `LICENSE` (asserted by `deploy/docker/cst/*.yaml`) and are served to readers by
//! `services/frontend`, whose WASM bundle is a binary distribution of its own.
//!
//! The document is the concatenation of two independent dependency graphs, because the frontend
//! image serves both a musl binary from the host workspace and a WASM bundle built from
//! `web/frontend`, which is its own workspace with its own lockfile (root `Cargo.toml`
//! `exclude`).
//!
//! Generation runs `--frozen`, so it reads only the two lockfiles and the extracted crate
//! sources: no network, no <https://clearlydefined.io> round-trip, no build timestamp. That is
//! what makes `--check`'s comparison a statement about this repository rather than about a
//! third-party service's mood.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

/// The committed artefact, at the repository root beside `LICENSE`.
///
/// Extensionless on purpose: `.dockerignore` excludes `**/*.md` wholesale, so a
/// `THIRD-PARTY-NOTICES.md` would be absent from the build context — the images would ship
/// without it and only the structure tests would say so.
const ARTEFACT: &str = "THIRD-PARTY-NOTICES";

/// One rendered section: a workspace, and how to name it to a reader.
struct Section {
    /// Section heading in the generated file.
    title: &'static str,
    /// Manifest directory relative to the repository root; `""` is the host workspace.
    dir: &'static str,
    /// Names this section's scratch file under `target/` (see [`harvest`]).
    slug: &'static str,
    /// What the crates listed in this section end up inside.
    ships_as: &'static str,
    /// Extra arguments. The host workspace needs `--workspace` (26 members, no root package);
    /// `web/frontend` is a single package that *is* its workspace, and passing `--workspace`
    /// there would be a no-op at best.
    args: &'static [&'static str],
}

/// The two graphs, in the order they appear in the document.
const SECTIONS: &[Section] = &[
    Section {
        title: "Part 1 of 2 — backend services",
        dir: "",
        slug: "backend",
        ships_as: "the eight service binaries (api, bootstrap, worker, control-plane, notifier, \
                   sync, challenge-solver, render) and the frontend image's static file server",
        args: &["--workspace"],
    },
    Section {
        title: "Part 2 of 2 — browser bundle (the SPA)",
        dir: "web/frontend",
        slug: "frontend",
        ships_as: "the WebAssembly bundle the frontend image serves, which executes in the \
                   reader's browser",
        args: &[],
    },
];

/// Generate the notices file, or with `check` compare the freshly rendered document against the
/// committed one without writing.
///
/// # Errors
/// When `cargo-about` is absent or fails (an unsatisfiable licence is a failure, by `--fail`),
/// when the artefact cannot be written, or — under `check` — when the committed file differs
/// from what the current lockfiles produce.
pub(crate) fn run(root: &Path, check: bool) -> Result<()> {
    let mut rendered = header();
    for section in SECTIONS {
        rendered.push_str(&banner(section));
        rendered.push_str(&render(&harvest(root, section)?));
    }

    let path = root.join(ARTEFACT);
    if check {
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        // Line endings normalised before comparing, belt-and-braces with `.gitattributes`'
        // `eol=lf` pin: this repository is developed on Windows and `cargo about` renders LF, so
        // without either half the gate would fail on every Windows checkout and pass in CI.
        if normalise(&current) != normalise(&rendered) {
            bail!(
                "{} is out of date; run `cargo run -p xtask -- notices`",
                path.display()
            );
        }
        println!("{ARTEFACT} is up to date");
        return Ok(());
    }

    std::fs::write(&path, &rendered).with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {} ({} bytes)", path.display(), rendered.len());
    Ok(())
}

/// Strip `\r` so a CRLF working copy and an LF render compare equal.
fn normalise(text: &str) -> String {
    text.replace('\r', "")
}

/// Write the same harvest as JSON, for the SPA's `/licenses` screen to render.
///
/// **Not a committed artefact, and deliberately so.** It is a second representation of the
/// ~500 KB already in `THIRD-PARTY-NOTICES`, and committing it would double the weight of the
/// repository's generated files to gain a drift gate over data the plain-text `--check` already
/// covers — the two come out of one [`merge`], so a lockfile that would change this one changes
/// that one too. The image build runs this into the frontend image instead; a checkout that has
/// not run it renders the screen's unavailable state.
///
/// # Errors
/// When `cargo-about` is absent or fails, or when the document cannot be written.
pub(crate) fn run_json(root: &Path, out: &Path) -> Result<()> {
    // Both harvests first, and held: a licence's name and id are borrowed out of the harvest
    // they came from, so a per-section loop that dropped each one would take the document's
    // strings with it.
    let mut harvests = Vec::with_capacity(SECTIONS.len());
    for section in SECTIONS {
        harvests.push(harvest(root, section)?);
    }

    let sections = SECTIONS
        .iter()
        .zip(&harvests)
        .map(|(section, harvest)| DocumentSection {
            slug: section.slug,
            title: section.title,
            ships_as: section.ships_as,
            licences: merge(harvest)
                .into_iter()
                .map(|licence| DocumentLicence {
                    id: licence.id,
                    name: licence.name,
                    crates: covered(&licence.notices),
                    notices: licence.notices,
                })
                .collect(),
        })
        .collect();

    // Compact, not pretty: nothing reads this by eye, and the indentation of a document that is
    // four fifths licence text is four fifths of nothing.
    let json = serde_json::to_vec(&Document { sections }).context("serialising the inventory")?;
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(out, &json).with_context(|| format!("writing {}", out.display()))?;
    println!("wrote {} ({} bytes)", out.display(), json.len());
    Ok(())
}

/// The structured inventory, one entry per dependency graph.
///
/// The schema `web/frontend/src/models.rs` mirrors — a separate workspace, so nothing but
/// `xtask repo-lint` holds the two in step.
#[derive(Serialize)]
struct Document<'a> {
    sections: Vec<DocumentSection<'a>>,
}

/// One graph: what it ships as, and the licences it resolves to.
#[derive(Serialize)]
struct DocumentSection<'a> {
    /// Stable key the SPA translates — `backend` or `frontend`. The prose below is the fallback
    /// for a locale that carries no string for it, so a new section is never nameless.
    slug: &'a str,
    title: &'a str,
    ships_as: &'a str,
    licences: Vec<DocumentLicence<'a>>,
}

/// One licence, and every distinct notice reproduced under it.
#[derive(Serialize)]
struct DocumentLicence<'a> {
    id: &'a str,
    name: &'a str,
    /// Distinct crates covered — the number the plain-text summary line carries, from the same
    /// [`covered`], so the page and the document cannot disagree.
    crates: usize,
    notices: Vec<Notice>,
}

/// What `cargo about generate --format json` reports: one entry per licence file it harvested,
/// so the same licence appears many times over — see [`render`].
#[derive(Deserialize)]
struct Harvest {
    licenses: Vec<Licence>,
}

/// One harvested licence file: its text, and the crates it was found in.
#[derive(Deserialize)]
struct Licence {
    /// Human-readable name, e.g. `Apache License 2.0`.
    name: String,
    /// SPDX identifier, e.g. `Apache-2.0`.
    id: String,
    text: String,
    used_by: Vec<UsedBy>,
}

#[derive(Deserialize)]
struct UsedBy {
    #[serde(rename = "crate")]
    krate: CrateRef,
}

#[derive(Deserialize)]
struct CrateRef {
    name: String,
    version: String,
}

/// Run `cargo about generate` over one workspace and parse what it harvested.
///
/// JSON rather than the handlebars renderer, because the document this produces has to merge
/// duplicate licence texts and handlebars cannot (see [`render`]). Written to a file with
/// `--output-file` and read back rather than captured from stdout: cargo-about refuses to write
/// a redirected stdout under `PowerShell`, so capturing the pipe fails on the platform this
/// repository is developed on and works in CI.
fn harvest(root: &Path, section: &Section) -> Result<Harvest> {
    let scratch = root.join("target/xtask/notices");
    std::fs::create_dir_all(&scratch).with_context(|| format!("creating {}", scratch.display()))?;
    let harvested_to = scratch.join(format!("{}.json", section.slug));

    // `CARGO` pins the child to the toolchain running us, as `ci.rs` does for its gates.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let out = Command::new(cargo)
        .current_dir(root.join(section.dir))
        .args([
            "about",
            "generate",
            "--frozen",
            "--all-features",
            "--fail",
            "--format",
            "json",
        ])
        .args(section.args)
        .arg("--output-file")
        .arg(&harvested_to)
        .output()
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to run `cargo about` ({e}); install it with \
                 `cargo install cargo-about --locked`"
            )
        })?;

    if !out.status.success() {
        // cargo-about reports the unsatisfied crate and licence on stderr, and that message is
        // the entire diagnosis — an accepted-list decision, not a bug to go hunting for.
        bail!(
            "`cargo about generate` failed in {}: {}\n{}",
            if section.dir.is_empty() {
                "the workspace root"
            } else {
                section.dir
            },
            out.status,
            String::from_utf8_lossy(&out.stderr).trim(),
        );
    }

    let json = std::fs::read_to_string(&harvested_to)
        .with_context(|| format!("reading {}", harvested_to.display()))?;
    serde_json::from_str(&json).with_context(|| format!("parsing {}", harvested_to.display()))
}

/// One crate, as a notice names it.
///
/// Name and version apart rather than one `"name version"` string, because the JSON document
/// carries them as separate fields. Ordering is unchanged by the split: a crate name cannot
/// contain a space, so the space that separated them sorted below every character a name can
/// hold, and `(name, version)` orders exactly as `"name version"` did.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct Covered {
    name: String,
    version: String,
}

/// A licence text and every crate that shipped that exact text.
#[derive(Serialize)]
struct Notice {
    text: String,
    crates: Vec<Covered>,
}

/// One licence and its distinct notices, most-covered first.
///
/// What [`merge`] produces and both renderers consume, so the document a reader downloads and
/// the page a reader opens are the same grouping of the same harvest. Distinct from [`Licence`],
/// which is one harvested *file* as cargo-about reports it.
struct LicenceNotices<'a> {
    name: &'a str,
    id: &'a str,
    notices: Vec<Notice>,
}

/// Group one workspace's harvest into the distinct notices each licence covers.
///
/// **Identical notices are kept once.** cargo-about hands back one entry per licence *file*, so a
/// graph of 531 crates yields 531 copies of ~10 licences — the Apache-2.0 text alone appeared
/// 370 times in the first draft of this file, at 11 KB a copy: 989 KB of document against 332 KB
/// now.
///
/// **Merging is on the text, not on the SPDX id.** An MIT file *is* mostly its copyright line,
/// and the 72 distinct MIT notices in this graph name 72 different copyright holders; collapsing
/// those to one would drop exactly the part the licence requires be reproduced. Whitespace is
/// folded for the comparison only — two texts differing by a line wrap are the same notice — and
/// the first copy is kept verbatim. See [`cluster`] for the near-identical case, which is a
/// device of the plain-text rendering alone.
fn merge(harvest: &Harvest) -> Vec<LicenceNotices<'_>> {
    // (name, id) -> normalised text -> the notice. `BTreeMap` throughout: the output is
    // byte-compared by `--check`, so iteration order has to be a property of the data and not of
    // a hasher.
    let mut by_licence: BTreeMap<(&str, &str), BTreeMap<String, Notice>> = BTreeMap::new();
    for licence in &harvest.licenses {
        let notices = by_licence
            .entry((licence.name.as_str(), licence.id.as_str()))
            .or_default();
        let notice = notices
            .entry(whitespace_folded(&licence.text))
            .or_insert_with(|| Notice {
                text: normalise(&licence.text),
                crates: Vec::new(),
            });
        for used_by in &licence.used_by {
            notice.crates.push(Covered {
                name: used_by.krate.name.clone(),
                version: used_by.krate.version.clone(),
            });
        }
    }

    // Ordered by how much of the graph each licence covers, which is also the order the summary
    // is read in. Ties break on the id so the document is stable.
    let mut licences: Vec<LicenceNotices<'_>> = by_licence
        .into_iter()
        .map(|((name, id), notices)| {
            let mut notices: Vec<Notice> = notices
                .into_values()
                .map(|mut notice| {
                    notice.crates.sort_unstable();
                    notice.crates.dedup();
                    notice
                })
                .collect();
            // Most-covered notice first, as the licences themselves are ordered. The text
            // renderer re-sorts within a cluster; this is the order the JSON keeps.
            notices.sort_by(|a, b| {
                b.crates
                    .len()
                    .cmp(&a.crates.len())
                    .then_with(|| a.crates.cmp(&b.crates))
            });
            LicenceNotices { name, id, notices }
        })
        .collect();
    licences.sort_by(|a, b| {
        covered(&b.notices)
            .cmp(&covered(&a.notices))
            .then(a.id.cmp(b.id))
    });
    licences
}

/// Distinct crates covered by a licence's notices.
///
/// Distinct, not the sum over notices: a crate whose vendored files carry several notices under
/// one licence — `ring` is the case, with fifteen ISC notices of its own — is one crate under
/// that licence. Summing counted it once per notice, which is how the ISC summary line came to
/// claim more crates than the section beneath it named.
fn covered(notices: &[Notice]) -> usize {
    notices
        .iter()
        .flat_map(|notice| notice.crates.iter())
        .collect::<BTreeSet<_>>()
        .len()
}

/// Render one workspace's merged harvest as the plain-text section.
fn render(harvest: &Harvest) -> String {
    let licences = merge(harvest);

    let mut out = String::from("Licences in this section, by number of crates:\n\n");
    for licence in &licences {
        let notices = licence.notices.len();
        let _ = writeln!(
            out,
            "  {} ({}) — {}{}",
            licence.name,
            licence.id,
            plural(covered(&licence.notices), "crate"),
            if notices > 1 {
                format!(", {}", plural(notices, "distinct notice"))
            } else {
                String::new()
            },
        );
    }

    for licence in licences {
        let (name, id) = (licence.name, licence.id);
        let _ = write!(out, "\n{RULE}\n{name} ({id})\n{RULE}\n");
        for cluster in &cluster(licence.notices) {
            let shared = cluster.opening;
            if shared > 0 {
                let _ = write!(
                    out,
                    "\nThe {} below open with the same text, reproduced here once. Each block\n\
                     that follows completes one of them: a crate's notice is this text followed\n\
                     by the block naming that crate.\n\n{}",
                    plural(cluster.notices.len(), "notice"),
                    &cluster.notices[0].text[..shared],
                );
            }
            for notice in &cluster.notices {
                out.push_str(if shared > 0 {
                    "\n… continued, for:\n\n"
                } else {
                    "\nApplies to:\n\n"
                });
                for krate in &notice.crates {
                    let _ = writeln!(out, "  - {} {}", krate.name, krate.version);
                }
                out.push('\n');
                out.push_str(notice.text[shared..].trim_end());
                out.push('\n');
            }
        }
    }
    out
}

/// `1 crate` / `2 crates`, because this document is read by people and "1 crates" reads as a bug
/// in the thing that generated it.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Notices that open alike, and how much of that opening they share.
struct Cluster {
    /// Bytes of `notices[0].text` common to every notice here; `0` when they share too little to
    /// be worth factoring out, in which case each is printed whole.
    opening: usize,
    notices: Vec<Notice>,
}

/// Least bytes of shared opening worth factoring out. Below this the split costs a reader more
/// than it saves: the point is the 9.7 KB Apache-2.0 body, not a shared `Copyright (c) `.
const MIN_SHARED_OPENING: usize = 1024;

/// Group notices by the opening they share, so one licence body is printed once per group
/// instead of once per crate.
///
/// Apache-2.0 is the case this exists for: 37 distinct notices in this graph, nearly all of them
/// the same 9.7 KB body with a different attribution filled into the appendix — 400 KB of the
/// same text. **Clustered, deliberately, rather than factoring out one opening common to the
/// whole licence**: the first attempt did the latter and silently did nothing, because a single
/// copy that differs in its opening line drops the common prefix to zero for all 37.
///
/// **The split is lossless, which is the only reason it is acceptable in this document.** An
/// opening and a block concatenate back to the bytes that crate shipped, up to trailing
/// whitespace — nothing is summarised, dropped or reformatted.
fn cluster(mut notices: Vec<Notice>) -> Vec<Cluster> {
    // By text, so notices sharing an opening are adjacent and the grouping is a single pass.
    notices.sort_by(|a, b| a.text.cmp(&b.text));

    let mut clusters: Vec<Cluster> = Vec::new();
    for notice in notices {
        // Common prefixes against one representative are nested, so the running minimum *is* the
        // opening shared by everything in the cluster.
        let joins = clusters.last().map(|last| {
            last.opening
                .min(shared_opening(&last.notices[0].text, &notice.text))
        });
        match joins {
            Some(opening) if opening >= MIN_SHARED_OPENING => {
                let last = clusters.last_mut().expect("just inspected");
                last.opening = opening;
                last.notices.push(notice);
            }
            _ => clusters.push(Cluster {
                opening: usize::MAX,
                notices: vec![notice],
            }),
        }
    }

    for cluster in &mut clusters {
        if cluster.notices.len() < 2 {
            cluster.opening = 0;
        }
        // Most-used notice first, as the licences themselves are ordered.
        cluster.notices.sort_by(|a, b| {
            b.crates
                .len()
                .cmp(&a.crates.len())
                .then_with(|| a.crates.cmp(&b.crates))
        });
    }
    clusters.sort_by(|a, b| {
        cluster_crates(b)
            .cmp(&cluster_crates(a))
            .then_with(|| a.notices[0].crates.cmp(&b.notices[0].crates))
    });
    clusters
}

/// How much of two notices is a byte-identical opening, cut back to a line boundary.
fn shared_opening(a: &str, b: &str) -> usize {
    let common = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    // Cut at the end of the last wholly-shared line, found over *bytes*: the common run is a byte
    // count and can land inside a multi-byte character, which slicing would panic on. An index
    // just past a newline is always a `char` boundary, so both halves slice safely — and neither
    // half ends mid-word, which a raw byte cut would allow.
    a.as_bytes()[..common]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |line_end| line_end + 1)
}

/// Crates covered by one cluster.
fn cluster_crates(cluster: &Cluster) -> usize {
    cluster
        .notices
        .iter()
        .map(|notice| notice.crates.len())
        .sum()
}

/// The horizontal rule between sections.
const RULE: &str =
    "--------------------------------------------------------------------------------";

/// Fold every run of whitespace to a single space, for comparison only.
///
/// Two licence files that differ solely in line wrapping or trailing whitespace are the same
/// notice; without this the Apache-2.0 text appears 54 times instead of the number of genuinely
/// different Apache-2.0 notices in the graph.
fn whitespace_folded(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The document's preamble.
fn header() -> String {
    let mut parts = String::new();
    for section in SECTIONS {
        let _ = writeln!(parts, "  - {}", section.title);
    }
    format!(
        "\
================================================================================
THIRD-PARTY NOTICES
================================================================================

Generated by `cargo run -p xtask -- notices`. DO NOT EDIT — every edit is
overwritten by the next regeneration, and CI's `notices` job fails on any
difference between this file and what the lockfiles produce.

TankoVault itself is licensed under PolyForm Noncommercial 1.0.0; those terms are
in `LICENSE`, beside this file and at `/LICENSE` inside every image. Nothing
below changes them. What follows are the licences of the third-party crates
TankoVault is built from, reproduced because a binary distribution is where
almost all of them require their text to travel.

Two things to know when reading a section:

  - A crate is listed under one licence even where it offers a choice. The
    choice is made once, in priority order, by `accepted` in `about.toml`, so a
    crate offered as `Apache-2.0 OR MIT` appears under Apache-2.0 only. Those
    crates also permit terms not printed here.
  - One licence can be followed by several texts. Copies that differ only in
    whitespace are merged, but an MIT or BSD notice is mostly its copyright
    line, and those differ per crate — every distinct one is reproduced.

Two sections follow, one per dependency graph — see `about.toml` and
`web/frontend/about.toml`:

{parts}",
    )
}

/// The heading that introduces one section.
fn banner(section: &Section) -> String {
    format!(
        "\n\
         ================================================================================\n\
         {}\n\
         ================================================================================\n\
         \n\
         Ships as: {}.\n\
         \n",
        section.title, section.ships_as,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ARTEFACT, Harvest, SECTIONS, banner, header, normalise, render, whitespace_folded,
    };

    /// Build a harvest the way cargo-about reports one: one entry per licence *file*, so the
    /// same licence recurs once per crate that carries it.
    fn harvest(entries: &[(&str, &str, &str, &[&str])]) -> Harvest {
        let licenses = entries
            .iter()
            .map(|(name, id, text, crates)| {
                let used_by = crates
                    .iter()
                    .map(|krate| {
                        serde_json::json!({ "crate": { "name": krate, "version": "1.0.0" } })
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({
                    "name": name, "id": id, "text": text, "used_by": used_by,
                })
            })
            .collect::<Vec<_>>();
        serde_json::from_value(serde_json::json!({ "licenses": licenses })).unwrap()
    }

    /// The reason this renderer exists instead of a handlebars template. cargo-about emits one
    /// entry per licence file, and the first generated draft of this artefact was 989 KB
    /// containing the Apache-2.0 text 370 times — one copy per crate. Handlebars cannot merge
    /// them; the merge is why the JSON output is parsed at all.
    #[test]
    fn an_identical_notice_is_printed_once_however_many_crates_carry_it() {
        const APACHE: &str = "Apache License\nVersion 2.0, January 2004\n";
        let rendered = render(&harvest(&[
            ("Apache License 2.0", "Apache-2.0", APACHE, &["alpha"]),
            ("Apache License 2.0", "Apache-2.0", APACHE, &["beta"]),
            ("Apache License 2.0", "Apache-2.0", APACHE, &["gamma"]),
        ]));

        assert_eq!(
            rendered.matches("Version 2.0, January 2004").count(),
            1,
            "the text must appear once:\n{rendered}"
        );
        // …and every crate is still attributed, which is the point of merging rather than
        // dropping.
        for krate in ["alpha 1.0.0", "beta 1.0.0", "gamma 1.0.0"] {
            assert!(rendered.contains(krate), "{krate} lost its attribution");
        }
        assert!(
            !rendered.contains("distinct notices"),
            "one text is not several"
        );
    }

    /// The other half of the merge, and the one with legal teeth: an MIT notice is mostly its
    /// copyright line, so two MIT files that name different holders are two notices. Merging on
    /// the SPDX id instead of the text would print one of them and silently drop the other —
    /// dropping exactly the part MIT requires be reproduced.
    #[test]
    fn notices_differing_only_in_their_copyright_line_are_both_printed() {
        let rendered = render(&harvest(&[
            (
                "MIT License",
                "MIT",
                "Copyright (c) 2016 Alpha Authors\nPermission is hereby granted...",
                &["alpha"],
            ),
            (
                "MIT License",
                "MIT",
                "Copyright (c) 2021 Beta Authors\nPermission is hereby granted...",
                &["beta"],
            ),
        ]));

        assert!(rendered.contains("Copyright (c) 2016 Alpha Authors"));
        assert!(rendered.contains("Copyright (c) 2021 Beta Authors"));
        assert!(
            rendered.contains("2 distinct notices"),
            "the reader is told why the licence repeats:\n{rendered}"
        );
    }

    /// Whitespace-only differences are the same notice — a re-wrapped copy of a licence is not a
    /// different licence — and folding them is what keeps the merge effective.
    #[test]
    fn a_rewrapped_copy_is_the_same_notice() {
        assert_eq!(
            whitespace_folded("Permission is\nhereby   granted"),
            whitespace_folded("Permission  is hereby\r\ngranted\n")
        );
        // But not so eager that different words collapse.
        assert_ne!(whitespace_folded("MIT"), whitespace_folded("MIT License"));

        let rendered = render(&harvest(&[
            (
                "ISC License",
                "ISC",
                "Permission to use\nand modify",
                &["a"],
            ),
            (
                "ISC License",
                "ISC",
                "Permission to use and modify\n",
                &["b"],
            ),
        ]));
        assert_eq!(rendered.matches("Permission to use").count(), 1);
    }

    /// The shared-opening split, and the property that makes it acceptable at all: what is
    /// printed still **reconstructs each crate's notice byte for byte**. Apache-2.0 is why it
    /// exists — 37 notices in this graph share a 9.7 KB body and differ only in the attribution
    /// filled into the appendix, which is 400 KB of the same text printed over and over.
    ///
    /// If this test is ever in the way, the thing to check is reconstruction, not the size: a
    /// notices file that paraphrases a licence discharges nothing.
    #[test]
    fn a_shared_opening_is_printed_once_and_still_reconstructs_every_notice() {
        let body = format!(
            "Apache License\n{}\nAPPENDIX\n",
            "boilerplate\n".repeat(200)
        );
        let alpha = format!("{body}Copyright 2016 Alpha Authors\n");
        let beta = format!("{body}Copyright 2021 Beta Authors\n");
        let rendered = render(&harvest(&[
            ("Apache License 2.0", "Apache-2.0", &alpha, &["alpha"]),
            ("Apache License 2.0", "Apache-2.0", &beta, &["beta"]),
        ]));

        assert_eq!(
            rendered.matches("APPENDIX").count(),
            1,
            "the shared body must be printed once"
        );
        assert!(rendered.contains(&body), "…and verbatim");
        // Each crate's own notice is the printed opening plus its own block, which is exactly
        // the text that crate shipped.
        for original in [&alpha, &beta] {
            let tail = original.strip_prefix(&body).unwrap();
            assert!(rendered.contains(tail.trim_end()), "{tail} was dropped");
            assert_eq!(format!("{body}{tail}"), *original);
        }
    }

    /// The split must not fire where it would only fragment a notice. MIT files begin with their
    /// copyright line, so two of them share almost nothing — printing "the shared opening" of
    /// `Copyright (c) ` and then two remainders would be strictly worse to read than two notices.
    #[test]
    fn notices_that_share_only_a_few_bytes_are_printed_whole() {
        let rendered = render(&harvest(&[
            (
                "MIT License",
                "MIT",
                "Copyright (c) 2016 Alpha\nPermission is hereby granted",
                &["alpha"],
            ),
            (
                "MIT License",
                "MIT",
                "Copyright (c) 2021 Beta\nPermission is hereby granted",
                &["beta"],
            ),
        ]));

        assert!(
            !rendered.contains("continued, for"),
            "no split:\n{rendered}"
        );
        assert_eq!(rendered.matches("Permission is hereby granted").count(), 2);
    }

    /// Licences are ordered by how much of the graph they cover, and the order must come from
    /// the data rather than from a hasher: `--check` compares the whole document, so an
    /// unstable order would fail the gate on a tree nobody had touched.
    #[test]
    fn the_document_is_ordered_by_coverage_and_is_stable() {
        let entries: &[(&str, &str, &str, &[&str])] = &[
            ("ISC License", "ISC", "ISC text", &["solo"]),
            ("MIT License", "MIT", "MIT text", &["one", "two", "three"]),
            ("zlib License", "Zlib", "zlib text", &["pair", "mate"]),
        ];
        let rendered = render(&harvest(entries));
        let position = |needle: &str| rendered.find(needle).expect(needle);
        assert!(position("MIT License (MIT)") < position("zlib License (Zlib)"));
        assert!(position("zlib License (Zlib)") < position("ISC License (ISC)"));

        // Same input, different insertion order: same bytes out.
        let shuffled: Vec<_> = entries.iter().rev().copied().collect();
        assert_eq!(rendered, render(&harvest(&shuffled)));
    }

    /// The artefact must not gain an extension. `.dockerignore` excludes `**/*.md` and
    /// `**/*.txt` is the kind of thing it may exclude next; the images copy this path verbatim,
    /// and a rename would make the `COPY` fail the build rather than ship a wrong file — but
    /// only after a full image build, which is the slowest place to learn it.
    #[test]
    fn the_artefact_name_is_extensionless() {
        assert!(
            !ARTEFACT.contains('.'),
            "`{ARTEFACT}` has an extension; `.dockerignore` excludes whole extensions and the \
             file would silently leave the build context"
        );
    }

    /// Both graphs are rendered. The frontend one is the easy one to lose: it is a separate
    /// workspace, so every root-level `cargo` command misses it, and its crates would then be
    /// distributed in the WASM bundle with no notice at all.
    #[test]
    fn both_workspaces_are_covered() {
        assert_eq!(SECTIONS.len(), 2, "backend and web/frontend");
        assert!(
            SECTIONS.iter().any(|s| s.dir == "web/frontend"),
            "the SPA's own dependency graph must have a section"
        );
        assert!(
            SECTIONS.iter().any(|s| s.dir.is_empty()),
            "the host workspace must have a section"
        );
    }

    /// `--workspace` belongs to the host workspace alone: `web/frontend` is a single package
    /// that is its own workspace root.
    #[test]
    fn only_the_host_workspace_is_scanned_as_a_workspace() {
        for section in SECTIONS {
            assert_eq!(
                section.args.contains(&"--workspace"),
                section.dir.is_empty(),
                "section `{}` has the wrong workspace flag",
                section.title
            );
        }
    }

    /// The preamble has to say the file is generated, or someone will fix a typo in 300-odd KB
    /// of licence text and lose it on the next regeneration.
    #[test]
    fn the_header_says_it_is_generated_and_names_the_command() {
        let header = header();
        assert!(header.contains("DO NOT EDIT"));
        assert!(header.contains("xtask -- notices"));
        // The project's own licence is a different thing from its dependencies', and the file
        // that would otherwise be mistaken for it sits right next to it.
        assert!(header.contains("LICENSE"));
        for section in SECTIONS {
            assert!(
                header.contains(section.title),
                "the header does not list `{}`",
                section.title
            );
        }
    }

    /// Each section says what it ships inside, since "which of these am I actually
    /// distributing" is the only question a reader of this file has.
    #[test]
    fn every_section_banner_names_what_it_ships_in() {
        for section in SECTIONS {
            let banner = banner(section);
            assert!(banner.contains(section.title));
            assert!(banner.contains(section.ships_as));
        }
    }

    /// Pins why `--check` compares normalised text rather than bytes: the renderer emits LF and
    /// this repository is developed on Windows, so a byte comparison would be a gate that fails
    /// locally and passes in CI — the shape of failure that gets a gate deleted rather than read.
    #[test]
    fn line_endings_do_not_decide_the_check() {
        assert_eq!(normalise("a\r\nb\r\n"), normalise("a\nb\n"));
        // Normalisation must not be so eager that it hides real drift.
        assert_ne!(normalise("MIT\n"), normalise("MIT License\n"));
    }
}
