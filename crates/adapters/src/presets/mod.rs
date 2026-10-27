//! Built-in provider presets, ready to seed (`xtask seed`). Each Madara preset stores only the
//! selector overrides where the site deviates from
//! [`madara_default_config`](crate::madara_default_config).

mod aggregators;
mod custom;
mod iken;
mod keyoapp;
mod madara;
mod manganato;
mod selector;
mod themesia;

use serde_json::{Value, json};
use tankovault_domain::{AdapterKind, Politeness};

use custom::custom_code;
use iken::iken_platform;
use keyoapp::keyoapp_family;
use madara::madara_family;
use manganato::manganato_family;
use selector::selector_only;
use themesia::mangathemesia_family;

/// A ready-to-seed provider definition.
///
/// Identity, domain and adapter kind, the selector overrides merged onto the adapter defaults
/// (empty for a fully custom adapter), and the crawl budget the site's size warrants.
pub struct BuiltinPreset {
    /// Stable slug (rate-limit + custom-adapter dispatch key).
    pub slug: &'static str,
    /// Human-readable display name.
    pub name: &'static str,
    /// Domain root; every stored relative link resolves against it.
    pub base_url: &'static str,
    /// Which adapter drives the site.
    pub adapter: AdapterKind,
    /// `providers.config` overrides (merged onto Madara defaults; ignored for `Custom`).
    pub config: Value,
    /// Seed crawl budget. Operators may tune it downward in the console at any time; the
    /// hard ceilings in [`tankovault_domain::Politeness`] bound it upward regardless.
    pub politeness: Politeness,
}

/// Every provider preset bundled with this build.
///
/// Grouped by how each is onboarded — the grouping is the point: a site on a shared theme is a
/// config row, and only a genuinely bespoke one costs a Rust module.
///
/// This is the definition side of the preset link: `bootstrap seed-providers` records the list
/// in `provider_presets` and re-applies it to every locked provider, so editing an entry here
/// reaches deployments that already carry the row.
#[must_use]
pub fn builtin() -> Vec<BuiltinPreset> {
    let mut all = madara_family();
    all.extend(mangathemesia_family());
    all.extend(manganato_family());
    all.extend(keyoapp_family());
    all.extend(selector_only());
    all.extend(iken_platform());
    all.extend(custom_code());
    all
}

/// Shorthands for the families that name most of the presets below.
const MADARA: AdapterKind = AdapterKind::Madara;
const THEMESIA: AdapterKind = AdapterKind::MangaThemesia;
const KEYOAPP: AdapterKind = AdapterKind::Keyoapp;
const GENERIC: AdapterKind = AdapterKind::GenericConfig;

/// A site that runs a family theme with no deviation from its defaults at all.
///
/// Most of a family's sites are this: the platform is hosted or the theme is installed
/// unmodified, so the row is pure identity. Spelling them as a helper keeps the deviations that
/// *do* exist visible instead of buried in repeated boilerplate.
fn plain(
    slug: &'static str,
    name: &'static str,
    base_url: &'static str,
    adapter: AdapterKind,
) -> BuiltinPreset {
    BuiltinPreset {
        slug,
        name,
        base_url,
        adapter,
        config: json!({}),
        politeness: Politeness::default(),
    }
}

/// The crawl budget for a catalogue in the tens of thousands of series.
///
/// Double the default, half the policy ceiling: enforced per worker process, so at the shipped
/// two replicas this is exactly `MAX_RPS`/`MAX_CONCURRENCY` aggregate. Raising the replica count
/// without lowering these silently exceeds the ceiling.
fn bulk_budget() -> Politeness {
    Politeness {
        rps: 4.0,
        concurrency: 8,
        ..Politeness::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_adapter;

    #[test]
    fn every_preset_crawl_budget_is_within_policy() {
        // A shipped budget outside policy ceilings would be silently clamped; catch it here.
        for p in builtin() {
            assert!(
                p.politeness.rps > 0.0
                    && p.politeness.rps <= tankovault_domain::politeness::MAX_RPS,
                "{}: rps {} outside (0, {}]",
                p.slug,
                p.politeness.rps,
                tankovault_domain::politeness::MAX_RPS
            );
            assert!(
                p.politeness.concurrency > 0
                    && p.politeness.concurrency <= tankovault_domain::politeness::MAX_CONCURRENCY,
                "{}: concurrency {} outside (0, {}]",
                p.slug,
                p.politeness.concurrency,
                tankovault_domain::politeness::MAX_CONCURRENCY
            );
            assert_eq!(
                p.politeness.clone().clamped(),
                p.politeness,
                "{}: clamping changed the shipped budget",
                p.slug
            );
        }
    }

    #[test]
    fn every_preset_builds_an_adapter() {
        // A preset that cannot be turned into a live adapter is a packaging bug.
        for p in builtin() {
            build_adapter(p.adapter, p.slug, &p.config)
                .unwrap_or_else(|e| panic!("preset {:?} failed to build: {e}", p.slug));
        }
    }

    #[test]
    fn slugs_are_unique() {
        let mut slugs: Vec<_> = builtin().iter().map(|p| p.slug).collect();
        slugs.sort_unstable();
        let len = slugs.len();
        slugs.dedup();
        assert_eq!(slugs.len(), len, "preset slugs must be unique");
    }
}
