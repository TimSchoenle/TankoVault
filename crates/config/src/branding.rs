//! Product identity: the name, wordmark, copyright and links every rendered surface reads
//! instead of a literal.
//!
//! One block, read by `api` (which serves it to the SPA at `GET /v1/branding` and stamps it into
//! transactional email and the authenticator prompts), by `frontend` (which writes it into the
//! served app shell) and by `worker` (the identifiable crawler user-agent). A fork rebrands by
//! editing configuration; nothing here needs a rebuild.
//!
//! What is deliberately **not** here: the operating-system identifiers the desktop build
//! registers — the keyring service name, the Windows `AppUserModelID`, the autostart registry
//! value. Those must agree with what the installer wrote at build time, and a value that changed
//! under a running install would strand saved credentials and silence toasts rather than rebrand
//! anything. They live beside the build in `web/frontend/src/platform/`.

use serde::Deserialize;

/// The name a stock deployment answers to.
const DEFAULT_NAME: &str = "TankoVault";
/// The unaccented half of the shipped two-tone lockup.
const DEFAULT_WORDMARK_LEAD: &str = "Tankō";
/// The accented half.
const DEFAULT_WORDMARK_ACCENT: &str = "Vault";

/// Everything a deployment shows a reader about *itself*.
///
/// Every field defaults, so an absent `[branding]` section is valid and reproduces the shipped
/// identity exactly.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BrandingConfig {
    /// The product name, in prose: email subjects and bodies, page titles, the authenticator
    /// prompt, the desktop connect screen.
    pub name: String,
    /// The two-tone lockup drawn in the rail and the footer. See [`BrandingConfig::wordmark`]
    /// for what an unset lockup resolves to.
    pub wordmark: WordmarkConfig,
    /// One line under the wordmark. Unset keeps the shipped, *translated* tagline; setting it
    /// replaces that with this string in every language, which is the right trade for an
    /// operator whose product is not the one the catalogue describes.
    pub tagline: Option<String>,
    /// The notice in the footer's meta line.
    pub copyright: CopyrightConfig,
    /// How the deployment's own code is licensed, as shown in the footer.
    pub licence: LicenceConfig,
    /// Where the project lives — the footer's source link and the desktop About tab.
    pub project_url: String,
    /// Where a reader downloads the native client.
    pub releases_url: String,
    /// The identifiable crawler user-agent, applied to any provider still carrying the built-in
    /// default. A provider whose politeness names its own user-agent keeps it.
    pub bot_user_agent: Option<String>,
}

/// The two halves of the wordmark.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WordmarkConfig {
    /// The part drawn in the body colour.
    pub lead: Option<String>,
    /// The part drawn in the accent colour. Absent draws the lockup as one word.
    pub accent: Option<String>,
}

/// The footer's copyright line.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CopyrightConfig {
    /// Who holds it.
    pub holder: String,
    /// The year, or a range (`2024–2026`). Unset resolves to the current year at request time,
    /// so a deployment nobody has touched since December is not still claiming last year.
    pub year: Option<String>,
    /// The whole notice, verbatim, when the `© {year} {holder}` shape does not fit. Wins over
    /// both fields above and over the catalogue's translation of the line.
    pub notice: Option<String>,
}

/// The licence label, and where its text lives.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LicenceConfig {
    /// What the footer prints.
    pub name: String,
    /// Where the label links, when the operator publishes the text somewhere. Unset renders the
    /// label as plain text, which is what a self-hosted deployment with no public licence page
    /// should show rather than a dead link.
    pub url: Option<String>,
}

impl BrandingConfig {
    /// The wordmark to draw: `(lead, accent)`.
    ///
    /// An operator who set `name` and nothing else gets their own name as one word rather than
    /// this project's lockup with their product's name in the email — the shipped two-tone
    /// split is a property of the shipped name, not a template to pour any name into.
    #[must_use]
    pub fn wordmark(&self) -> (String, Option<String>) {
        if let Some(lead) = trimmed(self.wordmark.lead.as_deref()) {
            return (lead, trimmed(self.wordmark.accent.as_deref()));
        }
        if self.name == DEFAULT_NAME {
            return (
                DEFAULT_WORDMARK_LEAD.to_owned(),
                Some(DEFAULT_WORDMARK_ACCENT.to_owned()),
            );
        }
        (self.name.clone(), None)
    }
}

impl Default for BrandingConfig {
    fn default() -> Self {
        Self {
            name: DEFAULT_NAME.to_owned(),
            wordmark: WordmarkConfig::default(),
            tagline: None,
            copyright: CopyrightConfig::default(),
            licence: LicenceConfig::default(),
            project_url: "https://github.com/TimSchoenle/TankoVault".to_owned(),
            releases_url: "https://github.com/TimSchoenle/TankoVault/releases/latest".to_owned(),
            bot_user_agent: None,
        }
    }
}

impl Default for CopyrightConfig {
    fn default() -> Self {
        Self {
            holder: "Tim Schönle".to_owned(),
            year: None,
            notice: None,
        }
    }
}

impl Default for LicenceConfig {
    fn default() -> Self {
        Self {
            name: "PolyForm Noncommercial 1.0.0".to_owned(),
            url: None,
        }
    }
}

/// `Some(trimmed)` for a value with content, `None` for absent or blank.
///
/// Blank is treated as unset throughout this block: an environment variable set to the empty
/// string is how a deployment tool spells "I have nothing for this", and it must not print as an
/// empty wordmark.
fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_config_keeps_the_two_tone_lockup() {
        assert_eq!(
            BrandingConfig::default().wordmark(),
            ("Tankō".to_owned(), Some("Vault".to_owned()))
        );
    }

    /// The lockup must never mix names: a rename with no explicit split draws the new name
    /// whole, not the new name beside this project's accent half.
    #[test]
    fn a_renamed_product_draws_its_own_name_as_one_word() {
        let branding = BrandingConfig {
            name: "MangaBox".to_owned(),
            ..BrandingConfig::default()
        };
        assert_eq!(branding.wordmark(), ("MangaBox".to_owned(), None));
    }

    #[test]
    fn an_explicit_split_wins() {
        let branding = BrandingConfig {
            name: "MangaBox".to_owned(),
            wordmark: WordmarkConfig {
                lead: Some("Manga".to_owned()),
                accent: Some("Box".to_owned()),
            },
            ..BrandingConfig::default()
        };
        assert_eq!(
            branding.wordmark(),
            ("Manga".to_owned(), Some("Box".to_owned()))
        );
    }

    #[test]
    fn blank_values_read_as_unset() {
        let branding = BrandingConfig {
            wordmark: WordmarkConfig {
                lead: Some("  ".to_owned()),
                accent: Some(String::new()),
            },
            ..BrandingConfig::default()
        };
        assert_eq!(
            branding.wordmark(),
            ("Tankō".to_owned(), Some("Vault".to_owned()))
        );
    }
}
