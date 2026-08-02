//! Rejection of implausible chapter numbers in a scraped listing.
//!
//! Aggregator sites publish stray entries that are not releases: a slug carrying a date
//! (`chapter-180302`), a year (`chapter-2025`), a number lifted from the series title
//! (`Demon-Lord-2099` → `chapter-2099`), or an uploader's typo. The adapter parses them
//! correctly — the source really does say that — so the listing has to be judged as a whole.

/// How aggressively a listing's trailing entries are judged implausible.
///
/// Every threshold is expressed relative to the listing's own rhythm rather than as an absolute
/// chapter number: real series run from single chapters to `Martial Peak`'s ~3,900, so any fixed
/// ceiling is wrong for one end of that range or the other.
#[derive(Debug, Clone, Copy)]
pub struct OutlierPolicy {
    /// Smallest listing worth judging. Below this there is no rhythm to compare against.
    pub min_sample: usize,
    /// Chapters that must survive. Stops a listing being judged down to nothing.
    pub min_body: usize,
    /// Absolute floor on what counts as a suspicious jump, in chapter numbers.
    pub min_gap: f64,
    /// A jump is suspicious past this multiple of the listing's typical spacing.
    pub gap_factor: f64,
    /// Numbers per entry, as a multiple of typical spacing, past which a run is judged noise
    /// rather than a continuation.
    pub sparse_factor: f64,
    /// Ceiling on the fraction of a listing that may be rejected.
    pub max_rejected_fraction: f64,
}

impl Default for OutlierPolicy {
    fn default() -> Self {
        Self {
            // Six entries is where a median gap starts to mean something. Newly-added series sit
            // below it and are trusted whole; they get judged once the next scan grows them.
            min_sample: 6,
            min_body: 5,
            // Series do skip a few numbers (a pulled chapter, a merged double release), so a jump
            // has to clear more than that before it is evidence of anything.
            min_gap: 10.0,
            gap_factor: 20.0,
            // Measured against the full catalogue: at 20x, dense trailing runs — a renumbered
            // arc restarting at 505 after 359 — stay, while scattered entries go. Lowering it
            // toward 10x starts taking real renumberings with it.
            sparse_factor: 20.0,
            // Junk is always a small minority. A quarter of a listing being implausible means
            // the numbering was misread wholesale, which is an adapter bug to fix rather than
            // data to silently drop.
            max_rejected_fraction: 0.25,
        }
    }
}

/// A chapter number paired with the caller's index into its own listing.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Entry {
    index: usize,
    number: f64,
}

/// Indices of the entries in `numbers` judged implausible, ascending.
///
/// `numbers[i]` is the chapter number of the caller's `i`th entry, in any order. An empty result
/// means the whole listing is plausible.
///
/// # Panics
/// Never. Non-finite numbers are dropped from consideration rather than compared.
#[must_use]
pub fn implausible_indices(numbers: &[f64], policy: &OutlierPolicy) -> Vec<usize> {
    let mut sorted: Vec<Entry> = numbers
        .iter()
        .enumerate()
        // `NaN` has no position in a sorted listing and would poison every comparison below.
        // `parse_number` already rejects it upstream; this keeps the function total anyway.
        .filter(|(_, n)| n.is_finite())
        .map(|(index, &number)| Entry { index, number })
        .collect();
    sorted.sort_by(|a, b| a.number.total_cmp(&b.number));

    let Some(cut) = plausible_len(&sorted, policy) else {
        return Vec::new();
    };

    let mut rejected: Vec<usize> = sorted[cut..].iter().map(|e| e.index).collect();
    rejected.sort_unstable();
    rejected
}

