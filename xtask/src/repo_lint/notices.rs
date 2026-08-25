//! Rules over `THIRD-PARTY-NOTICES`: the licence allowlists must agree, and the route the
//! server publishes must be the one the frontend links.

use std::path::Path;

use super::Finding;
use super::text::{const_str, toml_string_array};

/// **Every licence `cargo-deny` admits must be one the notices generator accepts, and vice
/// versa.** `deny.toml`'s `[licenses] allow` decides what may enter the dependency graph;
/// `about.toml`'s `accepted` decides which licence each crate's notice is published under in
/// `THIRD-PARTY-NOTICES`. Nothing connects the two lists, and each direction of drift fails
/// differently: a licence cargo-deny admits but the generator does not accept stops generation
/// dead with `--fail` (loud, but only in the job that regenerates), while one the generator
/// accepts and cargo-deny forbids is a standing invitation to publish a notice for terms that
/// must never be in the graph at all — the GPL-3.0 note in `deny.toml` is exactly that hazard,
/// and it is silent.
///
/// `web/frontend/about.toml` is deliberately *not* held to this: no `deny.toml` covers that
/// workspace, so its shorter list is the only licence gate the browser bundle has, and widening
/// it to match this one would retire the gate.
pub(super) fn notices_accept_every_allowed_licence(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "notices-accept-every-allowed-licence";

    let deny = root.join("deny.toml");
    let about = root.join("about.toml");
    let (Ok(deny_text), Ok(about_text)) = (
        std::fs::read_to_string(&deny),
        std::fs::read_to_string(&about),
    ) else {
        anyhow::bail!(
            "repo-lint: cannot read {} and {}",
            deny.display(),
            about.display()
        );
    };

    let Some((allow_line, allowed)) = toml_string_array(&deny_text, "[licenses]", "allow") else {
        anyhow::bail!(
            "repo-lint: {} declares no `[licenses] allow = [...]`; an absent list is not an \
             empty one — cargo-deny would admit nothing",
            deny.display()
        );
    };
    let Some((accepted_line, accepted)) = toml_string_array(&about_text, "", "accepted") else {
        anyhow::bail!(
            "repo-lint: {} declares no `accepted = [...]`; the notices generator would satisfy \
             no crate at all",
            about.display()
        );
    };

    let mut findings = Vec::new();
    for licence in &allowed {
        if !accepted.contains(licence) {
            findings.push(Finding {
                rule: RULE,
                file: about.clone(),
                line: accepted_line,
                detail: format!(
                    "`deny.toml` admits `{licence}` into the graph and this list does not \
                     accept it — `xtask notices` fails on the first crate that uses it"
                ),
            });
        }
    }
    for licence in &accepted {
        if !allowed.contains(licence) {
            findings.push(Finding {
                rule: RULE,
                file: deny.clone(),
                line: allow_line,
                detail: format!(
                    "`about.toml` accepts `{licence}`, which this list forbids — either it \
                     belongs in the graph and belongs here, or it does not and must not be \
                     publishable as a notice"
                ),
            });
        }
    }
    Ok(findings)
}

