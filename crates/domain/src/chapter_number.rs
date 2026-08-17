//! The chapter number's storage form: a fixed-width integer scaled by [`MILLI_SCALE`], and the
//! range it has to be inside to be storable at all.
//!
//! Distinct from [`crate::chapter_outliers`], which judges a number *implausible* by comparing it
//! against the rest of its listing. This is the harder, dumber question that has to be answered
//! first: can the column hold it?
//!
//! # Why an integer and not `numeric`
//!
//! `chapters.number` was `numeric(10,4)` until migration 0055. The type was not the problem; the
//! problem was that **`floor(number)` is not derivable from an index on `number`**, and the unread
//! predicate every reading surface runs is expressed in terms of `floor`. Migration 0026 built a
//! second index on `(series_source_id, (floor(number)))` to make it reachable and documents what
//! it cost when it was not — 2.9 s on the Home stats query — and 0047 widened that index again.
//!
//! Scaled to an integer, `floor(number) > w` is `number_milli >= (w + 1) * 10000`: a plain range
//! on the second key column of the `(series_source_id, number_milli)` index. The `floor` index has
//! nothing left to do, and with it three more indexes collapse into one. See
//! `docs/CHAPTER_STORAGE.md`.

/// Fixed-point scale: a chapter number is stored as `round(number * 10_000)`.
///
/// Four decimal places, the same precision the `numeric(10,4)` column carried, which is what part
/// releases (`152.1`, `152.65`) actually need.
pub const MILLI_SCALE: f64 = 10_000.0;

/// Largest storable chapter number.
///
/// Set by the storage type, not by editorial judgement: `MAX_CHAPTER_NUMBER * MILLI_SCALE` must
/// fit in the `int` column, so anything above `i32::MAX / 10_000` ≈ 214 748 is unrepresentable.
/// Rounded down to a legible number, it leaves roughly fifty times the length of the longest
/// series anyone has published — `Martial Peak` runs to about 3 900.
///
/// A value past it is not "a big chapter number", it is a statement Postgres refuses, and that
/// error aborts the transaction it is in. For ingest that is the whole per-source transaction: one
/// `chapter-20250817` slug that the outlier guard did not have the sample size to reject takes
/// every other chapter of that source down with it, on every rescan, permanently.
///
/// It is a **representability** bound and nothing more. Plenty of junk sits below it — a
/// `chapter-180302` date slug is 180 302 and perfectly storable — and catching that is
/// [`crate::chapter_outliers`]'s job, judged against the listing's own rhythm. Do not read this
/// constant as a plausibility check.
pub const MAX_CHAPTER_NUMBER: f64 = 200_000.0;

/// Whether `number` can be stored in a chapter-number column.
///
/// Negative numbers are refused rather than clamped: there is no reading order in which a
/// chapter is before the beginning, so a negative is a parse that went wrong, not a chapter.
/// Zero is allowed — prologues and chapter 0 are real releases.
///
/// `NaN` and the infinities are refused here as well as by the parsers, because this is the
/// predicate the writers are allowed to rely on.
#[must_use]
pub fn is_storable(number: f64) -> bool {
    number.is_finite() && (0.0..=MAX_CHAPTER_NUMBER).contains(&number)
}

/// The stored form of `number`, or `None` when it is not [`is_storable`].
///
/// Rounds to the nearest representable value rather than truncating. The rounding is not a
/// tolerance to be relied on: two listing entries whose numbers differ only past the fourth
/// decimal collapse to the same row, which is exactly what the old `numeric(10,4)` cast did and
/// what `upsert_chapters`' `DISTINCT ON` is written to survive.
#[must_use]
pub fn to_milli(number: f64) -> Option<i32> {
    if !is_storable(number) {
        return None;
    }
    // Bounded by `is_storable` above, so the cast cannot saturate or wrap.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "is_storable bounds the value to 0..=200_000, so *10_000 fits i32"
    )]
    Some((number * MILLI_SCALE).round() as i32)
}

/// The chapter number a stored value represents.
#[must_use]
pub fn from_milli(milli: i32) -> f64 {
    f64::from(milli) / MILLI_SCALE
}

/// The whole chapter a stored value belongs to — `floor(number)`, in the stored domain.
///
/// Integer division, which is only equal to `floor` because the column is constrained
/// non-negative (`chapters_number_milli_range`). The SQL side spells this `number_milli / 10000`
/// for the same reason.
#[must_use]
pub const fn whole_of_milli(milli: i32) -> i32 {
    milli / MILLI_SCALE_INT
}

/// [`MILLI_SCALE`] as the integer the SQL and the whole-chapter division use.
///
/// Both spellings exist because the conversions are floating-point and the divisions are not;
/// `the_two_scales_are_one_number` pins them together.
pub const MILLI_SCALE_INT: i32 = 10_000;

/// A whole-chapter index as `i64`, clamped into the storable range.
///
/// Clamping rather than erroring because the callers are query *bounds*: a nonsensical input
/// should return no rows, not fail the request. `NaN` clamps to zero, which is the empty-range
/// end of the scale.
fn clamp_whole(value: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to 0..=MAX_CHAPTER_NUMBER, which is far inside i64"
    )]
    let clamped = value.clamp(0.0, MAX_CHAPTER_NUMBER) as i64;
    clamped
}

