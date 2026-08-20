//! The automatic-merge policy the duplicate sweep applies, and the console surface that moves
//! it without a redeploy.
//!
//! Two layers, resolved here because this is the only service that holds both: the deployment's
//! configured `matching` block is the baseline, and a row in `tunable_overrides` — written from
//! the console — replaces it key by key. Nothing else may resolve them; an API that paired the
//! stored overrides with the *compiled* registry defaults instead would report, and reset to, a
//! policy this deployment does not run.

use tankovault_config::MatchingConfig;
use tankovault_contracts::admin::MergePolicyView;
use tankovault_db::PgPool;
use tankovault_db::repo::tunables::TunableOverrideRow;
use tankovault_domain::{Tunable, UserId};
use tankovault_matcher::{MergeGuards, Thresholds};
use tankovault_service::TunableSet;

/// The thresholds and guards in force: the configured policy with every stored override applied.
///
/// `high`, `low` and `candidate_limit` are absent from the console on purpose — they are read by
/// the worker and by external sync, neither of which watches this table, so a knob here would
/// move a number those services never see.
pub(crate) fn thresholds(config: &MatchingConfig, tunables: &TunableSet) -> Thresholds {
    let base = config.thresholds();
    Thresholds {
        auto_merge: as_f32(
            tunables.resolve(Tunable::MatchingAutoMerge, f64::from(base.auto_merge)),
        ),
        guards: MergeGuards {
            numeric_conflict: tunables.resolve_bool(
                Tunable::MatchingBlockOnNumericConflict,
                base.guards.numeric_conflict,
            ),
            author_conflict: tunables.resolve_bool(
                Tunable::MatchingBlockOnAuthorConflict,
                base.guards.author_conflict,
            ),
            year_conflict: tunables.resolve_bool(
                Tunable::MatchingBlockOnYearConflict,
                base.guards.year_conflict,
            ),
            type_conflict: tunables.resolve_bool(
                Tunable::MatchingBlockOnTypeConflict,
                base.guards.type_conflict,
            ),
        },
        ..base
    }
}

/// What this deployment falls back to for `tunable` when no override stands: its configured
/// value, not the compiled default.
///
/// The two coincide until someone sets a `TANKOVAULT_MATCHING__*` key, which is exactly the
/// deployment whose console would otherwise show a number the sweep does not use.
fn baseline(config: &MatchingConfig, tunable: Tunable) -> f64 {
    let on = |yes: bool| if yes { 1.0 } else { 0.0 };
    match tunable {
        Tunable::MatchingAutoMerge => f64::from(config.auto_merge),
        Tunable::MatchingBlockOnNumericConflict => on(config.block_auto_merge_on_numeric_conflict),
        Tunable::MatchingBlockOnAuthorConflict => on(config.block_auto_merge_on_author_conflict),
        Tunable::MatchingBlockOnYearConflict => on(config.block_auto_merge_on_year_conflict),
        Tunable::MatchingBlockOnTypeConflict => on(config.block_auto_merge_on_type_conflict),
        // Unreachable for anything `Tunable::matching()` lists, and the console is served from
        // that list. A knob added to the group without a baseline here would silently follow the
        // compiled default instead of the configuration, so it answers with the registry's own
        // value rather than pretending to a configured one.
        other => other.default_value(),
    }
}

/// Every knob of the policy, with its effective value and the provenance of any override.
///
/// # Errors
/// Database failures reading the override rows.
pub(crate) async fn view(
    pool: &PgPool,
    config: &MatchingConfig,
    tunables: &TunableSet,
) -> anyhow::Result<Vec<MergePolicyView>> {
    let stored = tankovault_db::repo::tunables::list_overrides(pool).await?;
    Ok(Tunable::matching()
        .iter()
        .map(|&tunable| {
            let spec = tunable.spec();
            let row = stored.iter().find(|row| row.key == spec.key);
            let default_value = spec.clamp(baseline(config, tunable));
            MergePolicyView {
                key: spec.key.to_owned(),
                title: spec.title.to_owned(),
                description: spec.description.to_owned(),
                kind: spec.kind.as_str().to_owned(),
                applies: spec.applies.as_str().to_owned(),
                // Resolved through the same call the sweep makes, rather than from `row`
                // directly, so the number on the page cannot disagree with the number applied.
                value: tunables.resolve(tunable, default_value),
                default_value,
                min: spec.min,
                max: spec.max,
                overridden: row.is_some(),
                note: row.and_then(|r| r.note.clone()),
                updated_by: row.and_then(|r| r.updated_by.clone()),
                updated_at: row.and_then(updated_at),
            }
        })
        .collect())
}