/// **Every URL the SPA reaches its notices at must be one the frontend service publishes.**
/// `web/frontend` is a separate workspace, so the literal pairs have no compile-time
/// relationship. Getting them out of step does not 404: every unmatched path on that server
/// falls back to the app shell, so a stale link answers `200` with the application itself, and
/// the reader — who is owed those notices for the bundle their browser just ran — sees a page
/// that looks like it worked. The JSON half fails worse: the screen would parse the app shell,
/// fail, and report a corrupt inventory rather than an absent one.
pub(super) fn the_notices_url_is_the_one_the_server_publishes(
    root: &Path,
) -> anyhow::Result<Vec<Finding>> {
    const RULE: &str = "notices-url-matches-the-route";
    const SERVER: &str = "services/frontend/src/main.rs";
    const SPA: &str = "web/frontend/src/views/licenses.rs";
    /// The plain-text document, then the structured inventory the screen renders.
    const PAIRS: [&str; 2] = ["NOTICES_ROUTE", "NOTICES_JSON_ROUTE"];

    let mut findings = Vec::new();
    for name in PAIRS {
        let mut routes = Vec::new();
        for relative in [SERVER, SPA] {
            let path = root.join(relative);
            let Ok(text) = std::fs::read_to_string(&path) else {
                anyhow::bail!("repo-lint: cannot read {}", path.display());
            };
            let Some((line, value)) = const_str(&text, name) else {
                anyhow::bail!(
                    "repo-lint: {} declares no `const {name}: &str = \"…\"`; the notices \
                     link and the route that serves it are held together by nothing else",
                    path.display()
                );
            };
            routes.push((path, line, value));
        }

        let [
            (server_path, server_line, server_route),
            (spa_path, spa_line, spa_route),
        ] = routes.as_slice()
        else {
            unreachable!("two files were read")
        };

        if server_route == spa_route {
            continue;
        }
        findings.push(Finding {
            rule: RULE,
            file: spa_path.clone(),
            line: *spa_line,
            detail: format!(
                "the SPA reaches `{spa_route}` but the server publishes `{server_route}` \
                 ({SERVER}:{server_line}) — the request resolves to the app shell with a 200 \
                 rather than failing"
            ),
        });
        findings.push(Finding {
            rule: RULE,
            file: server_path.clone(),
            line: *server_line,
            detail: format!("the other half of this disagreement is {SPA}:{spa_line}"),
        });
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo_lint::tempdir;

    /// Both directions of the licence-list rule. The dangerous one is the second: a licence the
    /// notices generator accepts and `deny.toml` forbids publishes terms for something that
    /// must never be in the graph, and nothing else in the repository would say so.
    #[test]
    fn the_licence_lists_are_compared_in_both_directions() {
        let root = tempdir("licences");
        let write = |name: &str, body: &str| std::fs::write(root.join(name), body).unwrap();

        write(
            "deny.toml",
            "[licenses]\nallow = [\n    \"MIT\",\n    \"MPL-2.0\",\n]\n",
        );
        write(
            "about.toml",
            "accepted = [\n    \"MIT\",\n    \"MPL-2.0\",\n]\n",
        );
        assert!(
            notices_accept_every_allowed_licence(&root)
                .unwrap()
                .is_empty(),
            "equal lists are the passing case"
        );

        // Admitted into the graph, unknown to the generator: `xtask notices` dies on the first
        // crate that uses it.
        write("about.toml", "accepted = [\n    \"MIT\",\n]\n");
        let missing = notices_accept_every_allowed_licence(&root).unwrap();
        assert_eq!(missing.len(), 1);
        assert!(
            missing[0].detail.contains("MPL-2.0"),
            "{}",
            missing[0].detail
        );

        // Accepted by the generator, forbidden in the graph.
        write(
            "about.toml",
            "accepted = [\n    \"MIT\",\n    \"MPL-2.0\",\n    \"GPL-3.0\",\n]\n",
        );
        let extra = notices_accept_every_allowed_licence(&root).unwrap();
        assert_eq!(extra.len(), 1);
        assert!(extra[0].detail.contains("GPL-3.0"), "{}", extra[0].detail);

        std::fs::remove_dir_all(&root).ok();
    }

    /// The `const NOTICES_ROUTE` parse, including the two shapes that must not be mistaken for
    /// the declaration.
    #[test]
    fn the_notices_route_is_read_off_its_const() {
        let source = "//! const NOTICES_ROUTE: &str = \"/decoy\";\n\
             use axum::Router;\n\
             \n\
             const NOTICES_ROUTE: &str = \"/third-party-notices\";\n";
        assert_eq!(
            const_str(source, "NOTICES_ROUTE"),
            Some((4, "/third-party-notices".to_owned()))
        );
        assert_eq!(
            const_str("let NOTICES_ROUTE = \"/x\";\n", "NOTICES_ROUTE"),
            None
        );
        assert_eq!(
            const_str("const NOTICES: &str = \"x\";\n", "NOTICES_ROUTE"),
            None
        );
        // The SPA's copy is `pub(crate)` so the footer can link it too. Visibility is not part
        // of the declaration this rule is about, and reading it as one turned the whole gate
        // into a hard error the moment the constant gained a reader.
        assert_eq!(
            const_str(
                "pub(crate) const NOTICES_ROUTE: &str = \"/n\";\n",
                "NOTICES_ROUTE"
            ),
            Some((1, "/n".to_owned()))
        );
        assert_eq!(
            const_str("pub const NOTICES_ROUTE: &str = \"/n\";\n", "NOTICES_ROUTE"),
            Some((1, "/n".to_owned()))
        );
    }

    /// Proves the URL rule fires, for both halves. It has to be proved rather than observed
    /// green, because the failure it guards against is itself invisible: the server answers
    /// every unmatched path with the app shell, so a stale link returns 200 and a page.
    #[test]
    fn a_stale_notices_link_is_a_violation() {
        let root = tempdir("notices-url");
        let server = root.join("services/frontend/src");
        let spa = root.join("web/frontend/src/views");
        std::fs::create_dir_all(&server).unwrap();
        std::fs::create_dir_all(&spa).unwrap();
        let write = |dir: &Path, name: &str, route: &str, json: &str| {
            std::fs::write(
                dir.join(name),
                format!(
                    "const NOTICES_ROUTE: &str = \"{route}\";\n\
                     const NOTICES_JSON_ROUTE: &str = \"{json}\";\n"
                ),
            )
            .unwrap();
        };

        let (text, json) = ("/third-party-notices", "/third-party-notices.json");
        write(&server, "main.rs", text, json);
        write(&spa, "licenses.rs", text, json);
        assert!(
            the_notices_url_is_the_one_the_server_publishes(&root)
                .unwrap()
                .is_empty()
        );

        write(&spa, "licenses.rs", "/licenses", json);
        let findings = the_notices_url_is_the_one_the_server_publishes(&root).unwrap();
        // Both halves are reported: either file could be the one that moved.
        assert_eq!(findings.len(), 2);
        assert!(findings[0].detail.contains("/licenses"));
        assert!(findings[0].detail.contains("/third-party-notices"));

        // The inventory URL is held to the same rule, and on its own: a link that still resolves
        // while the document behind it does not is the case that reports a corrupt inventory.
        write(&spa, "licenses.rs", text, "/inventory.json");
        let findings = the_notices_url_is_the_one_the_server_publishes(&root).unwrap();
        assert_eq!(findings.len(), 2);
        assert!(findings[0].detail.contains("/inventory.json"));

        std::fs::remove_dir_all(&root).ok();
    }
}