/// The length of the plausible prefix of `sorted`, or `None` if all of it is plausible.
///
/// Peels from the top down, one suspicious jump at a time, rather than picking the lowest
/// suspicious jump and judging everything above it in one go. The difference is load-bearing:
/// with a single extreme stray in the listing (`Martial Peak`'s `chapter-34922`), the span from
/// any lower cut point up to that stray is enormous, so a one-shot judgement calls every entry
/// above the lowest jump sparse and takes real chapters with it. Peeling removes the stray
/// first, then re-judges what is left against its own span.
fn plausible_len(sorted: &[Entry], policy: &OutlierPolicy) -> Option<usize> {
    if sorted.len() < policy.min_sample {
        return None;
    }
    let typical = typical_spacing(sorted)?;
    let suspicious_gap = policy.min_gap.max(policy.gap_factor * typical);
    let sparse_spacing = policy.sparse_factor * typical;
    let budget = rejection_budget(sorted.len(), policy.max_rejected_fraction);

    let mut end = sorted.len();
    let mut rejected = 0usize;
    while let Some(start) = topmost_jump(&sorted[..end], suspicious_gap) {
        if start < policy.min_body {
            break;
        }
        let run = end - start;
        if rejected + run > budget {
            break;
        }
        // Span measured from the last retained chapter, so a single detached entry — the common
        // case — has a span at all. `run` is non-zero by construction of `topmost_jump`.
        let span = sorted[end - 1].number - sorted[start - 1].number;
        #[expect(
            clippy::cast_precision_loss,
            reason = "a run longer than f64's exact-integer range exceeds any real listing"
        )]
        let spacing = span / run as f64;
        // A dense run is a continuation the site renumbered, not noise. Only a run spread far
        // more thinly than the series' own rhythm is rejected.
        if spacing <= sparse_spacing {
            break;
        }
        rejected += run;
        end = start;
    }

    (rejected > 0).then_some(end)
}

/// How many entries one scan may reject from a listing of `len`.
///
/// At least one, so the single-stray case — by far the most common — is never budgeted out of
/// existence on a short listing.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a listing long enough to lose f64 precision exceeds any real catalogue, and the \
              product of a length and a fraction in [0,1] is non-negative and no larger than it"
)]
fn rejection_budget(len: usize, fraction: f64) -> usize {
    ((len as f64 * fraction) as usize).max(1)
}

/// Index of the highest entry that sits more than `suspicious_gap` above its predecessor.
fn topmost_jump(sorted: &[Entry], suspicious_gap: f64) -> Option<usize> {
    (1..sorted.len())
        .rev()
        .find(|&i| sorted[i].number - sorted[i - 1].number > suspicious_gap)
}