fn updated_at(row: &TunableOverrideRow) -> Option<String> {
    row.updated_at
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

/// Record — or withdraw — an operator's decision about one knob, then re-read the snapshot so
/// this replica's next sweep already applies it.
///
/// `value: None` withdraws the override, returning the knob to the deployment's configuration.
/// The value is expected to have been range-checked by the caller — [`refusal`] is that check —
/// because every reader clamps regardless and a write that reached here out of range would be
/// stored and then silently applied as something else.
///
/// # Errors
/// Database failures.
pub(crate) async fn apply(
    pool: &PgPool,
    tunables: &TunableSet,
    tunable: Tunable,
    value: Option<f64>,
    note: Option<&str>,
    actor: UserId,
) -> anyhow::Result<()> {
    let key = tunable.key();
    match value {
        Some(value) => {
            tankovault_db::repo::tunables::set_override(pool, key, value, note, actor).await?;
        }
        None => {
            tankovault_db::repo::tunables::clear_override(pool, key).await?;
        }
    }
    // Before answering, so the sweep this replica runs next already applies the new policy
    // rather than waiting out the refresh interval.
    tunables.refresh().await;
    Ok(())
}

/// Why this write cannot be stored, or `None` when it can.
///
/// Refused rather than clamped: every reader clamps, so a stored value outside the range is
/// survivable — but accepting the write would report success for a policy the sweep will not
/// apply.
#[must_use]
pub(crate) fn refusal(tunable: Tunable, value: f64) -> Option<String> {
    let spec = tunable.spec();
    if value.is_finite() && spec.range().contains(&value) {
        return None;
    }
    Some(format!(
        "\"{}\" must be between {} and {}, got {value}",
        spec.title, spec.min, spec.max
    ))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "every matching threshold is a ratio in 0..=1, which f32 holds exactly enough of; \
              the scorer is f32 throughout and this is the one narrowing point"
)]
fn as_f32(value: f64) -> f32 {
    value as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> MatchingConfig {
        MatchingConfig {
            auto_merge: 0.99,
            block_auto_merge_on_year_conflict: false,
            ..MatchingConfig::default()
        }
    }

    /// **A deployment's configured policy survives the console being empty.**
    ///
    /// The bug this pins: resolving the sweep's policy out of the tuning snapshot alone, which
    /// answers with the compiled default for every knob nobody has touched. An operator who had
    /// switched the year guard off in configuration would have had it switched back on by an
    /// unrelated deploy, and the only evidence would be merges that stopped happening.
    #[test]
    fn an_untouched_console_leaves_the_configuration_in_force() {
        let resolved = thresholds(&configured(), &TunableSet::defaults());
        assert!((resolved.auto_merge - 0.99).abs() < f32::EPSILON);
        assert!(!resolved.guards.year_conflict);
        assert!(resolved.guards.author_conflict, "untouched, so still on");
    }

    #[test]
    fn an_override_replaces_the_configured_value_key_by_key() {
        let set = TunableSet::with_values(&[
            (Tunable::MatchingAutoMerge, 0.90),
            (Tunable::MatchingBlockOnYearConflict, 1.0),
            (Tunable::MatchingBlockOnTypeConflict, 0.0),
        ]);
        let resolved = thresholds(&configured(), &set);

        assert!((resolved.auto_merge - 0.90).abs() < f32::EPSILON);
        assert!(
            resolved.guards.year_conflict,
            "switched back on from the console"
        );
        assert!(!resolved.guards.type_conflict);
        // Untouched by either layer, so still the compiled default.
        assert!(resolved.guards.numeric_conflict);
    }

    /// The baseline the console offers as "reset to" has to be the value the sweep would then
    /// apply, or resetting a knob moves the policy somewhere neither layer asked for.
    #[test]
    fn every_knob_baselines_onto_the_configured_policy() {
        let config = configured();
        let applied = thresholds(&config, &TunableSet::defaults());
        for &tunable in Tunable::matching() {
            let base = baseline(&config, tunable);
            let expected = match tunable {
                Tunable::MatchingAutoMerge => f64::from(applied.auto_merge),
                Tunable::MatchingBlockOnNumericConflict => {
                    f64::from(u8::from(applied.guards.numeric_conflict))
                }
                Tunable::MatchingBlockOnAuthorConflict => {
                    f64::from(u8::from(applied.guards.author_conflict))
                }
                Tunable::MatchingBlockOnYearConflict => {
                    f64::from(u8::from(applied.guards.year_conflict))
                }
                Tunable::MatchingBlockOnTypeConflict => {
                    f64::from(u8::from(applied.guards.type_conflict))
                }
                other => panic!("{other} is in the matching group with no baseline"),
            };
            assert!(
                (base - expected).abs() < f64::EPSILON,
                "{tunable} baselines to {base}, sweep applies {expected}"
            );
        }
    }
}
