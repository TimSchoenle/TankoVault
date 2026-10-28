//! Owns the presets on the Keyoapp platform.

use super::{BuiltinPreset, KEYOAPP, plain};

/// Returns the providers on the Keyoapp platform: hosted, so every install is the stock layout and
/// each site is identity alone.
pub(super) fn keyoapp_family() -> Vec<BuiltinPreset> {
    // `sirenscans` is deliberately absent: its origin answers a bare nginx `404` on every route,
    // `/` included, through a fully solved real browser, while a sibling install answers the same
    // request from the same session normally. That is the origin being gone, not a block and not
    // the flap the whole family is prone to. A preset kept for a dead host spends a crawl budget
    // and fills the console's error feed every cycle; `retire_missing` drops the definition and
    // deliberately leaves any installed provider row alone for the operator to pause.
    vec![
        plain(
            "asmotoon",
            "Asmodeus Scans",
            "https://asmotoon.com",
            KEYOAPP,
        ),
        plain("genztoons", "Genz Toons", "https://genztoons.org", KEYOAPP),
    ]
}
