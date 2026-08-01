//! The set of external providers this service knows about, keyed by slug.

use std::collections::HashMap;

use tankovault_contracts::sync::ProviderInfo;

use crate::provider::ExternalProvider;

/// Every registered provider, keyed by slug.
pub(crate) struct ProviderRegistry {
    providers: HashMap<&'static str, Box<dyn ExternalProvider>>,
}

impl ProviderRegistry {
    pub(crate) const fn new(providers: HashMap<&'static str, Box<dyn ExternalProvider>>) -> Self {
        Self { providers }
    }

    /// The provider registered under `slug`, or `UnknownProvider` (maps to a 404, not a 500).
    pub(crate) fn get(&self, slug: &str) -> anyhow::Result<&dyn ExternalProvider> {
        self.try_get(slug).ok_or_else(|| {
            anyhow::Error::new(crate::error::SyncError::UnknownProvider(slug.to_owned()))
        })
    }

    /// The provider registered under `slug`, if any, for callers that skip unknown slugs instead of failing.
    pub(crate) fn try_get(&self, slug: &str) -> Option<&dyn ExternalProvider> {
        self.providers.get(slug).map(Box::as_ref)
    }

    /// Every provider with its slug, for the sweeps that iterate the whole registry.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&&'static str, &dyn ExternalProvider)> {
        self.providers.iter().map(|(slug, p)| (slug, p.as_ref()))
    }

    /// Whether any provider exposes a public, tokenless metadata API.
    pub(crate) fn any_public_metadata(&self) -> bool {
        self.providers
            .values()
            .any(|p| p.supports_public_metadata())
    }

    /// The registered providers, slug-sorted, for `GET /v1/sync/providers`.
    pub(crate) fn list(&self) -> Vec<ProviderInfo> {
        let mut list: Vec<_> = self
            .providers
            .values()
            .map(|p| ProviderInfo {
                slug: p.slug().to_owned(),
                name: p.display_name().to_owned(),
            })
            .collect();
        list.sort_by(|a, b| a.slug.cmp(&b.slug));
        list
    }
}
