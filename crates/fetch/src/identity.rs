//! The browser a request claims to be, parsed from a user-agent string.
//!
//! Exists because a solved challenge session is only usable by a client that can reproduce the
//! browser that earned it. The solver reports its user-agent and nothing else about itself, so
//! that string is the only description of the identity our own fetch stack has to match — see
//! `docs/CHALLENGE_HANDLING.md` §W5.
//!
//! Here rather than in `tankovault-domain`: [`BrowserEmulation`] is a stored and published type,
//! while a parsed user-agent is crawl mechanics with exactly one consumer, the client selection
//! in [`crate::base`].

use tankovault_domain::BrowserEmulation;

/// The operating system a user-agent claims.
///
/// Separate from [`BrowserEmulation`], which names a browser family and has historically implied
/// one platform per variant. A solver that randomises the platform it presents — Camoufox does,
/// per solve — makes the two independent facts they always were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BrowserPlatform {
    Windows,
    MacOs,
    Linux,
    Android,
    Ios,
}

/// A browser family, major version and claimed platform, together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BrowserIdentity {
    pub(crate) family: BrowserEmulation,
    pub(crate) major: u32,
    pub(crate) platform: BrowserPlatform,
}

impl BrowserIdentity {
    /// Parse the identity a user-agent claims, or `None` when it names no browser we can
    /// impersonate.
    ///
    /// `None` is a usable answer, not a failure: the caller's correct response is to decline to
    /// present that user-agent at all rather than pair it with a handshake belonging to some other
    /// browser.
    #[must_use]
    pub(crate) fn from_user_agent(ua: &str) -> Option<Self> {
        let family = family_of(ua)?;
        Some(Self {
            family,
            major: major_of(ua, family)?,
            platform: platform_of(ua),
        })
    }
}

/// Order matters: every Chromium user-agent also carries `Safari/537.36`, and Edge carries
/// `Chrome/` as well as `Edg/`, so the most specific claim has to be tested first.
fn family_of(ua: &str) -> Option<BrowserEmulation> {
    if ua.starts_with("okhttp/") {
        return Some(BrowserEmulation::OkHttp);
    }
    if ua.contains("Edg/") {
        return Some(BrowserEmulation::Edge);
    }
    if ua.contains("Firefox/") || ua.contains("Gecko/") {
        return Some(BrowserEmulation::Firefox);
    }
    if ua.contains("Chrome/") || ua.contains("Chromium/") {
        return Some(BrowserEmulation::Chrome);
    }
    if ua.contains("Safari/") && ua.contains("Version/") {
        return Some(BrowserEmulation::Safari);
    }
    None
}

