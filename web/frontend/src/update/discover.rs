//! Finding a release, and proving it is ours before a single installer byte is read.
//!
//! **The signature is the gate, and it is checked before anything else is trusted.** The release
//! workflow writes `desktop-manifest.json` — the version, and one entry per platform naming the
//! installer with its SHA-256 — and signs it with minisign. Nothing here believes a file name, a
//! length or a digest that did not arrive inside a manifest which verified against a key compiled
//! into this binary. That is what reduces "download and run an executable off the internet" to
//! "run what the holder of the release key published": the transport is TLS to GitHub, but TLS
//! only says who served the bytes, not who made them.
//!
//! The keys are a **list** on purpose. Rotating a signing key otherwise means every installed
//! client stops updating the moment the signer changes — the new signature verifies against a key
//! they do not have, and the only way out is a manual download. With a list it is a two-release
//! move: publish a client trusting `[old, new]`, then switch the signer on the release after.
//! `docs/RELEASING.md` carries the procedure.
//!
//! Errors are catalogue keys under `settings.update.error.`, abbreviated to `…` below; see
//! [`super`].

use serde::Deserialize;
use std::collections::BTreeMap;

/// The repository releases are read from.
///
/// A constant, not configuration. It decides where an executable this app will *run* comes from,
/// and a settings file is writable by anything running as the reader — so making it a preference
/// would hand that decision to the weakest thing in the chain. A fork changes this line and its
/// own keys below.
const REPO: &str = "TimSchoenle/TankoVault";

/// The minisign public keys a manifest may be signed with.
///
/// A **list**, never a single entry to be replaced in place: shipped clients only ever trust what
/// they were compiled with, so switching the signer in one step strands every installed reader on
/// the version they have. Rotation is therefore two releases — publish a client trusting
/// `[old, new]`, then switch `MINISIGN_SECRET_KEY` on the release after. `docs/RELEASING.md` has
/// the procedure and the order.
///
/// Empty would switch the updater off entirely ([`super::is_configured`] answers `false` and no
/// request is made to GitHub), which is how this shipped before a key existed.
///
/// The *key* line only — the `RW…` base64, not the `untrusted comment:` line above it in the
/// `.pub` file. A malformed entry is not a compile error and not a runtime error either: it simply
/// never verifies anything, so the updater would refuse every release as untrusted. The test below
/// is what stops that reaching a release.
const TRUSTED_KEYS: &[&str] = &[
    // Generated 2026-08-07. Its private half is the `release` environment's
    // `MINISIGN_SECRET_KEY`, and `desktop-release` fails the release if the two disagree.
    "RWRJbPWpabBZ+C+5MBbE04xjL6HFoNsBZLbqqWogP7sD5BedsiJDJ4Ve",
];

/// The manifest asset, and its detached minisign signature.
const MANIFEST_ASSET: &str = "desktop-manifest.json";
const SIGNATURE_ASSET: &str = "desktop-manifest.json.minisig";

/// How many releases back to look. Enough that a long-held-back client still finds a version it
/// may take, small enough to stay one page.
const RELEASE_PAGE_SIZE: u32 = 30;

/// A day, in milliseconds — the unit the hold-back window is expressed in.
const DAY_MS: f64 = 86_400_000.0;

/// Ceiling on the two metadata requests. The installer download has none: it is on the order of a
/// hundred megabytes and a reader on a slow line is not a failure.
const METADATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// One release, as much of GitHub's payload as this app reads.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Release {
    tag_name: String,
    html_url: String,
    /// When the release stopped being a draft — which for this repository is when its installers
    /// became reachable, since `desktop-release` publishes the draft only after attaching them.
    /// That makes it the right clock for the hold-back window.
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// One attached file.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Asset {
    name: String,
    browser_download_url: String,
}

/// A release this installation may move to.
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    /// The release's version, without the tag's `v`.
    pub(crate) version: semver::Version,
    /// Where a reader is sent when this app cannot install the release itself.
    pub(crate) page: String,
    /// The manifest and its signature, or `None` for a release published before the desktop
    /// manifest existed. Such a release is announced and never installed — see
    /// [`Candidate::is_installable`].
    signed: Option<Signed>,
    assets: Vec<Asset>,
}

