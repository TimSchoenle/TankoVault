//! robots.txt honouring: a minimal parser + a gate that refuses disallowed paths.
//!
//! Rules are extracted for our user-agent (falling back to the `*` group). Matching is
//! longest-prefix with allow-wins-on-tie, which covers the CMS-style robots files our
//! providers ship. (Wildcard `*`/`$` patterns are treated as prefixes — a documented
//! simplification; tighten if a provider needs it.)

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use url::Url;

/// Parsed robots rules applicable to one user-agent.
#[derive(Debug, Clone, Default)]
pub struct RobotsRules {
    disallow: Vec<String>,
    allow: Vec<String>,
    /// Crawl-delay in seconds, if the provider specified one for our agent.
    pub crawl_delay: Option<f64>,
}

#[derive(Default)]
struct Group {
    agents: Vec<String>,
    disallow: Vec<String>,
    allow: Vec<String>,
    crawl_delay: Option<f64>,
}

impl RobotsRules {
    /// Parse `text`, selecting the rule group for `agent` (case-insensitive) or `*`.
    #[must_use]
    pub fn parse(text: &str, agent: &str) -> Self {
        let agent_lc = agent.to_lowercase();
        let mut groups: Vec<Group> = Vec::new();
        let mut expecting_agent = false;

        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            let Some((field, value)) = line.split_once(':') else {
                continue;
            };
            let field = field.trim().to_lowercase();
            let value = value.trim().to_owned();

            match field.as_str() {
                "user-agent" => {
                    if !expecting_agent || groups.is_empty() {
                        groups.push(Group::default());
                    }
                    if let Some(g) = groups.last_mut() {
                        g.agents.push(value.to_lowercase());
                    }
                    expecting_agent = true;
                }
                "disallow" => {
                    expecting_agent = false;
                    if let Some(g) = groups.last_mut() {
                        g.disallow.push(value);
                    }
                }
                "allow" => {
                    expecting_agent = false;
                    if let Some(g) = groups.last_mut() {
                        g.allow.push(value);
                    }
                }
                "crawl-delay" => {
                    expecting_agent = false;
                    if let (Some(g), Ok(d)) = (groups.last_mut(), value.parse::<f64>()) {
                        g.crawl_delay = Some(d);
                    }
                }
                _ => {}
            }
        }

        // Prefer a group naming our agent; otherwise the `*` group.
        let specific = groups
            .iter()
            .find(|g| g.agents.iter().any(|a| a == &agent_lc));
        let wildcard = groups.iter().find(|g| g.agents.iter().any(|a| a == "*"));

        match specific.or(wildcard) {
            Some(g) => Self {
                // A single empty Disallow means "allow all" — drop it.
                disallow: g
                    .disallow
                    .iter()
                    .filter(|d| !d.is_empty())
                    .cloned()
                    .collect(),
                allow: g.allow.clone(),
                crawl_delay: g.crawl_delay,
            },
            None => Self::default(),
        }
    }

    /// Whether `path` may be crawled under these rules.
    #[must_use]
    pub fn is_allowed(&self, path: &str) -> bool {
        let longest = |rules: &[String]| -> usize {
            rules
                .iter()
                .filter(|r| path.starts_with(r.as_str()))
                .map(String::len)
                .max()
                .unwrap_or(0)
        };
        let dis = longest(&self.disallow);
        if dis == 0 {
            return true;
        }
        let allow = longest(&self.allow);
        allow >= dis // allow wins on ties
    }
}

/// The robots gate: rejects disallowed paths before any network call.
pub struct RobotsFetcher<F> {
    inner: F,
    rules: Option<RobotsRules>,
}

impl<F> RobotsFetcher<F> {
    /// Wrap `inner`; `rules` of `None` means no robots restrictions are applied.
    #[must_use]
    pub fn new(inner: F, rules: Option<RobotsRules>) -> Self {
        Self { inner, rules }
    }
}

#[async_trait]
impl<F: Fetcher> Fetcher for RobotsFetcher<F> {
    async fn get(&self, req: FetchRequest) -> Result<FetchResponse, FetchError> {
        if let Some(rules) = &self.rules {
            let url = Url::parse(&req.url).map_err(|_| FetchError::InvalidUrl(req.url.clone()))?;
            if !rules.is_allowed(url.path()) {
                return Err(FetchError::RobotsDisallowed(url.path().to_owned()));
            }
        }
        self.inner.get(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
User-agent: *
Disallow: /admin/
Disallow: /private
Allow: /private/public
Crawl-delay: 2

User-agent: TankoVaultBot
Disallow: /nothing-for-us/
";

    #[test]
    fn selects_specific_agent_group() {
        let rules = RobotsRules::parse(SAMPLE, "TankoVaultBot");
        assert!(rules.is_allowed("/admin/")); // not disallowed for our specific agent
        assert!(!rules.is_allowed("/nothing-for-us/page"));
    }

    #[test]
    fn falls_back_to_wildcard_group() {
        let rules = RobotsRules::parse(SAMPLE, "SomeOtherBot");
        assert!(!rules.is_allowed("/admin/x"));
        assert!(!rules.is_allowed("/private/secret"));
        assert!(rules.is_allowed("/private/public/page")); // allow overrides disallow
        assert!(rules.is_allowed("/manga/solo-leveling"));
        assert_eq!(rules.crawl_delay, Some(2.0));
    }

    #[test]
    fn empty_disallow_allows_all() {
        let rules = RobotsRules::parse("User-agent: *\nDisallow:", "x");
        assert!(rules.is_allowed("/anything"));
    }
}
