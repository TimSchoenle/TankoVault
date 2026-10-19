//! Edition and arc markers: words that make a title a different book from the same name without
//! them. A marker on one side and not the other is the `edition_conflict` veto.

use std::collections::BTreeSet;

/// Token sequences as `normalize_title` produces them, each with the marker it reads as.
///
/// Every entry is a production pair the fuzzy or alias route merged. A spelling variant maps to
/// the same marker so that `4-koma` and `yonkoma` agree.
const EDITION_MARKERS: &[(&[&str], &str)] = &[
    // A side story set in the same world: same characters, different book.
    (&["gaiden"], "gaiden"),
    (&["side", "story"], "side story"),
    (&["after", "story"], "after story"),
    // Bonus chapters and collections published apart from the main run.
    (&["extra"], "extra"),
    (&["anthology"], "anthology"),
    // Four-panel spin-offs.
    (&["4", "koma"], "4koma"),
    (&["4koma"], "4koma"),
    (&["yonkoma"], "4koma"),
    // A single chapter, often the pilot of a later serialisation.
    (&["oneshot"], "oneshot"),
    (&["one", "shot"], "oneshot"),
    (&["pilot"], "pilot"),
    // A recoloured edition is listed as its own series by the providers that carry it.
    (&["colored"], "colored"),
    (&["coloured"], "colored"),
    (&["remake"], "remake"),
    // Sequel and prequel suffixes: `Tokyo Ghoul:re`, `Fate/Zero`.
    (&["re"], "re"),
    (&["zero"], "zero"),
    // Adaptation tie-ins.
    (&["ova"], "ova"),
    (&["movie"], "movie"),
    (&["doujinshi"], "doujinshi"),
    // Subdivisions of a long run, each listed separately.
    (&["part"], "part"),
    (&["arc"], "arc"),
    (&["season"], "season"),
];

/// One marker a title carries.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Marker<'a> {
    /// A word or phrase from [`EDITION_MARKERS`].
    Word(&'static str),
    /// An `-hen` arc, by the word naming it: `Onikakushi-hen` against `Onisarashi-hen`.
    Arc(&'a str),
    /// A single trailing letter: `Railgun S` against `Railgun T`, `Symphogear G`.
    Letter(&'a str),
}

/// The markers `normalized` carries.
fn markers(normalized: &str) -> BTreeSet<Marker<'_>> {
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let mut out = BTreeSet::new();
    for (i, token) in tokens.iter().enumerate() {
        for (pattern, marker) in EDITION_MARKERS {
            if tokens[i..].starts_with(pattern) {
                out.insert(Marker::Word(marker));
            }
        }
        if *token == "hen" {
            out.insert(Marker::Arc(
                i.checked_sub(1).map_or("", |prev| tokens[prev]),
            ));
        }
    }
    if let Some(last) = tokens.last()
        && tokens.len() > 1
        && last.len() == 1
        && last.bytes().all(|b| b.is_ascii_alphabetic())
    {
        out.insert(Marker::Letter(last));
    }
    out
}

/// Whether one title carries an edition or arc marker the other does not.
pub(crate) fn editions_conflict(a: &str, b: &str) -> bool {
    markers(a) != markers(b)
}

#[cfg(test)]
mod tests {
    use super::{editions_conflict, markers};

    #[test]
    fn a_marker_on_one_side_only_conflicts() {
        for (a, b) in [
            ("tokyo ghoul", "tokyo ghoul re"),
            ("solo leveling", "solo leveling colored"),
            ("naruto", "naruto gaiden"),
            ("kanojo okarishimasu", "kanojo okarishimasu 4 koma"),
            ("berserk", "berserk side story"),
        ] {
            assert!(editions_conflict(a, b), "{a:?} vs {b:?}");
            assert!(editions_conflict(b, a), "{b:?} vs {a:?}");
        }
    }

    #[test]
    fn the_same_markers_or_none_agree() {
        for (a, b) in [
            ("tokyo ghoul re", "tokyo ghoul re"),
            ("solo leveling", "na honjaman level up"),
            ("kanojo okarishimasu 4 koma", "kanojo okarishimasu yonkoma"),
            (
                "re zero kara hajimeru isekai seikatsu",
                "re zero starting life",
            ),
        ] {
            assert!(!editions_conflict(a, b), "{a:?} vs {b:?}");
        }
    }

    /// Two arcs of one work differ in the word before `hen`, not in whether it is there.
    #[test]
    fn arcs_compare_by_name() {
        assert!(editions_conflict(
            "higurashi no naku koro ni onikakushi hen",
            "higurashi no naku koro ni onisarashi hen"
        ));
        assert!(editions_conflict(
            "higurashi no naku koro ni",
            "higurashi no naku koro ni onikakushi hen"
        ));
        assert!(!editions_conflict(
            "higurashi onikakushi hen",
            "higurashi no naku koro ni onikakushi hen"
        ));
    }

    /// A single trailing letter is a season, not noise.
    #[test]
    fn a_trailing_letter_is_a_marker() {
        assert!(editions_conflict(
            "toaru kagaku no railgun s",
            "toaru kagaku no railgun t"
        ));
        assert!(editions_conflict(
            "senki zesshou symphogear",
            "senki zesshou symphogear g"
        ));
        // A one-letter title is the title, not a suffix.
        assert!(markers("x").is_empty());
    }

    /// A marker is a whole token: `extras`, `parted`, `rezero` and `kitchen` carry none.
    #[test]
    fn markers_are_whole_tokens() {
        for title in [
            "the extras academy survival guide",
            "parted ways",
            "rezero",
            "kitchen of the hero",
        ] {
            assert!(markers(title).is_empty(), "{title:?}");
        }
    }
}