/// The two URLs an installable release is proved by.
#[derive(Debug, Clone)]
struct Signed {
    manifest: String,
    signature: String,
}

impl Candidate {
    /// Whether this release carries the signed manifest an unattended install needs.
    pub(crate) fn is_installable(&self) -> bool {
        self.signed.is_some()
    }

    /// The download URL of the asset named `file`, if the release actually carries it.
    ///
    /// A manifest entry is a claim about the release; this is the check that the claim matches the
    /// asset list, so a manifest naming a file nobody attached fails here rather than at a 404
    /// halfway through a download.
    pub(crate) fn asset_url(&self, file: &str) -> Option<&str> {
        self.assets
            .iter()
            .find(|asset| asset.name == file)
            .map(|asset| asset.browser_download_url.as_str())
    }
}

/// The document the release signs: which version, and one entry per platform.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Manifest {
    pub(crate) version: String,
    pub(crate) targets: BTreeMap<String, Target>,
}

/// One platform's installer, as the manifest describes it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Target {
    pub(crate) file: String,
    /// Lower-case hex SHA-256. Every downloaded byte is held to this.
    pub(crate) sha256: String,
    pub(crate) size: u64,
}

/// Whether this build carries a key to verify a manifest with.
pub(crate) fn is_configured() -> bool {
    !TRUSTED_KEYS.is_empty()
}

/// Fetch the recent releases.
///
/// # Errors
/// `…network` for a refused request, a non-success status, or a body that is not the release
/// list.
pub(crate) async fn releases(client: &reqwest::Client) -> Result<Vec<Release>, &'static str> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page={RELEASE_PAGE_SIZE}");
    let response = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .timeout(METADATA_TIMEOUT)
        .send()
        .await
        .map_err(|_| "settings.update.error.network")?;
    if !response.status().is_success() {
        return Err("settings.update.error.network");
    }
    response
        .json::<Vec<Release>>()
        .await
        .map_err(|_| "settings.update.error.network")
}

/// The newest release this installation may move to, or `None`.
///
/// Three conditions, all of which have to hold:
///
/// * it parses as a semver release — no draft, no prerelease, no `-rc` in the tag;
/// * it is **newer than `current`**;
/// * it is **at least `min_age_days` old**.
///
/// The last two together are why this is not `releases.first()`. With a seven-day hold-back, a
/// 2.1.0 published yesterday is not offered while a 2.0.5 published a month ago still is — the
/// point of the window is that a release pulled within days of publication is never installed, and
/// that only works if the *next best* release is still reachable.
///
/// A release whose `published_at` cannot be read is skipped rather than treated as old enough. The
/// window is the reader's protection and an unreadable timestamp cannot honour it; refusing to
/// offer a release is recoverable, installing one that should have been held back is not.
pub(crate) fn eligible(
    releases: &[Release],
    current: &semver::Version,
    min_age_days: u32,
    now_ms: f64,
) -> Option<Candidate> {
    let hold_back_ms = f64::from(min_age_days) * DAY_MS;
    releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            let version = version_of(release)?;
            if version <= *current {
                return None;
            }
            let published = release.published_at.as_deref()?;
            let published_ms = crate::platform::parse_timestamp_ms(published);
            if !published_ms.is_finite() || now_ms - published_ms < hold_back_ms {
                return None;
            }
            Some(Candidate {
                version,
                page: release.html_url.clone(),
                signed: signed_urls(release),
                assets: release.assets.clone(),
            })
        })
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// A release's version, or `None` if the tag is not a plain `vX.Y.Z`.
///
/// Prerelease and build metadata are rejected rather than compared. `release-please` cuts plain
/// semver on this repository, so anything else is a hand-made tag — and a hand-made tag is not
/// something to push at every installed client.
fn version_of(release: &Release) -> Option<semver::Version> {
    let text = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let version = semver::Version::parse(text).ok()?;
    (version.pre.is_empty() && version.build.is_empty()).then_some(version)
}

/// The manifest and signature URLs, if the release carries both.
fn signed_urls(release: &Release) -> Option<Signed> {
    let find = |name: &str| {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.clone())
    };
    Some(Signed {
        manifest: find(MANIFEST_ASSET)?,
        signature: find(SIGNATURE_ASSET)?,
    })
}

