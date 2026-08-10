//! Adapter construction: map a provider's `adapter` kind + `config` to a live adapter.

use crate::config::AdapterConfig;
use crate::demonicscans::DemonicScansAdapter;
use crate::error::AdapterError;
use crate::generic::GenericConfigAdapter;
use crate::kunmanga::KunMangaAdapter;
use crate::astro::AstroIslandAdapter;
use crate::comick::ComickAdapter;
use crate::flamecomics::FlameComicsAdapter;
use crate::heancms::HeanCmsAdapter;
use crate::madara::madara_default_config;
use crate::mangadex::MangaDexAdapter;
use crate::webtoons::WebtoonsAdapter;
use crate::manganato::{ManganatoAdapter, manganato_default_config};
use crate::mangathemesia::mangathemesia_default_config;
use crate::types::SourceAdapter;
use serde_json::Value;
use tankovault_domain::AdapterKind;

/// Deep-merge `over` into `base` (object keys recurse; other values replace).
fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                merge(b.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (b, o) => *b = o.clone(),
    }
}

/// Build the adapter for a provider: `Madara` merges provider `config` onto Madara defaults,
/// `GenericConfig` uses `config` as-is, `Custom` dispatches by slug (`kunmanga` reuses the
/// Madara HTML selectors and overrides only chapter fetching).
///
/// # Errors
/// Malformed effective config, or an unregistered custom provider slug.
pub fn build_adapter(
    adapter: AdapterKind,
    slug: &str,
    config: &Value,
) -> Result<Box<dyn SourceAdapter>, AdapterError> {
    match adapter {
        AdapterKind::Madara => Ok(Box::new(GenericConfigAdapter::new(family_config(
            madara_default_config(),
            config,
        )?))),
        AdapterKind::MangaThemesia => Ok(Box::new(GenericConfigAdapter::new(family_config(
            mangathemesia_default_config(),
            config,
        )?))),
        // The family's chapter list is a JSON endpoint, not markup — see `ManganatoAdapter`.
        AdapterKind::Manganato => Ok(Box::new(ManganatoAdapter::new(family_config(
            manganato_default_config(),
            config,
        )?))),
        AdapterKind::GenericConfig => {
            let cfg = AdapterConfig::from_value(config)?;
            Ok(Box::new(GenericConfigAdapter::new(cfg)))
        }
        AdapterKind::Custom => match slug {
            "demonicscans" => Ok(Box::new(DemonicScansAdapter::new())),
            // Hybrid: Madara-shaped HTML for catalogue/series, JSON API for chapters.
            "kunmanga" => Ok(Box::new(KunMangaAdapter::new(family_config(
                madara_default_config(),
                config,
            )?))),
            // First-party JSON APIs. Both split the reader host from the API host, which is why
            // neither is a config row: the requests go somewhere the `base_url` does not.
            "mangadex" => Ok(Box::new(MangaDexAdapter::new())),
            "comick" => Ok(Box::new(ComickAdapter::new())),
            // HeanCMS, whose chapter rows carry the paywall as `price`/`free_at`.
            "omegascans" => Ok(Box::new(HeanCmsAdapter::new(config)?)),
            // Astro islands: the chapter list, with its lock flags, is the island's props.
            "asura" | "hivetoons" => Ok(Box::new(AstroIslandAdapter::new(slug)?)),
            "flamecomics" => Ok(Box::new(FlameComicsAdapter::new())),
            "webtoons" => Ok(Box::new(WebtoonsAdapter::new())),
            other => Err(AdapterError::UnknownCustom(other.to_owned())),
        },
    }
}

/// Merge a provider `config` onto a family's selector defaults and parse the result.
fn family_config(mut defaults: Value, config: &Value) -> Result<AdapterConfig, AdapterError> {
    merge(&mut defaults, config);
    AdapterConfig::from_value(&defaults)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn madara_defaults_allow_empty_provider_config() {
        let adapter = build_adapter(AdapterKind::Madara, "kunmanga", &serde_json::json!({}));
        assert!(adapter.is_ok());
    }

    #[test]
    fn provider_config_overrides_madara_default() {
        let over = serde_json::json!({ "catalog": { "path": "/series/?p={page}" } });
        let mut base = madara_default_config();
        merge(&mut base, &over);
        assert_eq!(base["catalog"]["path"], "/series/?p={page}");
        assert_eq!(base["catalog"]["item"], "div.page-item-detail");
    }

    #[test]
    fn registered_custom_adapter_builds() {
        let adapter = build_adapter(AdapterKind::Custom, "demonicscans", &serde_json::json!({}));
        assert!(adapter.is_ok());
    }

    #[test]
    fn custom_without_registration_errors() {
        let err = build_adapter(
            AdapterKind::Custom,
            "not-a-real-provider",
            &serde_json::json!({}),
        );
        assert!(matches!(err, Err(AdapterError::UnknownCustom(_))));
    }
}
