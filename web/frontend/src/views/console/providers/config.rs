//! The Config tab's dry-run verdict, and the one rule that reads a sample out of it.

use crate::components::{ErrorLine, Section};
use crate::i18n::use_i18n;
use dioxus::prelude::*;

/// The dry-run panel: what the adapter parsed, and the raw sample behind the claim.
#[component]
pub(super) fn DryRunResult(result: Option<Result<serde_json::Value, String>>) -> Element {
    let i18n = use_i18n();
    let Some(result) = result else {
        return rsx! {
            Section { label: i18n.t("console.providers.dryRunHead"),
                p { class: "ik-muted", style: "font-size:12px;line-height:1.5;margin:0;",
                    {i18n.t("console.adapterTest.hint")}
                }
            }
        };
    };

    match result {
        Err(message) => rsx! {
            Section { label: i18n.t("console.providers.dryRunHead"),
                ErrorLine { message }
            }
        },
        Ok(value) => {
            let parsed = parsed_count(&value);
            let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
            rsx! {
                Section {
                    label: i18n.t("console.providers.dryRunHead"),
                    trailing: rsx! {
                        span { class: "ik-pill jade", style: "font-size:9.5px;",
                            match parsed {
                                Some(count) => {
                                    i18n.plural(
                                        "console.providers.parsed",
                                        i64::try_from(count).unwrap_or(0),
                                        &[],
                                    )
                                }
                                None => i18n.t("console.providers.parsedOk"),
                            }
                        }
                    },
                    pre {
                        class: "ik-jsonblock",
                        style: "max-height:340px;white-space:pre-wrap;word-break:break-word;",
                        "{text}"
                    }
                }
            }
        }
    }
}

/// How many entries a dry-run returned, when the payload's shape makes that answerable.
///
/// Adapter output is adapter-defined, so this looks for the two shapes every adapter in the
/// tree produces — a bare array, or an object with one array in it — and otherwise declines to
/// guess rather than reporting a number it cannot stand behind.
pub(super) fn parsed_count(value: &serde_json::Value) -> Option<usize> {
    if let Some(array) = value.as_array() {
        return Some(array.len());
    }
    let object = value.as_object()?;
    let mut arrays = object.values().filter_map(serde_json::Value::as_array);
    let first = arrays.next()?;
    arrays.next().is_none().then_some(first.len())
}

#[cfg(test)]
mod tests {
    use super::parsed_count;
    use serde_json::json;

    #[test]
    fn a_bare_array_is_counted() {
        assert_eq!(parsed_count(&json!([1, 2, 3])), Some(3));
    }

    #[test]
    fn an_object_with_exactly_one_array_is_counted() {
        assert_eq!(
            parsed_count(&json!({ "ok": true, "series": [1, 2] })),
            Some(2)
        );
    }

    #[test]
    fn an_ambiguous_shape_declines_to_guess() {
        assert_eq!(parsed_count(&json!({ "a": [1], "b": [2] })), None);
        assert_eq!(parsed_count(&json!({ "ok": true })), None);
        assert_eq!(parsed_count(&json!("text")), None);
    }
}