fn major_of(ua: &str, family: BrowserEmulation) -> Option<u32> {
    // Firefox carries its real version twice — `rv:150.0` and `Firefox/150.0` — and Camoufox
    // keeps both in step. `Version/` is Safari's, and is the *browser* version there while
    // `Safari/` is the WebKit build, which is not what identifies the release.
    let token = match family {
        BrowserEmulation::Chrome => "Chrome/",
        BrowserEmulation::Firefox => "Firefox/",
        BrowserEmulation::Safari => "Version/",
        BrowserEmulation::Edge => "Edg/",
        BrowserEmulation::OkHttp => "okhttp/",
    };
    let rest = ua.split(token).nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Android before Linux, and iOS before macOS: an Android user-agent also says `Linux`, and an
/// iPhone's also says `like Mac OS X`.
fn platform_of(ua: &str) -> BrowserPlatform {
    if ua.contains("Android") {
        BrowserPlatform::Android
    } else if ua.contains("iPhone") || ua.contains("iPad") || ua.contains("iPod") {
        BrowserPlatform::Ios
    } else if ua.contains("Windows") {
        BrowserPlatform::Windows
    } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
        BrowserPlatform::MacOs
    } else {
        BrowserPlatform::Linux
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two user-agents the deployed solver actually returned, captured 2026-08-12 from
    /// `ghcr.io/germondai/trawl:1.3.1`.
    ///
    /// They are the whole reason this module exists, and they carry two facts that a static
    /// configuration constant cannot express. The solver is a **Firefox** (Camoufox) while every
    /// provider defaults to a Chrome emulation profile; and it **randomises the platform it
    /// presents per solve**, so consecutive solves against the same URL returned Windows and macOS.
    /// A session's identity therefore has to be read off the session, never assumed.
    #[test]
    fn the_solvers_own_user_agents_parse_to_firefox_on_two_platforms() {
        let windows = BrowserIdentity::from_user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:150.0) Gecko/20100101 Firefox/150.0",
        )
        .expect("a Firefox user-agent parses");
        assert_eq!(windows.family, BrowserEmulation::Firefox);
        assert_eq!(windows.major, 150);
        assert_eq!(windows.platform, BrowserPlatform::Windows);

        let macos = BrowserIdentity::from_user_agent(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:150.0) Gecko/20100101 Firefox/150.0",
        )
        .expect("a Firefox user-agent parses");
        assert_eq!(macos.platform, BrowserPlatform::MacOs);
        assert_eq!(
            (macos.family, macos.major),
            (windows.family, windows.major),
            "only the platform rotates"
        );
    }

    /// Every Chromium user-agent ends in `Safari/537.36`, and Edge's also contains `Chrome/`.
    /// Testing in the wrong order reports Chrome as Safari and Edge as Chrome — a mismatch that
    /// would then be presented to a provider as a deliberate disguise.
    #[test]
    fn chromium_family_user_agents_are_not_confused_for_one_another() {
        let chrome = BrowserIdentity::from_user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/149.0.0.0 Safari/537.36",
        )
        .expect("parses");
        assert_eq!(
            (chrome.family, chrome.major),
            (BrowserEmulation::Chrome, 149)
        );

        let edge = BrowserIdentity::from_user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/148.0.0.0 Safari/537.36 Edg/148.0.2903.51",
        )
        .expect("parses");
        assert_eq!((edge.family, edge.major), (BrowserEmulation::Edge, 148));

        let safari = BrowserIdentity::from_user_agent(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/26.4 Safari/605.1.15",
        )
        .expect("parses");
        assert_eq!(
            (safari.family, safari.major, safari.platform),
            (BrowserEmulation::Safari, 26, BrowserPlatform::MacOs)
        );
    }

    #[test]
    fn mobile_platforms_win_over_the_desktop_words_they_contain() {
        let android = BrowserIdentity::from_user_agent("okhttp/5.0.0").expect("parses");
        assert_eq!(
            (android.family, android.major),
            (BrowserEmulation::OkHttp, 5)
        );

        // An Android user-agent also says "Linux"; an iPhone's also says "like Mac OS X".
        let chrome_android = BrowserIdentity::from_user_agent(
            "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/149.0.0.0 Mobile Safari/537.36",
        )
        .expect("parses");
        assert_eq!(chrome_android.platform, BrowserPlatform::Android);

        let ios = BrowserIdentity::from_user_agent(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1",
        )
        .expect("parses");
        assert_eq!(ios.platform, BrowserPlatform::Ios);
    }

    #[test]
    fn a_user_agent_naming_no_browser_we_can_impersonate_is_none() {
        assert_eq!(BrowserIdentity::from_user_agent(""), None);
        assert_eq!(BrowserIdentity::from_user_agent("TankoVaultBot/0.1"), None);
        assert_eq!(BrowserIdentity::from_user_agent("curl/8.7.1"), None);
        // A family we recognise but a version we cannot read is still unusable: the profile is
        // chosen by major version, so "some Firefox" does not identify a handshake.
        assert_eq!(
            BrowserIdentity::from_user_agent("Mozilla/5.0 Firefox/nightly"),
            None
        );
    }
}
