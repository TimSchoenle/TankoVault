//! Fills each published key's `default` from what the binary's own deserialiser produces.
//!
//! [`Schema::with_defaults_from`] reads defaults off a serialised value of the root. No root here
//! implements `Default`, because each has keys the service refuses to start without, so the value
//! is built the way the loader builds one: by deserialising a document. That document supplies the
//! required keys and nothing else, so every other field takes exactly the default serde gives it
//! at boot, `#[serde(default = "…")]` functions included.
//!
//! An `Option` section left out of that document has no fields to read, so its keys would publish
//! no default even where the section, once present, gives them one. Each such section is observed
//! in a second pass, present with only its text keys supplied.
//!
//! A supplied value never reaches the contract: a required key publishes no default by definition,
//! and a key supplied to open a section is not read from that pass.

use std::collections::BTreeSet;

use serde::Serialize;
use serde::de::DeserializeOwned;
use terrace_config::Error;
use terrace_config::figment::Figment;
use terrace_config::figment::providers::Serialized;
use terrace_config::figment::value::Value;
use terrace_config::schema::{Key, Schema};

/// What a text key is supplied with while defaults are observed.
const PLACEHOLDER: &str = "required";

/// Returns `schema` with every non-required key's default observed from `T`'s deserialiser.
///
/// # Errors
/// [`Error::Invalid`] when a required key is not text, so no value can be supplied without
/// guessing one, or when `T` refuses a document holding only its required keys.
pub(crate) fn with_observed_defaults<T>(schema: Schema) -> Result<Schema, Error>
where
    T: DeserializeOwned + Serialize,
{
    let required: Vec<&Key> = schema.keys.iter().filter(|key| key.required).collect();
    for key in &required {
        if !is_text(key) {
            return Err(Error::Invalid(format!(
                "the defaults could not be observed: `{}` is required and not text, so a document \
                 holding only the required keys would need a value invented for it",
                key.path
            )));
        }
    }

    let minimal = supplying(required.iter().map(|key| key.path.as_str()));
    let root: T = minimal.clone().extract().map_err(|error| {
        Error::Invalid(format!(
            "the defaults could not be observed: the root refuses a document holding only its \
             required keys: {error}"
        ))
    })?;
    let observed = Value::serialize(&root).map_err(Box::new)?;
    let mut published = schema.with_defaults_from_value(&observed);

    for section in absent_sections(&published, &observed) {
        // Opening the section takes a value for each of its text keys that has no default; a
        // section that still refuses the document stays unobserved rather than guessed at.
        let opened: BTreeSet<String> = published
            .keys
            .iter()
            .filter(|key| within(&key.path, &section) && key.default.is_none() && is_text(key))
            .map(|key| key.path.clone())
            .collect();
        let Ok(root) = minimal
            .clone()
            .merge(supplying(opened.iter().map(String::as_str)))
            .extract::<T>()
        else {
            continue;
        };
        let observed = Value::serialize(&root).map_err(Box::new)?;
        let pass = published.clone().with_defaults_from_value(&observed);

        for key in published.keys.iter_mut().filter(|key| {
            within(&key.path, &section) && key.default.is_none() && !opened.contains(&key.path)
        }) {
            if let Some(seen) = pass.keys.iter().find(|seen| seen.path == key.path) {
                key.default.clone_from(&seen.default);
                key.default_value.clone_from(&seen.default_value);
                key.required = seen.required;
            }
        }
    }
    Ok(published)
}

/// A figment supplying [`PLACEHOLDER`] at each path.
fn supplying<'a>(paths: impl IntoIterator<Item = &'a str>) -> Figment {
    paths.into_iter().fold(Figment::new(), |figment, path| {
        figment.merge(Serialized::default(path, PLACEHOLDER))
    })
}

