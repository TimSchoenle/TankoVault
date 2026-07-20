//! Adapter construction: map a provider's `adapter` kind + `config` to a live adapter.

use crate::config::AdapterConfig;
use crate::demonicscans::DemonicScansAdapter;
use crate::error::AdapterError;
use crate::generic::GenericConfigAdapter;
use crate::madara::madara_default_config;
use crate::types::SourceAdapter;
use tankovault_domain::AdapterKind;
use serde_json::Value;

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

/// Build the adapter for a provider.
///
/// - `Madara`: Madara defaults with the provider `config` merged on top (provider wins).
/// - `GenericConfig`: the provider `config` used as-is.
/// - `Custom`: dispatched by slug to a registered struct, otherwise
///   [`AdapterError::UnknownCustom`].
///
/// # Errors
/// [`AdapterError::Config`] if the effective config is malformed, or
/// [`AdapterError::UnknownCustom`] for an unregistered custom provider.
pub fn build_adapter(
    adapter: AdapterKind,
    slug: &str,
    config: &Value,
) -> Result<Box<dyn SourceAdapter>, AdapterError> {
    match adapter {
        AdapterKind::Madara => {
            let mut effective = madara_default_config();
            merge(&mut effective, config);
            let cfg = AdapterConfig::from_value(&effective)?;
            Ok(Box::new(GenericConfigAdapter::new(cfg)))
        }
        AdapterKind::GenericConfig => {
            let cfg = AdapterConfig::from_value(config)?;
            Ok(Box::new(GenericConfigAdapter::new(cfg)))
        }
        AdapterKind::Custom => match slug {
            "demonicscans" => Ok(Box::new(DemonicScansAdapter::new())),
            other => Err(AdapterError::UnknownCustom(other.to_owned())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn madara_defaults_allow_empty_provider_config() {
        // A brand-new Madara provider needs no selectors of its own.
        let adapter = build_adapter(AdapterKind::Madara, "kunmanga", &serde_json::json!({}));
        assert!(adapter.is_ok());
    }

    #[test]
    fn provider_config_overrides_madara_default() {
        let over = serde_json::json!({ "catalog": { "path": "/series/?p={page}" } });
        let mut base = madara_default_config();
        merge(&mut base, &over);
        assert_eq!(base["catalog"]["path"], "/series/?p={page}");
        // Untouched defaults survive the merge.
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
