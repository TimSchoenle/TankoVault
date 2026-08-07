//! The politeness editor's emulation vocabulary, and the body its Save submits.

use crate::models::{Politeness, PolitenessEmulation};
// Not re-exported by `crate::models`: only this picker names the profiles.
use crate::wire::types::BrowserEmulation;

/// The emulation profiles the picker offers, in display order, each with its label.
///
/// Hand-listed because the generated client carries no way to enumerate a schema enum, and kept
/// in step with the API by `the_picker_offers_every_emulation_profile`. The labels are browser
/// product names rather than catalogue keys — they are the same in every locale.
pub(super) const EMULATION_CHOICES: [(BrowserEmulation, &str); 5] = [
    (BrowserEmulation::Chrome, "Chrome"),
    (BrowserEmulation::Firefox, "Firefox"),
    (BrowserEmulation::Safari, "Safari"),
    (BrowserEmulation::Edge, "Edge"),
    (BrowserEmulation::OkHttp, "OkHttp (Android)"),
];

/// The picker's value for a provider's stored emulation; empty is the "no emulation" option.
pub(super) fn emulation_token(stored: Option<&PolitenessEmulation>) -> String {
    match stored {
        Some(PolitenessEmulation::Variant1(profile)) => profile.to_string(),
        // The raw-JSON arm of the generated untagged nullable `$ref`; `null` reads as empty.
        Some(PolitenessEmulation::Variant0(value)) => value.as_str().unwrap_or_default().to_owned(),
        None => String::new(),
    }
}

/// Build the politeness body from the editor's fields, or the catalogue key of the field that
/// would not parse.
///
/// `emulation` is the wire token of a browser profile, or empty for "no emulation".
///
/// # Errors
///
/// The catalogue key naming the first of `rps`, `concurrency` or `crawl_delay_ms` that is not a
/// non-negative number.
pub(super) fn politeness_body(
    rps: &str,
    concurrency: &str,
    crawl_delay_ms: &str,
    user_agent: &str,
    emulation: &str,
) -> Result<Politeness, &'static str> {
    let rps: f64 = rps.trim().parse().map_err(|_| "console.providers.badRps")?;
    // Parsed unsigned and then narrowed, so a negative or oversized figure is refused here
    // rather than reaching the server as a value it would clamp into something else.
    let concurrency = concurrency
        .trim()
        .parse::<u32>()
        .ok()
        .and_then(|n| i32::try_from(n).ok())
        .ok_or("console.providers.badConcurrency")?;
    let crawl_delay_ms = crawl_delay_ms
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| i64::try_from(n).ok())
        .ok_or("console.providers.badCrawlDelay")?;
    Ok(Politeness {
        rps: Some(rps),
        concurrency: Some(concurrency),
        crawl_delay_ms: Some(crawl_delay_ms),
        user_agent: Some(user_agent.to_owned()),
        emulation: Some(emulation_field(emulation)),
    })
}

