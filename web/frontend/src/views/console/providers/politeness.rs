//! The politeness editor's emulation vocabulary, and the body its Save submits.

use crate::models::PolitenessInput;
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
pub(super) fn emulation_token(stored: Option<&BrowserEmulation>) -> String {
    stored.map(ToString::to_string).unwrap_or_default()
}

/// Build the politeness body from the editor's fields, or the catalogue key of the field that
/// would not parse.
///
/// `emulation` is the wire token of a browser profile, or empty for "no emulation".
///
/// # Errors
///
/// The catalogue key naming the first of `rps`, `concurrency`, `crawl_delay_ms` or `emulation`
/// that the server would not accept.
pub(super) fn politeness_body(
    rps: &str,
    concurrency: &str,
    crawl_delay_ms: &str,
    user_agent: &str,
    emulation: &str,
) -> Result<PolitenessInput, &'static str> {
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
    Ok(PolitenessInput {
        rps: Some(rps),
        concurrency: Some(concurrency),
        crawl_delay_ms: Some(crawl_delay_ms),
        user_agent: Some(user_agent.to_owned()),
        emulation: emulation_field(emulation)?,
    })
}

/// The `emulation` field: a profile, or `None` for "no emulation".
///
/// `None` leaves the key out of the request, and `PolitenessInput` is the schema that makes that
/// mean what it says — the stored `Politeness` reads an absent `emulation` as Chrome, which is
/// the right answer for a provider row written before the field existed and the wrong one for a
/// request. Sending the two shapes at the same schema is what made this silently unfixable.
///
/// # Errors
///
/// `console.providers.badEmulation` for a token no profile answers to. Not degraded to `None`:
/// an unrecognised token can only be a bug in the picker above, and refusing the save names it,
/// where a quiet `None` would turn emulation off and look like it worked.
fn emulation_field(token: &str) -> Result<Option<BrowserEmulation>, &'static str> {
    if token.is_empty() {
        return Ok(None);
    }
    token
        .parse::<BrowserEmulation>()
        .map(Some)
        .map_err(|_| "console.providers.badEmulation")
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
    /// Twice, for the same reason wearing two shapes. The editor first built the payload as a
    /// `serde_json::Value` carrying an explicit `"emulation": null` and round-tripped it through
    /// the generated struct, whose `skip_serializing_if = "Option::is_none"` dropped the key from
    /// the request entirely. The workaround that replaced it — holding `Value::Null` in the
    /// raw-JSON arm of the generated untagged nullable `$ref` — worked only because that arm
    /// swallowed every such field, which was itself the bug it stood on.
    ///
    /// Both are gone because the server no longer reads an absent key as Chrome on this path:
    /// `PolitenessInput` is a request-only schema whose absent `emulation` means no emulation, so
    /// the key the generated client omits is the key the server wants omitted. Either way the
    /// symptom was the same — the provider went back to impersonating Chrome, the save reported
    /// success, and the picker still read "no emulation" until the next fetch.
    #[test]
    fn choosing_no_emulation_omits_the_key() {
        assert!(
            body("")
                .as_object()
                .is_some_and(|o| !o.contains_key("emulation")),
            "the key must be absent — that is how this request shape spells \"no emulation\""
        );
    }

    /// A chosen profile still goes out as its own wire token.
    #[test]
    fn a_chosen_profile_is_sent_as_its_token() {
        assert_eq!(body("ok_http")["emulation"], serde_json::json!("ok_http"));
    }

    /// An unrecognised token fails the save and names itself. Mapping it to "no emulation"
    /// instead would silently disable emulation on a save that looked like it worked.
    #[test]
    fn an_unrecognised_token_is_not_quietly_turned_into_no_emulation() {
        assert_eq!(
            politeness_body("1", "4", "250", "", "netscape").unwrap_err(),
            "console.providers.badEmulation"
        );
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

    /// A provider with no stored emulation seeds the "no emulation" option, not the first one.
    #[test]
    fn no_stored_emulation_seeds_the_no_emulation_option() {
        assert_eq!(emulation_token(None), "");
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
