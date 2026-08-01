//! Adapter construction: map a provider's `adapter` kind + `config` to a live adapter.

use crate::config::AdapterConfig;
use crate::demonicscans::DemonicScansAdapter;
use crate::error::AdapterError;
use crate::generic::GenericConfigAdapter;
use crate::kunmanga::KunMangaAdapter;
use crate::madara::madara_default_config;
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
        AdapterKind::Madara => Ok(Box::new(GenericConfigAdapter::new(madara_config(config)?))),
        AdapterKind::GenericConfig => {
            let cfg = AdapterConfig::from_value(config)?;
            Ok(Box::new(GenericConfigAdapter::new(cfg)))
        }
        AdapterKind::Custom => match slug {
            "demonicscans" => Ok(Box::new(DemonicScansAdapter::new())),
            // Hybrid: Madara-shaped HTML for catalogue/series, JSON API for chapters.
            "kunmanga" => Ok(Box::new(KunMangaAdapter::new(madara_config(config)?))),
            other => Err(AdapterError::UnknownCustom(other.to_owned())),
        },
    }
}

/// Merge a provider `config` onto the Madara selector defaults and parse the result.
fn madara_config(config: &Value) -> Result<AdapterConfig, AdapterError> {
    let mut effective = madara_default_config();
    merge(&mut effective, config);
    AdapterConfig::from_value(&effective)
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
