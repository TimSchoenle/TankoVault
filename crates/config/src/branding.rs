//! Product identity: the name, wordmark, copyright and links every rendered surface reads
//! instead of a literal.
//!
//! `api` serves the block to the SPA at `GET /v1/branding` and stamps it into transactional email
//! and the authenticator prompts. `frontend` writes it into the served app shell, and `worker`
//! builds the crawler user-agent from it. A fork rebrands through configuration, with no rebuild.
//!
//! The operating-system identifiers the desktop build registers are not here: the keyring service
//! name, the Windows `AppUserModelID` and the autostart registry value. They must match what the
//! installer wrote, since a change under a running install strands saved credentials and silences
//! toasts. They live in `web/frontend/src/platform/`.

use serde::Deserialize;
use terrace_config::schema::Describe;

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
#[derive(Debug, Clone, Deserialize, Describe)]
#[serde(default)]
pub struct BrandingConfig {
    /// The product name as prose uses it: email subjects and bodies, page titles, the
    /// authenticator prompt and the desktop connect screen.
    pub name: String,
    /// The two-tone lockup drawn in the rail and the footer. Unset, it is the product name as one
    /// word, or the shipped lockup under the shipped name.
    #[config(nested)]
    pub wordmark: WordmarkConfig,
    /// One line under the wordmark. Unset keeps the shipped, translated tagline. Set, it replaces
    /// the tagline with this string in every language.
    pub tagline: Option<String>,
    /// The notice in the footer's meta line.
    #[config(nested)]
    pub copyright: CopyrightConfig,
    /// How the deployment's own code is licensed, as shown in the footer.
    #[config(nested)]
    pub licence: LicenceConfig,
    /// Where the project lives, linked from the footer and the desktop About tab.
    pub project_url: String,
    /// Where a reader downloads the native client.
    pub releases_url: String,
    /// The identifiable crawler user-agent, applied to any provider still carrying the built-in
    /// default. A provider whose politeness names its own user-agent keeps it.
    pub bot_user_agent: Option<String>,
}

/// The two halves of the wordmark.
#[derive(Debug, Clone, Default, Deserialize, Describe)]
#[serde(default)]
pub struct WordmarkConfig {
    /// The part drawn in the body colour.
    pub lead: Option<String>,
    /// The part drawn in the accent colour. Absent draws the lockup as one word.
    pub accent: Option<String>,
}

/// The footer's copyright line.
#[derive(Debug, Clone, Deserialize, Describe)]
#[serde(default)]
pub struct CopyrightConfig {
    /// Who holds it.
    pub holder: String,
    /// The year, or a range (`2024–2026`). Unset is the current year at request time.
    pub year: Option<String>,
    /// The whole notice, verbatim, when the `© {year} {holder}` shape does not fit. Wins over
    /// both fields above and over the catalogue's translation of the line.
    pub notice: Option<String>,
}

/// The licence label, and where its text lives.
#[derive(Debug, Clone, Deserialize, Describe)]
#[serde(default)]
pub struct LicenceConfig {
    /// What the footer prints.
    pub name: String,
    /// Where the label links. Unset renders the label as plain text, for a deployment with no
    /// public licence page.
    pub url: Option<String>,
}

impl BrandingConfig {
    /// Returns the wordmark to draw as `(lead, accent)`.
    ///
    /// An explicit lead wins. Otherwise the shipped name keeps its two-tone lockup, and any
    /// other name is drawn whole as one word.
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

/// Returns the trimmed value, or `None` for an absent or blank one.
///
/// Blank means unset throughout this block, because a deployment tool with nothing to supply
/// sets the variable to the empty string.
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

    /// A rename with no explicit split draws the new name whole, never beside the shipped accent
    /// half.
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
