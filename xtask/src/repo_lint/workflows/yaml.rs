//! Owns the line-level readers for workflow YAML: jobs, steps, `env` values and expressions.

use crate::repo_lint::text::is_comment;
use std::ops::Range;

/// The first `${{ … }}` in `value`: the bytes it occupies, and its trimmed body.
pub(super) fn expression(value: &str) -> Option<(Range<usize>, &str)> {
    let start = value.find("${{")?;
    let end = value[start..].find("}}")? + start + 2;
    Some((start..end, value[start + 3..end - 2].trim()))
}

/// The first `${VAR}` in `value`: the bytes it occupies, and the name.
///
/// A `${{ … }}` expression opens with the same two characters and is not one of these, so it is
/// skipped instead of ending the search: an unresolved expression would otherwise hide every
/// variable behind it.
pub(super) fn shell_variable(value: &str) -> Option<(Range<usize>, String)> {
    for (start, _) in value.match_indices("${") {
        let rest = &value[start + 2..];
        if rest.starts_with('{') {
            continue;
        }
        let Some((name, _)) = rest.split_once('}') else {
            continue;
        };
        if name
            .chars()
            .all(|character| character.is_ascii_uppercase() || character == '_')
        {
            return Some((start..start + 3 + name.len(), name.to_owned()));
        }
    }
    None
}

/// The value the step with `id` writes for `name` with `echo "<name>=<value>"`.
///
/// Scoped to the one step, because the name alone is ambiguous here: `resolve image names`
/// writes `dockerhub=<image>` and `resolve the manifest list digests` writes
/// `dockerhub=<digest>`, and a digest carries the colon that would otherwise read as a
/// registry port.
pub(super) fn step_output(text: &str, id: &str, name: &str) -> Option<String> {
    let header = format!("id: {id}");
    let prefix = format!("echo \"{name}=");
    text.lines()
        .skip_while(|line| line.trim().trim_start_matches("- ") != header)
        .skip(1)
        .take_while(|line| is_comment(line) || !line.trim().starts_with("- "))
        .filter(|line| !is_comment(line))
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .map(|value| value.trim_end_matches('"').to_owned())
}

/// Each `uses:` of `action`, as its 1-based line number and the non-comment lines of that step.
///
/// A step's continuation is everything indented further than its `-`, which is what bounds the
/// body without reading `with:`: the keys this rule looks for may sit under any of them.
pub(super) fn action_steps<'text>(text: &'text str, action: &str) -> Vec<(usize, Vec<&'text str>)> {
    let mut steps = Vec::new();
    let mut lines = text.lines().enumerate().peekable();
    while let Some((index, line)) = lines.next() {
        if is_comment(line) || !line.contains(action) {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let mut body = Vec::new();
        while let Some((_, next)) = lines.peek() {
            let trimmed = next.trim();
            if !trimmed.is_empty() && next.len() - next.trim_start().len() <= indent {
                break;
            }
            if !is_comment(next) && !trimmed.is_empty() {
                body.push(*next);
            }
            lines.next();
        }
        steps.push((index + 1, body));
    }
    steps
}

/// The name of the top-level job the 1-based `line` falls in.
pub(super) fn job_at(text: &str, line: usize) -> Option<String> {
    text.lines()
        .take(line)
        .filter(|candidate| is_job_header(candidate))
        .map(|candidate| candidate.trim().trim_end_matches(':').to_owned())
        .last()
}

/// Whether the line opens a top-level job: jobs are the only two-space-indented keys in these
/// files, and everything nested inside one is indented further.
pub(super) fn is_job_header(line: &str) -> bool {
    !is_comment(line)
        && line.starts_with("  ")
        && !line.starts_with("   ")
        && line.trim_end().ends_with(':')
}

/// The non-comment, non-empty lines of one top-level job, each with its 1-based line number.
///
/// Jobs are the only two-space-indented keys in these files, so the next one of those ends the
/// job, which is what keeps the rule off `plan`'s deliberately unretried probes.
pub(super) fn job_lines<'text>(text: &'text str, job: &str) -> Vec<(usize, &'text str)> {
    let header = format!("  {job}:");
    let mut inside = false;
    let mut lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim_end() == header {
            inside = true;
            continue;
        }
        if inside
            && line.starts_with("  ")
            && !line.starts_with("   ")
            && line.trim_end().ends_with(':')
        {
            break;
        }
        if inside && !is_comment(line) && !line.trim().is_empty() {
            lines.push((index + 1, line));
        }
    }
    lines
}

/// The value of one key in a workflow's **top-level** `env:` block, and the line it is on.
///
/// Top-level only: an `env:` nested under a job or a step is the override this rule reports, so
/// reading one as the declaration would make the rule report nothing. Column zero is what
/// distinguishes them.
pub(super) fn workflow_env_value(text: &str, key: &str) -> Option<(usize, String)> {
    let mut inside = false;
    for (index, line) in text.lines().enumerate() {
        if is_comment(line) || line.trim().is_empty() {
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            inside = line.trim_end() == "env:";
            continue;
        }
        if inside
            && let Some(value) = line.trim().strip_prefix(key).and_then(|rest| {
                rest.strip_prefix(':')
                    .map(|value| value.trim().trim_matches(['"', '\'']).to_owned())
            })
        {
            return Some((index + 1, value));
        }
    }
    None
}
