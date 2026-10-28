//! The integration harness, CI's service containers and production run one Postgres.

use crate::repo_lint::Finding;
use crate::repo_lint::text::{const_str, is_comment};
use std::path::{Path, PathBuf};

/// The compose file that pins production's Postgres.
const COMPOSE: &str = "deploy/docker-compose.yml";

/// The Postgres distributions the compose file and the workflows may pin, as
/// `image: <name>:<tag>`.
///
/// Enumerated so an arbitrary `image:` line cannot satisfy a rule about which Postgres runs.
/// Stock `postgres` stays listed so the rule keeps working if pgvector is ever dropped.
const POSTGRES_IMAGES: &[&str] = &["postgres", "pgvector/pgvector"];

/// **The integration harness must run production's Postgres image *and* major.** The query
/// planner is a major-version artefact, so `crates/db/tests/repo_query_plans.rs` — which asserts
/// that the trigram searches reach their GIN indexes instead of scanning the whole catalogue —
/// proves nothing about production the moment the two majors diverge, and proves it silently,
/// because no output of a green run names a version. Nothing else connects them: the harness tag
/// is a Rust `const`, the deployed image is a digest-pinned line in the compose file, and a bump
/// to either one alone is a perfectly ordinary-looking change.
///
/// The **image name** is checked for a second reason, added when migration 0027 made pgvector a
/// hard dependency. A harness on `pgvector/pgvector` with production on stock `postgres` is the
/// dangerous direction: every test passes, because the tests can run `CREATE EXTENSION vector`,
/// and the deployment then fails on its first migration. The reverse at least fails loudly and
/// immediately. Only the silent direction needs a rule, but comparing the names catches both.
///
/// There are **three** copies of this decision, not two, and the third is the one nothing pointed
/// at: CI's own service containers. The `sqlx offline cache` job pins a Postgres of its own and
/// applies every migration to it. On the wrong image that job dies at `CREATE EXTENSION vector`
/// having verified nothing, and it reports a missing extension rather than a wrong pin — so the
/// obvious reading is "the migration is broken", not "this job runs a different database than
/// production does". Every `image:` line under `.github/workflows/` naming a Postgres is held to
/// the compose file too.
pub(in crate::repo_lint) fn tests_run_the_production_postgres_major(
    root: &Path,
) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "tests-run-the-production-postgres-major";
    const HARNESS: &str = "crates/test-support/src/lib.rs";
    let harness_path = root.join(HARNESS);
    let Ok(harness) = std::fs::read_to_string(&harness_path) else {
        anyhow::bail!("repo-lint: cannot read {}", harness_path.display());
    };
    let Some((tag_line, tag)) = const_str(&harness, "POSTGRES_TAG") else {
        anyhow::bail!(
            "repo-lint: {} declares no `const POSTGRES_TAG: &str = \"…\"`; nothing else records \
             which Postgres major the integration suites run",
            harness_path.display()
        );
    };
    let harness_major = major_of(&tag).unwrap_or(tag.as_str()).to_owned();
    let Some((_, harness_image)) = const_str(&harness, "POSTGRES_IMAGE") else {
        anyhow::bail!(
            "repo-lint: {} declares no `const POSTGRES_IMAGE: &str = \"…\"`; nothing else \
             records which Postgres distribution the integration suites run, and stock \
             `postgres` cannot apply migration 0027",
            harness_path.display()
        );
    };

    let compose_path = root.join(COMPOSE);
    let Ok(compose) = std::fs::read_to_string(&compose_path) else {
        anyhow::bail!("repo-lint: cannot read {}", compose_path.display());
    };
    let deployed = compose.lines().enumerate().find_map(|(index, line)| {
        let trimmed = line.trim_start();
        if is_comment(trimmed) {
            return None;
        }
        let (name, tag) = image_reference(trimmed)?;
        POSTGRES_IMAGES
            .contains(&name)
            .then(|| Some((index + 1, name.to_owned(), major_of(tag)?.to_owned())))?
    });
    let Some((compose_line, compose_image, compose_major)) = deployed else {
        anyhow::bail!(
            "repo-lint: {} pins none of {POSTGRES_IMAGES:?} as `image: <name>:<tag>`; the rule \
             cannot tell which Postgres production runs",
            compose_path.display()
        );
    };

    let mut findings =
        workflow_postgres_findings(root, RULE, &compose_image, &compose_major, compose_line);

    if harness_major != compose_major {
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(HARNESS),
            line: tag_line,
            detail: format!(
                "tests run Postgres {harness_major}, production runs {compose_major} \
                 ({COMPOSE}:{compose_line}); plan assertions are only evidence when they match"
            ),
        });
    }
    if harness_image != compose_image {
        findings.push(Finding {
            rule: RULE,
            file: PathBuf::from(HARNESS),
            line: tag_line,
            detail: format!(
                "tests run `{harness_image}`, production runs `{compose_image}` \
                 ({COMPOSE}:{compose_line}); migration 0027 needs `CREATE EXTENSION vector`, so \
                 a harness with the extension and a deployment without it is a green suite and \
                 a failed migration"
            ),
        });
    }
    Ok(findings)
}