/// The `emulation` field, with "no emulation" carried as the explicit `null` the server needs.
///
/// This must never be `None`. The generated `Politeness` skips a `None` emulation when it
/// serialises, and the server's serde default for an absent `emulation` key is
/// `Some(BrowserEmulation::Chrome)` — so omitting the key does not mean "no emulation", it
/// means "put this provider back on Chrome". `Variant0` is the raw-JSON arm of the generated
/// untagged nullable `$ref`, and holding `Value::Null` in it is the only way this client can
/// put a literal `null` on the wire.
fn emulation_field(token: &str) -> PolitenessEmulation {
    if token.is_empty() {
        return PolitenessEmulation::Variant0(serde_json::Value::Null);
    }
    match token.parse::<BrowserEmulation>() {
        Ok(profile) => PolitenessEmulation::Variant1(profile),
        // Not degraded to `null`: an unrecognised token can only be a bug in the picker above,
        // and the server refusing it names the value, where a quiet `null` would silently turn
        // emulation off and look like it worked.
        Err(_) => PolitenessEmulation::Variant0(serde_json::Value::String(token.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(emulation: &str) -> serde_json::Value {
        let politeness = politeness_body("1.5", "4", "250", "TankoVault/1.0", emulation)
            .expect("the numeric fields parse");
        serde_json::to_value(politeness).expect("the body serialises")
    }

    /// Choosing "no emulation" and saving used to leave the provider on Chrome.
    ///
    /// The editor built the payload as a `serde_json::Value` carrying an explicit
    /// `"emulation": null` — which is what the server needs, because its serde default for an
    /// *absent* key is `Some(BrowserEmulation::Chrome)` — and then round-tripped it through
    /// `serde_json::from_value::<Politeness>(…)`. That deserialised the `null` to
    /// `Option::None`, and the generated struct's `skip_serializing_if = "Option::is_none"`
    /// dropped the key from the request entirely. The provider went back to impersonating
    /// Chrome, the save reported success, and reloading showed the picker on "no emulation"
    /// until the next fetch.
    #[test]
    fn choosing_no_emulation_sends_an_explicit_null() {
        assert_eq!(body("")["emulation"], serde_json::Value::Null);
        assert!(
            body("")
                .as_object()
                .is_some_and(|o| o.contains_key("emulation")),
            "the key must be present and null, not absent — an absent key defaults to Chrome"
        );
    }

    /// A chosen profile still goes out as its own wire token.
    #[test]
    fn a_chosen_profile_is_sent_as_its_token() {
        assert_eq!(body("ok_http")["emulation"], serde_json::json!("ok_http"));
    }

    /// An unrecognised token must reach the server, which refuses it by name. Mapping it to
    /// `null` instead would silently disable emulation on a save that looked like it worked.
    #[test]
    fn an_unrecognised_token_is_not_quietly_turned_into_no_emulation() {
        assert_eq!(body("netscape")["emulation"], serde_json::json!("netscape"));
    }

    /// The picker's value for a provider must round-trip: what seeds the control has to be a
    /// token the control can send back, or every save silently rewrites the emulation.
    #[test]
    fn the_picker_token_round_trips_through_the_body() {
        let cases = ["", "chrome", "firefox", "safari", "edge", "ok_http"];
        for token in cases {
            let politeness = politeness_body("1", "1", "0", "", token).expect("fields parse");
            assert_eq!(
                emulation_token(politeness.emulation.as_ref()),
                token,
                "`{token}` did not survive the editor's own round trip"
            );
        }
    }

    /// A stored emulation of `null` seeds the "no emulation" option, not the first one.
    #[test]
    fn a_null_emulation_seeds_the_no_emulation_option() {
        assert_eq!(emulation_token(None), "");
        assert_eq!(
            emulation_token(Some(&PolitenessEmulation::Variant0(
                serde_json::Value::Null
            ))),
            ""
        );
    }

    /// Each numeric field reports itself, so the message names the control to correct.
    #[test]
    fn each_unparseable_field_names_itself() {
        assert_eq!(
            politeness_body("x", "4", "250", "", "").unwrap_err(),
            "console.providers.badRps"
        );
        assert_eq!(
            politeness_body("1", "-1", "250", "", "").unwrap_err(),
            "console.providers.badConcurrency"
        );
        assert_eq!(
            politeness_body("1", "4", "2.5", "", "").unwrap_err(),
            "console.providers.badCrawlDelay"
        );
    }

    /// The emulation picker must offer every profile the API accepts.
    ///
    /// The list is hand-maintained — the generated client cannot enumerate a schema enum — and a
    /// missing profile is unreachable from the console with nothing reporting it. Read out of
    /// the committed `openapi.json`, the only artefact connecting this workspace to the API's.
    #[test]
    fn the_picker_offers_every_emulation_profile() {
        const SPEC: &str = include_str!("../../../../../../openapi.json");
        let spec: serde_json::Value = serde_json::from_str(SPEC).expect("openapi.json parses");

        let mut published: Vec<String> = spec["components"]["schemas"]["BrowserEmulation"]["enum"]
            .as_array()
            .expect("the document declares the BrowserEmulation vocabulary")
            .iter()
            .map(|v| v.as_str().expect("emulation tokens are strings").to_owned())
            .collect();
        let mut offered: Vec<String> = EMULATION_CHOICES
            .iter()
            .map(|(profile, _)| profile.to_string())
            .collect();

        published.sort();
        offered.sort();
        assert_eq!(
            offered, published,
            "the politeness emulation picker offers a different set of profiles than the API \
             publishes; add the missing variant to `EMULATION_CHOICES`"
        );
    }
}
