//! Owns the one way a user's search term becomes an `ILIKE` pattern.

/// Longest term, in characters, a substring search matches on; the rest is dropped.
///
/// A leading wildcard rules out an index, so every search is a scan whose per-row cost grows with
/// the pattern; no search box needs more than this.
pub(crate) const MAX_SEARCH_CHARS: usize = 200;

/// Wraps `term` as a case-insensitive substring pattern, matched with `ILIKE … ESCAPE '\'`.
///
/// `\`, `%` and `_` are escaped so they match themselves. Unescaped, `_` and `%` are wildcards:
/// an admin search for `a_c%` probes the users table one character at a time, which is email
/// enumeration, and a lone `%` matches every row. A trailing `\` would also be a dangling escape
/// that Postgres rejects.
pub(crate) fn contains_pattern(term: &str) -> String {
    let mut pattern = String::with_capacity(term.len().min(MAX_SEARCH_CHARS * 4) + 2);
    pattern.push('%');
    for ch in term.chars().take(MAX_SEARCH_CHARS) {
        if matches!(ch, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push('%');
    pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug: admin search spliced the raw term between `%`s, so `_` and `%` were wildcards
    /// and a search for `_` matched every user.
    #[test]
    fn wildcards_and_the_escape_character_match_literally() {
        assert_eq!(contains_pattern("alice"), "%alice%");
        assert_eq!(contains_pattern("_"), "%\\_%");
        assert_eq!(contains_pattern("50%"), "%50\\%%");
        assert_eq!(contains_pattern("a\\"), "%a\\\\%");
        assert_eq!(contains_pattern(""), "%%");
    }

    #[test]
    fn the_term_is_capped_in_characters_not_bytes() {
        let long = "é".repeat(MAX_SEARCH_CHARS + 50);
        let pattern = contains_pattern(&long);
        assert_eq!(pattern.chars().count(), MAX_SEARCH_CHARS + 2);
    }
}