/// The listing's typical spacing: the median gap between consecutive numbers, floored at 1.
///
/// Median, not mean, because the strays this module exists to find are exactly the values that
/// would drag a mean. Floored at 1 because a listing numbered in fractions (`0.01, 0.02, …`, seen
/// on sources that index pages rather than chapters) otherwise reports a spacing so fine that
/// every later whole chapter looks like a jump.
fn typical_spacing(sorted: &[Entry]) -> Option<f64> {
    let mut gaps: Vec<f64> = sorted
        .windows(2)
        .map(|w| w[1].number - w[0].number)
        .filter(|g| *g > 0.0)
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_by(f64::total_cmp);
    let mid = gaps.len() / 2;
    let median = if gaps.len().is_multiple_of(2) {
        f64::midpoint(gaps[mid - 1], gaps[mid])
    } else {
        gaps[mid]
    };
    Some(median.max(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected(numbers: &[f64]) -> Vec<f64> {
        implausible_indices(numbers, &OutlierPolicy::default())
            .into_iter()
            .map(|i| numbers[i])
            .collect()
    }

    fn run(from: i32, to: i32) -> Vec<f64> {
        (from..=to).map(f64::from).collect()
    }

    /// The two reports this module was written for: a lone entry far above a contiguous run.
    /// `chapter-1000` on a 37-chapter series and `chapter-94` on a 58-chapter one are both the
    /// source's own slugs, so nothing upstream of ingest can tell they are not releases.
    #[test]
    fn a_lone_entry_far_above_the_run_is_rejected() {
        let mut listing = run(0, 36);
        listing.push(1000.0);
        assert_eq!(rejected(&listing), vec![1000.0]);

        let mut listing = run(1, 58);
        listing.push(94.0);
        assert_eq!(rejected(&listing), vec![94.0]);
    }

    #[test]
    fn a_clean_listing_is_left_alone() {
        assert!(rejected(&run(1, 200)).is_empty());
    }

    /// Ordinary numbering holes — a pulled chapter, a double release — must survive. The whole
    /// value of the rule is that it fires on strays and not on these.
    #[test]
    fn small_holes_are_not_jumps() {
        let listing = [1.0, 2.0, 3.0, 7.0, 8.0, 9.0, 15.0, 16.0, 17.0, 18.0];
        assert!(rejected(&listing).is_empty());
    }

    /// A site that renumbers an arc leaves a dense run above a large jump. It is a real
    /// continuation and is kept — density, not jump size, is what separates it from noise.
    #[test]
    fn a_dense_run_above_a_jump_is_a_renumbering_not_noise() {
        let mut listing = run(1, 359);
        listing.extend(run(505, 519));
        assert!(rejected(&listing).is_empty());
    }

    /// Pins the bug that top-down peeling exists to prevent. `Martial Peak` lists 3,862
    /// contiguous chapters, then 4855, 4856, 21622 and 34922. Judged in one pass from the
    /// lowest suspicious jump, the span up to 34922 makes the entire tail look sparse and the
    /// legitimate top of the run is rejected with it; peeled from the top, each segment is
    /// measured against its own span.
    #[test]
    fn peeling_from_the_top_does_not_widen_a_cut_below_it() {
        let mut listing = run(1, 3862);
        listing.extend([4855.0, 4856.0, 21622.0, 34922.0]);
        assert_eq!(rejected(&listing), vec![4855.0, 4856.0, 21622.0, 34922.0]);
    }

    /// Strays arrive in clusters as well as alone — date-derived slugs on one source ran
    /// 25025, 25028, 26028, 27028, 28028 above a 61-chapter series. The 3-number gap inside
    /// the cluster is not itself suspicious, so peeling has to keep descending past it.
    #[test]
    fn a_cluster_of_strays_is_rejected_whole() {
        let mut listing = run(1, 61);
        listing.extend([25025.0, 25028.0, 26028.0, 27028.0, 28028.0]);
        assert_eq!(
            rejected(&listing),
            vec![25025.0, 25028.0, 26028.0, 27028.0, 28028.0]
        );
    }

    /// Year-derived slugs cluster tightly, so the cluster's *internal* spacing looks like a
    /// real run. It is the jump beneath it, and the span from the last real chapter, that give
    /// it away.
    #[test]
    fn year_numbered_entries_are_rejected() {
        let mut listing = run(1, 43);
        listing.extend([2022.0, 2022.1, 2024.0, 2024.1, 2025.0, 2025.1]);
        assert_eq!(
            rejected(&listing),
            vec![2022.0, 2022.1, 2024.0, 2024.1, 2025.0, 2025.1]
        );
    }

    /// Sources that number pages rather than chapters produce a body spaced in hundredths.
    /// Without the floor under typical spacing, the suspicious-jump threshold collapses to
    /// fractions of a chapter and every ordinary chapter above the body is rejected.
    #[test]
    fn a_fractionally_numbered_body_does_not_condemn_whole_chapters() {
        let mut listing: Vec<f64> = (0..80).map(|i| f64::from(i) * 0.01).collect();
        listing.extend(run(53, 77));
        assert!(rejected(&listing).is_empty());
    }

    /// Too small to judge: with a handful of entries there is no rhythm to compare against, and
    /// guessing costs a real chapter. They are trusted until the next scan grows the listing.
    #[test]
    fn short_listings_are_trusted() {
        assert!(rejected(&[1.0, 2.0, 900.0]).is_empty());
    }

    /// The budget is a backstop against a wholesale misreading of a source's numbering: however
    /// implausible a listing looks, one scan may never reject more than
    /// [`OutlierPolicy::max_rejected_fraction`] of it. A source that trips this is an adapter to
    /// fix, not a catalogue to quietly empty.
    #[test]
    fn no_scan_rejects_more_than_the_budget() {
        let policy = OutlierPolicy::default();
        let mut listing = run(1, 10);
        listing.extend([500.0, 1000.0, 1500.0, 2000.0, 2500.0, 3000.0]);

        let count = implausible_indices(&listing, &policy).len();
        assert!(count > 0, "strays this far out must still be caught");
        assert!(
            count <= rejection_budget(listing.len(), policy.max_rejected_fraction),
            "rejected {count} of {}, over budget",
            listing.len()
        );
    }

    /// Order is the caller's, not ours: indices must map back to the input positions.
    #[test]
    fn indices_refer_to_the_input_order() {
        let mut listing = vec![5000.0];
        listing.extend(run(1, 40));
        assert_eq!(
            implausible_indices(&listing, &OutlierPolicy::default()),
            [0]
        );
    }

    #[test]
    fn non_finite_numbers_are_ignored_rather_than_compared() {
        let mut listing = run(1, 40);
        listing.push(f64::NAN);
        listing.push(9000.0);
        assert_eq!(rejected(&listing), vec![9000.0]);
    }

    #[test]
    fn an_empty_listing_is_plausible() {
        assert!(rejected(&[]).is_empty());
    }

    /// Duplicated numbers give zero gaps; they must not be counted as the typical spacing and
    /// drive the threshold to the floor.
    #[test]
    fn repeated_numbers_do_not_define_the_spacing() {
        let mut listing = vec![1.0; 30];
        listing.extend(run(2, 40));
        listing.push(9000.0);
        assert_eq!(rejected(&listing), vec![9000.0]);
    }
}