/// Download `candidate`'s manifest, verify its signature, and return it with the bytes that were
/// signed.
///
/// The bytes come back alongside the parsed document because they are what a later start has to
/// re-verify: [`super::install`] stores them next to the staged installer and checks the signature
/// again before executing anything, rather than trusting that what it wrote is what it reads.
///
/// # Errors
/// `…network` if either file cannot be fetched, `…unconfigured` with no trusted key,
/// `…untrusted` if no key verifies the manifest, `…manifest` if it verifies but does not parse
/// or describes another release.
pub(crate) async fn manifest(
    client: &reqwest::Client,
    candidate: &Candidate,
) -> Result<(Manifest, Vec<u8>, String), &'static str> {
    let signed = candidate
        .signed
        .as_ref()
        .ok_or("settings.update.error.manifest")?;
    let bytes = fetch_bytes(client, &signed.manifest).await?;
    let signature = String::from_utf8(fetch_bytes(client, &signed.signature).await?)
        .map_err(|_| "settings.update.error.untrusted")?;

    verify(&bytes, &signature, TRUSTED_KEYS)?;
    let manifest = parse(&bytes)?;
    // A verified manifest still has to be *this* release's. Both are published by the same job so
    // a mismatch means a mixed-up upload, not an attack — but the failure it would otherwise
    // produce is a download that mysteriously never matches its digest.
    if semver::Version::parse(&manifest.version).ok().as_ref() != Some(&candidate.version) {
        return Err("settings.update.error.manifest");
    }
    Ok((manifest, bytes, signature))
}

/// Verify `signature` over `bytes` against any of `keys`.
///
/// `allow_legacy: false` — the release signs with prehashing (`minisign -S -H`), and a legacy
/// signature is one made by a much older minisign than the workflow installs. Accepting both would
/// widen what verifies for no reason anyone here needs.
///
/// # Errors
/// `…unconfigured` when `keys` is empty, `…untrusted` when no key verifies — including a
/// malformed signature or key, which are not worth telling apart for a reader.
fn verify(bytes: &[u8], signature: &str, keys: &[&str]) -> Result<(), &'static str> {
    if keys.is_empty() {
        return Err("settings.update.error.unconfigured");
    }
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|_| "settings.update.error.untrusted")?;
    let verified = keys.iter().any(|key| {
        minisign_verify::PublicKey::from_base64(key)
            .is_ok_and(|key| key.verify(bytes, &signature, false).is_ok())
    });
    if verified {
        Ok(())
    } else {
        Err("settings.update.error.untrusted")
    }
}

/// Verify a manifest against the keys this build trusts. See [`verify`].
///
/// # Errors
/// As [`verify`].
pub(crate) fn verify_trusted(bytes: &[u8], signature: &str) -> Result<(), &'static str> {
    verify(bytes, signature, TRUSTED_KEYS)
}

/// Parse a manifest whose signature has already been checked.
///
/// # Errors
/// `…manifest` for anything that is not the document the release workflow writes.
pub(crate) fn parse(bytes: &[u8]) -> Result<Manifest, &'static str> {
    serde_json::from_slice(bytes).map_err(|_| "settings.update.error.manifest")
}

/// The manifest key for the platform this build is running on.
///
/// Composed from the host triple's own words rather than a `cfg` ladder, so a build for an
/// architecture no release publishes — an `aarch64` Windows client, say — asks for a key the
/// manifest does not have, and reports that instead of installing an x86 build.
pub(crate) fn target_key(kind: &str) -> String {
    format!(
        "{}-{}-{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        kind
    )
}

/// GET `url` and return the body.
async fn fetch_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, &'static str> {
    let response = client
        .get(url)
        .timeout(METADATA_TIMEOUT)
        .send()
        .await
        .map_err(|_| "settings.update.error.network")?;
    if !response.status().is_success() {
        return Err("settings.update.error.network");
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|_| "settings.update.error.network")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A published minisign test vector — the key pair is `rust-minisign-verify`'s own, and the
    /// payload it signs is the four bytes `test`. Used here because verification is the only half
    /// of minisign this crate carries: there is no signer to generate a fixture with, and a
    /// hand-written signature would only ever prove that verification fails.
    const FIXTURE_KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const FIXTURE_SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=
