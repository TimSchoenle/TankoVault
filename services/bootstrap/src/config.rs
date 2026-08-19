//! What the `bootstrap` binary reads from its configuration.
//!
//! Public, and in the library rather than beside `main`, because it is the root
//! `config-contract` describes for this image: the contract has to be generated from the very
//! type the binary deserialises, or it is a claim about something else.

use secrecy::SecretString;
use serde::Deserialize;
use tankovault_config::DatabaseConfig;
use terrace_config::schema::Describe;

#[derive(Debug, Deserialize, Describe)]
pub struct Config {
    #[config(nested)]
    pub database: DatabaseConfig,
    #[serde(default)]
    #[config(nested)]
    pub auth: AuthConfig,
    #[serde(default = "default_admin_email")]
    pub seed_admin_email: String,
    #[serde(default = "default_admin_username")]
    pub seed_admin_username: String,
    /// No default, unlike `xtask seed`: a published image must not be able to create an account
    /// whose password is written down in this repository.
    #[config(secret)]
    pub seed_admin_password: Option<SecretString>,
}

/// The one `auth` key this binary needs. Deliberately not
/// [`tankovault_api::config::AuthConfig`](../../api/src/config.rs), which requires the JWT
/// secret a migration job has no business holding.
#[derive(Debug, Default, Deserialize, Describe)]
pub struct AuthConfig {
    /// Server-side password pepper: mixed into every argon2id hash so a leak alone can't be
    /// brute-forced offline. Empty (default) is un-peppered, for compatibility with old
    /// hashes; once configured it must stay stable or passwords stop verifying.
    //
    // Word for word the API's, and a `//` rather than a `///` on purpose: this is the same value
    // — the seed writes a hash the API then verifies, and a deployment that gave the two
    // different peppers has an admin account whose correct password is refused with nothing in
    // the logs naming the cause. The published contracts carry the whole `///` comment, and a
    // consumer merging two images that read one key requires the descriptions to agree, so the
    // rationale for the duplication must not itself become part of the description.
    #[config(secret)]
    pub password_pepper: Option<SecretString>,
}

fn default_admin_email() -> String {
    "admin@tankovault.local".to_owned()
}

fn default_admin_username() -> String {
    "admin".to_owned()
}