/// The shallowest tables holding a key with no default that the observed root left out entirely.
fn absent_sections(schema: &Schema, observed: &Value) -> BTreeSet<String> {
    let mut sections = BTreeSet::new();
    for key in schema
        .keys
        .iter()
        .filter(|key| key.default.is_none() && !key.secret)
    {
        let segments: Vec<&str> = key.path.split('.').collect();
        let absent = (1..segments.len())
            .map(|depth| segments[..depth].join("."))
            // A `None` section serialises as `Empty` rather than going missing.
            .find(|table| {
                observed
                    .find_ref(table)
                    .is_none_or(|held| matches!(held, Value::Empty(..)))
            });
        if let Some(table) = absent {
            sections.insert(table);
        }
    }
    sections
}

/// Whether `path` lies inside the table `section`.
fn within(path: &str, section: &str) -> bool {
    path.strip_prefix(section)
        .is_some_and(|rest| rest.starts_with('.'))
}

/// Whether a key's constraint says it holds text.
fn is_text(key: &Key) -> bool {
    key.constraint
        .as_ref()
        .and_then(|constraint| constraint.get("type"))
        .and_then(serde_json::Value::as_str)
        == Some("string")
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use terrace_config::schema::{Describe, Schema};

    use super::with_observed_defaults;

    #[derive(Deserialize, Serialize, Describe)]
    struct Root {
        /// Required: no serde default.
        url: String,
        /// Defaulted by a function, as most keys here are.
        #[serde(default = "default_port")]
        port: u16,
        #[serde(default)]
        #[config(nested)]
        section: Section,
        /// A section the service may run without.
        #[serde(default)]
        #[config(nested)]
        broker: Option<Broker>,
    }

    #[derive(Default, Deserialize, Serialize, Describe)]
    struct Section {
        /// Defaulted through the section's `Default`.
        #[serde(default)]
        enabled: bool,
        /// No default at all.
        name: Option<String>,
    }

    #[derive(Deserialize, Serialize, Describe)]
    struct Broker {
        /// Required once the section is present.
        address: String,
        /// Defaulted once the section is present.
        #[serde(default = "default_retention")]
        retention_secs: u64,
    }

    fn default_port() -> u16 {
        8080
    }

    fn default_retention() -> u64 {
        604_800
    }

    fn observed<T: Describe + serde::de::DeserializeOwned + Serialize>() -> Schema {
        with_observed_defaults::<T>(tankovault_config::terrace().schema::<T>())
            .expect("the defaults are observed")
    }

    fn default_of(schema: &Schema, path: &str) -> Option<String> {
        schema
            .keys
            .iter()
            .find(|key| key.path == path)
            .unwrap_or_else(|| panic!("no key {path}"))
            .default
            .clone()
    }

    /// The contract published no defaults at all while the roots could not be serialised, which
    /// left every chart value reading `null` with nothing saying what the image does instead.
    #[test]
    fn serde_defaults_are_published_and_required_keys_are_not() {
        let schema = observed::<Root>();
        assert_eq!(default_of(&schema, "port").as_deref(), Some("8080"));
        assert_eq!(
            default_of(&schema, "section.enabled").as_deref(),
            Some("false")
        );
        assert_eq!(default_of(&schema, "section.name"), None);
        assert_eq!(
            default_of(&schema, "url"),
            None,
            "the placeholder must not leak"
        );
    }

    /// `api`'s optional `nats` section hid `nats.events_max_age_secs`'s default, which every
    /// service requiring the section published.
    #[test]
    fn an_optional_section_publishes_the_defaults_it_takes_once_present() {
        let schema = observed::<Root>();
        assert_eq!(
            default_of(&schema, "broker.retention_secs").as_deref(),
            Some("604800")
        );
        assert_eq!(
            default_of(&schema, "broker.address"),
            None,
            "the opener must not leak"
        );
    }

    #[derive(Deserialize, Serialize, Describe)]
    struct NumericRequired {
        /// Required and not text.
        workers: u32,
    }

    /// A required key with no obvious value is refused rather than guessed at.
    #[test]
    fn a_required_key_that_is_not_text_is_refused() {
        let Err(error) = with_observed_defaults::<NumericRequired>(
            tankovault_config::terrace().schema::<NumericRequired>(),
        ) else {
            panic!("a numeric required key was given an invented value");
        };
        assert!(
            error.to_string().contains("`workers` is required"),
            "{error}"
        );
    }
}
