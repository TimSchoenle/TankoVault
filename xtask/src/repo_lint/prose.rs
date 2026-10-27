//! Rules from the estate's writing standards that a text scan can check.
//!
//! The doc-comment contract and the prose contract in `TimSchoenle/actions` (`docs/doc-comments/`
//! and `docs/readme/PROSE.md`) each name one mechanical check with a budget. Everything else in
//! them is a review criterion and stays one.

use std::path::{Path, PathBuf};

use super::Finding;
use super::text::walk;

/// Summary openers that restate the item's kind or soften the sentence, from the contract's check.
const BANNED_OPENERS: &[&str] = &[
    "This function",
    "This method",
    "This struct",
    "This class",
    "This enum",
    "This trait",
    "This module",
    "This field",
    "Used to ",
    "Helper for",
    "Helper that",
    "Responsible for",
    "A utility",
    "Note that",
    "In other words",
    "Under the hood",
    "Simply ",
    "Basically ",
];

/// Generated sources, whose comments come from the document they are generated from.
const GENERATED: &[&str] = &["crates/api-client/src/lib.rs"];

/// Documents held to the em-dash budget.
const BUDGETED_DOCUMENTS: &[&str] = &["README.md"];

/// Em dashes a document may use outside tables, quotes, numbered lists and code.
const EM_DASH_BUDGET: usize = 5;

/// **No doc comment opens with a banned phrase.**
///
/// Rustdoc renders a summary next to the item's own name, so `This function` repeats what is
/// already on screen, and a softener introduces a fact that should have been stated directly.
pub(super) fn doc_comments_open_with_the_fact(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    for path in walk(root, &["rs"], &["target", "node_modules", "dist"]) {
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if GENERATED
            .iter()
            .any(|generated| relative == Path::new(generated))
        {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (line, opener) in banned_openers(&text) {
            findings.push(Finding {
                rule: "doc-comment-opener",
                file: relative.clone(),
                line,
                detail: format!("a doc comment line opens with `{opener}`; start with the fact"),
            });
        }
    }
    findings
}

/// The 1-based lines of `source` whose doc comment text opens with a banned phrase.
///
/// Line doc comments (`///`, `//!`) are read line by line. A block doc comment (`/**`, `/*!`) is
/// read from its opening line through its `*/`, with each interior line's leading `*` dropped.
/// Ordinary comments are not doc comments and are skipped.
fn banned_openers(source: &str) -> Vec<(usize, &'static str)> {
    let mut findings = Vec::new();
    let mut in_block_doc = false;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        let body = if in_block_doc {
            in_block_doc = !trimmed.contains("*/");
            Some(trimmed.strip_prefix('*').unwrap_or(trimmed))
        } else if let Some(rest) = block_doc_opening(trimmed) {
            in_block_doc = !rest.contains("*/");
            Some(rest)
        } else {
            ["///", "//!"]
                .iter()
                .find_map(|marker| trimmed.strip_prefix(marker))
                .filter(|rest| !rest.starts_with('/'))
        };
        let Some(body) = body else {
            continue;
        };
        let body = body.trim_start();
        if let Some(opener) = BANNED_OPENERS
            .iter()
            .find(|opener| body.starts_with(**opener))
        {
            findings.push((index + 1, *opener));
        }
    }
    findings
}

/// The text after a block doc comment's opening marker, or `None` when `line` does not open one.
///
/// `/**/` and a run of three or more asterisks open ordinary comments, as in rustc.
fn block_doc_opening(line: &str) -> Option<&str> {
    if let Some(rest) = line.strip_prefix("/*!") {
        return Some(rest);
    }
    let rest = line.strip_prefix("/**")?;
    (!rest.starts_with('*') && !rest.starts_with('/')).then_some(rest)
}

/// **A budgeted document uses at most five em dashes as punctuation.**
///
/// A dash setting off a genuine aside is correct; past five, they are doing a full stop's work.
/// Tables, blockquotes, numbered lists, HTML comments and code are not counted, as in the shell
/// check in PROSE.md. A fence closes only on its own character and at least its own length.
///
/// # Errors
///
/// A budgeted document that cannot be read. It is required, so its absence is a broken checkout.
pub(super) fn documents_keep_the_em_dash_budget(root: &Path) -> anyhow::Result<Vec<Finding>> {
    let mut findings = Vec::new();
    for document in BUDGETED_DOCUMENTS {
        let path = PathBuf::from(document);
        let absolute = root.join(&path);
        let text = std::fs::read_to_string(&absolute)
            .map_err(|e| anyhow::anyhow!("{}: {e}", absolute.display()))?;
        let count = counted_em_dashes(&text);
        if count > EM_DASH_BUDGET {
            findings.push(Finding {
                rule: "em-dash-budget",
                file: path,
                line: 1,
                detail: format!(
                    "{count} em dashes outside tables, quotes, lists and code; the budget is \
                     {EM_DASH_BUDGET}"
                ),
            });
        }
    }
    Ok(findings)
}