trusted comment: timestamp:1556193335\tfile:test
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";
    const FIXTURE_PAYLOAD: &[u8] = b"test";

    fn release(tag: &str, published_at: Option<&str>, assets: &[&str]) -> Release {
        Release {
            tag_name: tag.to_owned(),
            html_url: format!("https://example.invalid/{tag}"),
            published_at: published_at.map(str::to_owned),
            draft: false,
            prerelease: false,
            assets: assets
                .iter()
                .map(|name| Asset {
                    name: (*name).to_owned(),
                    browser_download_url: format!("https://example.invalid/{tag}/{name}"),
                })
                .collect(),
        }
    }

    /// Epoch milliseconds for a day offset from a fixed "now", so the fixtures read as ages.
    fn days_ago(now_ms: f64, days: f64) -> String {
        let ms = now_ms - days * DAY_MS;
        crate::platform::format_timestamp_iso(ms).expect("a representable instant")
    }

    const NOW_MS: f64 = 1_800_000_000_000.0;

    fn signed(tag: &str, days: f64) -> Release {
        release(
            tag,
            Some(&days_ago(NOW_MS, days)),
            &[MANIFEST_ASSET, SIGNATURE_ASSET],
        )
    }

    /// The hold-back window has to reach *past* the newest release, not merely delay it.
    ///
    /// The first version of this returned the latest release and then refused it when it was too
    /// young, which made a non-zero hold-back mean "never update until the next release lands"
    /// rather than "stay one release behind". A reader on 2.0.0 with a 7-day window and a 2.1.0
    /// published yesterday must still be offered 2.0.5.
    #[test]
    fn the_window_falls_back_to_the_newest_release_old_enough() {
        let releases = [signed("v2.1.0", 1.0), signed("v2.0.5", 30.0)];
        let current = semver::Version::new(2, 0, 0);

        let held = eligible(&releases, &current, 7, NOW_MS).expect("2.0.5 has cleared the window");
        assert_eq!(held.version, semver::Version::new(2, 0, 5));

        let immediate = eligible(&releases, &current, 0, NOW_MS).expect("no window, take the top");
        assert_eq!(immediate.version, semver::Version::new(2, 1, 0));
    }

    #[test]
    fn a_release_no_newer_than_the_running_build_is_not_offered() {
        let releases = [signed("v2.0.0", 30.0)];
        assert!(eligible(&releases, &semver::Version::new(2, 0, 0), 0, NOW_MS).is_none());
        assert!(eligible(&releases, &semver::Version::new(2, 1, 0), 0, NOW_MS).is_none());
    }

    /// Drafts, prereleases and hand-made tags are all invisible to the updater.
    ///
    /// `release-please` cuts plain `vX.Y.Z` on this repository, so anything else was made by hand
    /// — and a hand-made tag is not something to push at every installed client.
    #[test]
    fn only_plain_published_semver_releases_are_candidates() {
        let mut draft = signed("v3.0.0", 30.0);
        draft.draft = true;
        let mut pre = signed("v3.0.1", 30.0);
        pre.prerelease = true;
        let releases = [
            draft,
            pre,
            signed("v3.0.2-rc.1", 30.0),
            signed("nightly", 30.0),
        ];
        assert!(eligible(&releases, &semver::Version::new(2, 0, 0), 0, NOW_MS).is_none());
    }

    /// A timestamp that cannot be read means the window cannot be honoured, so the release is
    /// skipped rather than assumed old enough — refusing an update is recoverable, installing one
    /// that should have been held back is not.
    #[test]
    fn an_unreadable_publication_date_is_not_treated_as_old() {
        let releases = [
            release("v2.1.0", None, &[MANIFEST_ASSET, SIGNATURE_ASSET]),
            release(
                "v2.0.9",
                Some("not a timestamp"),
                &[MANIFEST_ASSET, SIGNATURE_ASSET],
            ),
        ];
        assert!(eligible(&releases, &semver::Version::new(2, 0, 0), 0, NOW_MS).is_none());
    }

    /// A release cut before the manifest existed is announced, never installed: there is nothing
    /// to verify it against, and this app does not run an installer it cannot prove the origin of.
    #[test]
    fn a_release_without_a_manifest_is_not_installable() {
        let releases = [release(
            "v2.1.0",
            Some(&days_ago(NOW_MS, 30.0)),
            &["Tankovault_2.1.0_x64_en-US.msi"],
        )];
        let candidate = eligible(&releases, &semver::Version::new(2, 0, 0), 0, NOW_MS)
            .expect("still a newer version");
        assert!(!candidate.is_installable());
    }

    #[test]
    fn a_manifest_entry_naming_an_unattached_file_resolves_to_no_url() {
        let releases = [release(
            "v2.1.0",
            Some(&days_ago(NOW_MS, 30.0)),
            &[MANIFEST_ASSET, SIGNATURE_ASSET, "real.msi"],
        )];
        let candidate =
            eligible(&releases, &semver::Version::new(2, 0, 0), 0, NOW_MS).expect("a candidate");
        assert!(candidate.asset_url("real.msi").is_some());
        assert!(candidate.asset_url("invented.msi").is_none());
    }

    #[test]
    fn the_fixture_signature_verifies_against_its_own_key() {
        assert!(verify(FIXTURE_PAYLOAD, FIXTURE_SIGNATURE, &[FIXTURE_KEY]).is_ok());
    }

    /// One changed byte must fail. This is the whole security property of the module: everything
    /// downstream — the file to fetch, its length, its digest — is read out of these bytes.
    #[test]
    fn a_tampered_payload_fails_verification() {
        assert_eq!(
            verify(b"Test", FIXTURE_SIGNATURE, &[FIXTURE_KEY]),
            Err("settings.update.error.untrusted")
        );
    }

    /// A signature made by a key this build does not carry is refused, which is what makes the
    /// trusted list a list of *permitted* signers rather than a hint.
    #[test]
    fn an_untrusted_key_is_refused() {
        const OTHER: &str = "RWSmKaOrf6m3xrbLIL9EqiKMTiOoV1AGXjKGdcexNbcqNRQNe3TAkI3P";
        assert_eq!(
            verify(FIXTURE_PAYLOAD, FIXTURE_SIGNATURE, &[OTHER]),
            Err("settings.update.error.untrusted")
        );
    }

    /// With no key compiled in there is nothing that could establish provenance, so verification
    /// fails closed rather than open. This is what made shipping an empty `TRUSTED_KEYS` safe.
    #[test]
    fn an_empty_key_list_verifies_nothing() {
        assert_eq!(
            verify(FIXTURE_PAYLOAD, FIXTURE_SIGNATURE, &[]),
            Err("settings.update.error.unconfigured")
        );
    }

    /// Every shipped key has to parse as a minisign public key.
    ///
    /// A malformed one fails **silently**: [`verify`] tries each key with `is_ok_and`, so an entry
    /// that cannot be decoded simply never matches, and the updater reports every release as
    /// untrusted — a broken update channel that no compiler, and no other test here, would notice.
    /// A truncated paste or an accidentally included `untrusted comment:` line is exactly the way
    /// that happens.
    #[test]
    fn every_trusted_key_is_a_well_formed_minisign_key() {
        assert!(
            is_configured(),
            "this build ships a signing key, so the updater is live"
        );
        for key in TRUSTED_KEYS {
            assert!(
                minisign_verify::PublicKey::from_base64(key).is_ok(),
                "TRUSTED_KEYS entry does not decode as a minisign public key: {key}"
            );
        }
    }

    #[test]
    fn a_manifest_parses_into_its_targets() {
        let document = br#"{
            "version": "2.1.0",
            "targets": {
                "windows-x86_64-msi": { "file": "T.msi", "sha256": "ab", "size": 12 }
            }
        }"#;
        let manifest = parse(document).expect("the document the workflow writes");
        assert_eq!(manifest.version, "2.1.0");
        let target = manifest
            .targets
            .get("windows-x86_64-msi")
            .expect("the entry");
        assert_eq!(target.file, "T.msi");
        assert_eq!(target.size, 12);
    }

    #[test]
    fn a_target_key_names_the_host_platform() {
        let key = target_key("msi");
        assert!(key.ends_with("-msi"), "{key}");
        assert!(key.contains(std::env::consts::ARCH), "{key}");
    }
}
