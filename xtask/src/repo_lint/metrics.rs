//! The metrics catalogue rule: every emitted series has a row, and every row is emitted.

use std::path::Path;

use super::Finding;
use super::text::{is_comment, rust_sources, walk};

/// The macros that create a series. `describe_*` is deliberately absent: it does not create
/// one, and matching it would make the catalogue's own describes look like call sites.
const RECORDING_MACROS: [&str; 3] = [
    "metrics::counter!(",
    "metrics::gauge!(",
    "metrics::histogram!(",
];

/// Path to the catalogue, which is the rule's reference set.
const CATALOGUE_SRC: &str = "crates/service/src/metrics.rs";

/// Every metric emitted anywhere in the workspace must have a row in
/// `tankovault_service::metrics::CATALOGUE`, and every row must be emitted.
///
/// Nothing else can catch this. A metric name is a string: a typo compiles, a new counter with
/// no row compiles and exposes a series with no `# HELP`, `# TYPE` or unit, and a row for a
/// metric nobody emits compiles and reads as coverage that does not exist — which is the exact
/// defect `docs/OBSERVABILITY.md` carried by hand until this rule replaced the hand-checking.
///
/// `crates/fetch` and `crates/solver` sit below `tankovault-service` and cannot import the
/// name constants, so their names are literals; this is what holds them to the list.
pub(super) fn every_metric_is_described(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let source = std::fs::read_to_string(root.join(CATALOGUE_SRC))?;
    let constants = metric_constants(&source);
    let described = catalogue_names(&source, &constants);

    let mut findings = Vec::new();
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();

    for file in rust_sources(root, &["crates", "services"]) {
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (line, name) in recorded_metrics(&src, &constants) {
            match name {
                Some(name) if described.contains(&name) => {
                    emitted.insert(name);
                }
                Some(name) => findings.push(Finding {
                    rule: "metrics-catalogue",
                    file: file.clone(),
                    line,
                    detail: format!(
                        "`{name}` is recorded here but has no row in CATALOGUE ({CATALOGUE_SRC}), \
                         so it is exposed with no HELP, TYPE or unit — and, if it is a histogram, \
                         with the exporter's summary quantiles instead of buckets"
                    ),
                }),
                None => findings.push(Finding {
                    rule: "metrics-catalogue",
                    file: file.clone(),
                    line,
                    detail: "the metric name here is not a literal or a `names::*` constant, so \
                             it cannot be held to the catalogue. Name it with a constant"
                        .to_owned(),
                }),
            }
        }
    }

    for name in &described {
        if !emitted.contains(name) {
            findings.push(Finding {
                rule: "metrics-catalogue",
                file: root.join(CATALOGUE_SRC),
                line: 0,
                detail: format!(
                    "`{name}` has a CATALOGUE row but nothing records it. A documented metric \
                     nobody emits reads as coverage that does not exist — drop the row, or wire \
                     the call site it promises"
                ),
            });
        }
    }
    findings.sort_by(|a, b| (&a.file, a.line, &a.detail).cmp(&(&b.file, b.line, &b.detail)));
    Ok(findings)
}

/// `pub const IDENT: &str = "value";` from the `names` module, as IDENT → value.
fn metric_constants(source: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("pub const ") else {
            continue;
        };
        let Some((ident, value)) = rest.split_once(": &str = ") else {
            continue;
        };
        if let Some(value) = value
            .trim()
            .strip_suffix("\";")
            .and_then(|v| v.strip_prefix('"'))
        {
            out.insert(ident.trim().to_owned(), value.to_owned());
        }
    }
    out
}

/// The metric names the catalogue declares, resolved through [`metric_constants`].
fn catalogue_names(
    source: &str,
    constants: &std::collections::HashMap<String, String>,
) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for line in source.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("name: names::") else {
            continue;
        };
        if let Some(value) = constants.get(rest.trim_end_matches(',')) {
            out.insert(value.clone());
        }
    }
    out
}

/// Every recording macro in `src`, as (1-based line, resolved name).
///
/// `None` for a name that is neither a string literal nor a `…::IDENT` path, which the rule
/// reports rather than silently passing — an unresolvable name is a hole in the check.
fn recorded_metrics(
    src: &str,
    constants: &std::collections::HashMap<String, String>,
) -> Vec<(usize, Option<String>)> {
    let mut out = Vec::new();
    for macro_call in RECORDING_MACROS {
        let mut from = 0;
        while let Some(at) = src[from..].find(macro_call) {
            let open = from + at + macro_call.len();
            from = open;
            // Comment lines are skipped for the same reason every other rule skips them: the
            // prose documenting a metric must not read as an emission of it.
            let line_start = src[..open].rfind('\n').map_or(0, |i| i + 1);
            if is_comment(&src[line_start..open]) {
                continue;
            }
            let line = src[..open].matches('\n').count() + 1;
            let arg: String = src[open..]
                .chars()
                .take_while(|c| *c != ',' && *c != ')')
                .collect();
            let arg = arg.trim();
            let resolved =
                if let Some(literal) = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
                    Some(literal.to_owned())
                } else {
                    constants
                        .get(arg.rsplit("::").next().unwrap_or(arg).trim())
                        .cloned()
                };
            out.push((line, resolved));
        }
    }
    out
}

/// Every long-running service must install the recorder, serve the scrape on its own port and
/// mount the ops probes.
///
/// This was convention, held only by copy-paste: a new service that omits
/// `spawn_metrics_server` compiles, passes every other gate, reports healthy, and is silently
/// unscrapable. `bootstrap` is exempt — it is a one-shot CLI that exits.
pub(super) fn every_service_serves_metrics(root: &Path) -> anyhow::Result<Vec<Finding>> {
    const REQUIRED: [(&str, &str); 3] = [
        (
            "MetricsRegistry::install",
            "no recorder is installed, so every measurement in the process is dropped",
        ),
        (
            "spawn_metrics_server",
            "the scrape is never served on the isolated metrics port, so Prometheus reports \
             the target down while the service looks healthy",
        ),
        (
            "ops_router",
            "`/health` and `/ready` are unmounted, so an orchestrator cannot probe it",
        ),
    ];

    let mut findings = Vec::new();
    for entry in std::fs::read_dir(root.join("services"))? {
        let dir = entry?.path();
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if !dir.is_dir() || name == "bootstrap" || name == "test-support" {
            continue;
        }
        let sources: String = walk(&dir, &["rs"], &["target"])
            .iter()
            .filter_map(|f| std::fs::read_to_string(f).ok())
            .collect();
        for (needle, consequence) in REQUIRED {
            if !sources.contains(needle) {
                findings.push(Finding {
                    rule: "metrics-wiring",
                    file: dir.join("src"),
                    line: 0,
                    detail: format!("`{name}` never calls `{needle}`: {consequence}"),
                });
            }
        }
    }
    Ok(findings)
}