/// The em dashes PROSE.md's check counts in `markdown`.
fn counted_em_dashes(markdown: &str) -> usize {
    let mut fence: Option<Fence> = None;
    let mut in_comment = false;
    let mut count = 0;
    for line in markdown.lines() {
        if let Some(open) = fence {
            if open.is_closed_by(line) {
                fence = None;
            }
            continue;
        }
        if let Some(open) = Fence::opened_by(line) {
            fence = Some(open);
            continue;
        }
        if in_comment || line.contains("<!--") {
            in_comment = !line.contains("-->");
            continue;
        }
        if line.starts_with('|') || line.starts_with('>') || is_numbered_item(line) {
            continue;
        }
        count += without_inline_code(line).matches('\u{2014}').count();
    }
    count
}

/// A code fence: its character and how many of it opened the block.
#[derive(Clone, Copy)]
struct Fence {
    marker: char,
    length: usize,
}

impl Fence {
    /// The fence `line` opens, if it opens one: three or more backticks or tildes, indented by
    /// at most three spaces.
    fn opened_by(line: &str) -> Option<Self> {
        let body = Self::unindented(line)?;
        let marker = body.chars().next().filter(|c| *c == '`' || *c == '~')?;
        let length = body.chars().take_while(|c| *c == marker).count();
        (length >= 3).then_some(Self { marker, length })
    }

    /// Whether `line` closes this fence: the same character, at least as many of it, and nothing
    /// after them but whitespace.
    fn is_closed_by(self, line: &str) -> bool {
        let Some(body) = Self::unindented(line) else {
            return false;
        };
        let length = body.chars().take_while(|c| *c == self.marker).count();
        length >= self.length && body[length * self.marker.len_utf8()..].trim().is_empty()
    }

    /// `line` without its indentation, or `None` when four or more spaces make it a code line.
    fn unindented(line: &str) -> Option<&str> {
        let indent = line.len() - line.trim_start_matches(' ').len();
        (indent <= 3).then(|| &line[indent..])
    }
}

/// Whether `line` opens a numbered list item, `1. ` and the like.
fn is_numbered_item(line: &str) -> bool {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && line[digits..].starts_with(". ")
}

/// `line` with every backtick span removed.
fn without_inline_code(line: &str) -> String {
    line.split('`').step_by(2).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_opener_is_found_in_every_doc_marker() {
        let source = "/// This function adds.\n//! Note that it blocks.\n";
        assert_eq!(
            banned_openers(source),
            vec![(1, "This function"), (2, "Note that")]
        );
    }

    #[test]
    fn block_doc_comments_are_checked_and_ordinary_blocks_are_not() {
        let source = concat!(
            "/** This function adds. */\n",
            "/*!\n",
            " * Note that it blocks.\n",
            " */\n",
            "/*\n",
            " * Simply a plain block.\n",
            " */\n",
            "/*** Simply a plain block too. */\n",
        );
        assert_eq!(
            banned_openers(source),
            vec![(1, "This function"), (3, "Note that")]
        );
    }

    #[test]
    fn a_plain_comment_and_a_later_phrase_are_not_openers() {
        let source = "// This function is private reasoning.\n/// Adds. Note that it blocks.\n";
        assert_eq!(banned_openers(source), vec![]);
    }

    #[test]
    fn dashes_in_skipped_regions_are_not_counted() {
        let markdown = "\
One \u{2014} two.
| a \u{2014} b |
> quoted \u{2014} text
1. listed \u{2014} item
`code \u{2014} span` and prose \u{2014} here
<!--
banner \u{2014} text
-->
```text
fenced \u{2014} block
```
";
        assert_eq!(counted_em_dashes(markdown), 2);
    }

    #[test]
    fn a_fence_closes_only_on_its_own_marker_and_length() {
        let markdown = "\
````markdown
```
inner \u{2014} example
```
````
~~~
tilde \u{2014} block
~~~ trailing text does not close it
still \u{2014} code
~~~
after \u{2014} the fences
";
        assert_eq!(counted_em_dashes(markdown), 1);
    }

    #[test]
    fn a_numbered_item_needs_digits_then_a_full_stop_and_a_space() {
        assert!(is_numbered_item("12. twelve"));
        assert!(!is_numbered_item("1.5 is a number"));
        assert!(!is_numbered_item(". no digits"));
    }
}