/// `number` rounded **up** to a whole chapter, clamped into the storable range.
#[must_use]
pub fn whole_ceil(number: f64) -> i64 {
    clamp_whole(number.ceil())
}

/// `number` rounded **down** to a whole chapter, clamped into the storable range.
#[must_use]
pub fn whole_floor(number: f64) -> i64 {
    clamp_whole(number.floor())
}

/// The stored value a whole chapter begins at.
///
/// `i64` on purpose, and every SQL comparison against it is a `bigint` for the same reason:
/// `(200_000 + 1) * 10_000` is larger than `i32::MAX`, so the one-past-the-end bound the unread
/// predicate needs is not representable in the column's own type.
#[must_use]
pub const fn milli_of_whole(whole: i64) -> i64 {
    whole * MILLI_SCALE_INT as i64
}

/// The smallest stored value that is not below `number`.
///
/// `number_milli < milli_ceil(n)` is exactly `chapter < n`, for an `n` that is representable at
/// scale 4 and for one that is not.
#[must_use]
pub fn milli_ceil(number: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to 0..=i32::MAX before the cast"
    )]
    let clamped = (number * MILLI_SCALE)
        .ceil()
        .clamp(0.0, f64::from(i32::MAX)) as i64;
    clamped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug: a source whose listing was too short for the outlier guard to judge (below
    /// `OutlierPolicy::min_sample`) served a date-shaped slug, parsed correctly as a huge number.
    /// That overflows the column, so the `INSERT` failed, and because ingest is one transaction
    /// per source, *every* chapter of that series was rolled back and the failure repeated on
    /// every rescan.
    ///
    /// The second assertion is the honest limit of this guard, and is here so nobody mistakes it
    /// for a plausibility check: a six-digit date slug is **inside** the storable range. Only
    /// `chapter_outliers` rejects that, and only when the listing is long enough to judge.
    #[test]
    fn a_date_shaped_slug_is_not_storable() {
        assert!(!is_storable(20_250_817.0));
        assert!(is_storable(180_302.0), "junk, but representable");
        assert!(is_storable(1050.5));
    }

    #[test]
    fn the_bounds_are_the_columns_bounds() {
        assert!(is_storable(0.0));
        assert!(is_storable(MAX_CHAPTER_NUMBER));
        assert!(!is_storable(MAX_CHAPTER_NUMBER + 1.0));
        assert!(!is_storable(-1.0));
    }

    #[test]
    fn non_finite_is_never_storable() {
        assert!(!is_storable(f64::NAN));
        assert!(!is_storable(f64::INFINITY));
        assert!(!is_storable(f64::NEG_INFINITY));
    }

    /// The scaled form must never overflow the `int` column, which is the entire reason
    /// [`MAX_CHAPTER_NUMBER`] is where it is.
    #[test]
    fn the_largest_storable_number_still_fits_the_column() {
        let milli = to_milli(MAX_CHAPTER_NUMBER).expect("storable");
        assert_eq!(milli, 2_000_000_000);
        assert!(milli < i32::MAX);
    }

    /// Why the unread predicate's lower bound is computed as `bigint` in SQL and `i64` here.
    ///
    /// Not because an in-range chapter needs it — `(200_000 + 1) * 10_000` fits `i32` with room
    /// to spare. Because the bound is derived from `read_progress.last_read_whole_number`, which
    /// is still `numeric(10,4)` and was never range-checked before this ceiling existed. A row
    /// holding a date-shaped value from that era produces a bound two orders of magnitude past
    /// `i32::MAX`, and an overflow there is an error on a read path, not a wrong answer.
    #[test]
    fn a_legacy_progress_value_forces_the_bound_into_bigint() {
        assert!(
            milli_of_whole(whole_ceil(MAX_CHAPTER_NUMBER) + 1) < i64::from(i32::MAX),
            "an in-range bound fits i32; bigint is not for this case"
        );
        // What a `read_progress` row written before the ceiling can still hold.
        let legacy = milli_of_whole(20_250_817 + 1);
        assert!(legacy > i64::from(i32::MAX));
        // `whole_ceil`/`whole_floor` clamp, so a bound *derived through them* stays representable
        // even for that row — which is what keeps the query an error-free empty result.
        assert!(milli_of_whole(whole_floor(20_250_817.0) + 1) < i64::from(i32::MAX));
    }

    #[test]
    fn round_trips_the_numbers_the_sites_actually_publish() {
        for n in [0.0, 1.0, 10.5, 152.1, 152.65, 1050.0, 3900.25] {
            let milli = to_milli(n).expect("storable");
            assert!(
                (from_milli(milli) - n).abs() < f64::EPSILON,
                "{n} did not round-trip"
            );
        }
    }

    /// The float scale and the integer scale are two spellings of one number, and the SQL
    /// hard-codes a third (`10000`). A divergence would put the read predicates and the write path
    /// on different scales, which is the kind of wrong that looks right in every unit test.
    #[test]
    fn the_two_scales_are_one_number() {
        assert!((MILLI_SCALE - f64::from(MILLI_SCALE_INT)).abs() < f64::EPSILON);
    }

    #[test]
    fn the_whole_chapter_of_a_part_release_is_its_floor() {
        assert_eq!(whole_of_milli(to_milli(152.65).expect("storable")), 152);
        assert_eq!(whole_of_milli(to_milli(152.0).expect("storable")), 152);
        assert_eq!(whole_of_milli(to_milli(0.5).expect("storable")), 0);
    }
}