/// Every Postgres a workflow pins for itself, held to the compose file.
///
/// Split out of [`tests_run_the_production_postgres_major`] only for length; the reasoning lives
/// in that function's documentation.
fn workflow_postgres_findings(
    root: &Path,
    rule: &'static str,
    compose_image: &str,
    compose_major: &str,
    compose_line: usize,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for entry in std::fs::read_dir(root.join(".github/workflows"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|ext| ext == "yml" || ext == "yaml")
        {
            continue;
        }
        let Ok(workflow) = std::fs::read_to_string(&path) else {
            continue;
        };
        let workflow_name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );

        for (index, line) in workflow.lines().enumerate() {
            let trimmed = line.trim_start();
            if is_comment(trimmed) {
                continue;
            }
            let Some((name, tag)) = image_reference(trimmed) else {
                continue;
            };
            if !POSTGRES_IMAGES.contains(&name) {
                continue;
            }
            if name != compose_image || major_of(tag).is_none_or(|major| major != compose_major) {
                findings.push(Finding {
                    rule,
                    file: PathBuf::from(format!(".github/workflows/{workflow_name}")),
                    line: index + 1,
                    detail: format!(
                        "this job runs `{name}:{tag}`, production runs `{compose_image}` \
                         {compose_major} ({COMPOSE}:{compose_line}); a job that applies the \
                         migrations against a different Postgres verifies nothing about the one \
                         that runs them"
                    ),
                });
            }
        }
    }
    findings
}

/// Split a compose `image: <repository>:<tag>[@<digest>]` line into repository and tag.
///
/// Split out so the parse can be tested without a filesystem — and it needs testing, because
/// both separators appear twice in a real pinned line. The digest is dropped first: `sha256:…`
/// carries the same `:` as the tag, so taking the last colon of the whole reference lands inside
/// the hex and reads it as a version. Only then is the repository split off from the right,
/// which is what lets a registry host with a port (`ghcr.io:443/…`) keep working.
fn image_reference(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("image: ")?;
    let reference = rest.split_once('@').map_or(rest, |(before, _)| before);
    reference.rsplit_once(':')
}

/// The major version leading a Postgres image tag: `18` from `18-alpine`, from
/// `18.4-alpine@sha256:…`, and from a bare `18`.
///
/// Split out so the shapes a digest pin and a Renovate bump produce can be tested without a
/// filesystem — the `@sha256:` suffix is the one that looks like it would not parse.
fn major_of(tag: &str) -> Option<&str> {
    let major = tag.trim_matches('"').split(['-', '.', '@']).next()?.trim();
    // `pgvector/pgvector` tags its images `pg<major>`; the stock image uses the bare major. The
    // prefix is stripped rather than special-cased at the call site so both spellings compare as
    // the same number — which is the only thing this rule is actually about.
    let major = major.strip_prefix("pg").unwrap_or(major);
    (!major.is_empty() && major.bytes().all(|b| b.is_ascii_digit())).then_some(major)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compose file pins images by digest, so the tag the rule has to read is
    /// `18-alpine@sha256:…` rather than anything as tidy as `18`.
    #[test]
    fn a_postgres_major_survives_the_shapes_a_pin_produces() {
        assert_eq!(major_of("18-alpine"), Some("18"));
        assert_eq!(
            major_of("18-alpine@sha256:9a8afca54e7861fd90fab5fdf4c42477a6b1cb7d29"),
            Some("18")
        );
        assert_eq!(major_of("18.4-alpine"), Some("18"));
        assert_eq!(major_of("18"), Some("18"));
        assert_eq!(major_of("\"17-alpine\""), Some("17"));
        // `pgvector/pgvector` tags as `pg<major>`; both spellings must reduce to the same number,
        // or the harness and the compose file can never agree after the pgvector move.
        assert_eq!(major_of("pg18"), Some("18"));
        assert_eq!(major_of("\"pg18\""), Some("18"));
        assert_eq!(
            major_of("pg18@sha256:691673308c99d2161ba298736f3147"),
            Some("18")
        );
        // Not a version: a floating tag pins nothing, so the rule must not read one as agreement.
        assert_eq!(major_of("latest"), None);
        assert_eq!(major_of("pglatest"), None);
        assert_eq!(major_of(""), None);
    }

    /// The digest and the tag are separated by the same character, and a real compose line
    /// carries both.
    ///
    /// The bug this pins: splitting the whole reference on its last colon lands inside
    /// `sha256:<hex>`, so the "tag" becomes hex, `major_of` rejects it, and the rule reports that
    /// the compose file pins no Postgres at all — while looking directly at the line that does.
    #[test]
    fn an_image_reference_survives_a_digest_pin_and_a_registry_port() {
        assert_eq!(
            image_reference("image: postgres:18-alpine@sha256:9a8afca54e7861fd90fab5fdf4c42"),
            Some(("postgres", "18-alpine"))
        );
        assert_eq!(
            image_reference("image: pgvector/pgvector:pg18@sha256:691673308c99d2161ba29873"),
            Some(("pgvector/pgvector", "pg18"))
        );
        // No digest, and a registry host carrying a port: the repository half has its own colon.
        assert_eq!(
            image_reference("image: pgvector/pgvector:pg18"),
            Some(("pgvector/pgvector", "pg18"))
        );
        assert_eq!(
            image_reference("image: ghcr.io:443/timschoenle/tankovault:1.2.0"),
            Some(("ghcr.io:443/timschoenle/tankovault", "1.2.0"))
        );
        // Not an image line at all, and an image with no tag: both must decline rather than
        // guess, or an untagged image would be read as pinning something.
        assert_eq!(image_reference("environment:"), None);
        assert_eq!(image_reference("image: postgres"), None);
    }
}
