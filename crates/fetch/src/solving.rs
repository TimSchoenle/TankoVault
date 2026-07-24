//! Challenge-solving decorator: detect a bot-management challenge in-band, delegate to a
//! [`ChallengeSolver`], cache the solved session, and replay (design §9).
//!
//! Sessions are cached per provider and replayed on later requests so one solve
//! amortises across many fetches until it expires; expiry re-triggers a solve, not a
//! block. The store is abstracted so a Redis-backed implementation (shared across worker
//! replicas) can replace the in-memory default without touching this decorator.

use crate::error::FetchError;
use crate::fetcher::Fetcher;
use crate::types::{FetchRequest, FetchResponse};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tankovault_solver::{ChallengeSolver, SolveRequest, detect_challenge};
use time::{Duration, OffsetDateTime};
use tokio::sync::RwLock;

/// A cached solved session (cookies + UA + expiry).
#[derive(Debug, Clone)]
pub struct SolvedSession {
    pub cookies: Vec<(String, String)>,
    pub user_agent: String,
    pub expires_at: OffsetDateTime,
}

impl SolvedSession {
    fn is_valid(&self) -> bool {
        self.expires_at > OffsetDateTime::now_utc()
    }

    /// Render the `Cookie` header value.
    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Storage for solved sessions, keyed by provider slug.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Fetch a still-valid session for `provider`, if any.
    async fn get(&self, provider: &str) -> Option<SolvedSession>;
    /// Store (or replace) the session for `provider`.
    async fn put(&self, provider: &str, session: SolvedSession);
}

/// Process-local session store (default; suitable for a single replica or tests).
#[derive(Default)]
pub struct InMemorySessionStore {
    map: RwLock<HashMap<String, SolvedSession>>,
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn get(&self, provider: &str) -> Option<SolvedSession> {
        let guard = self.map.read().await;
        guard.get(provider).filter(|s| s.is_valid()).cloned()
    }
    async fn put(&self, provider: &str, session: SolvedSession) {
        self.map.write().await.insert(provider.to_owned(), session);
    }
}

/// Detects challenges and solves them via the injected solver.
pub struct SolvingFetcher<F> {
    inner: F,
    solver: Arc<dyn ChallengeSolver>,
    store: Arc<dyn SessionStore>,
}

impl<F> SolvingFetcher<F> {
    #[must_use]
    pub fn new(inner: F, solver: Arc<dyn ChallengeSolver>, store: Arc<dyn SessionStore>) -> Self {
        Self {
            inner,
            solver,
            store,
        }
    }
}

fn apply_session(mut req: FetchRequest, session: &SolvedSession) -> FetchRequest {
    req.user_agent = Some(session.user_agent.clone());
    req.headers
        .retain(|(k, _)| !k.eq_ignore_ascii_case("cookie"));
    if !session.cookies.is_empty() {
        req.headers
            .push(("Cookie".to_owned(), session.cookie_header()));
    }
    req
}

#[async_trait]
impl<F: Fetcher> Fetcher for SolvingFetcher<F> {
    async fn get(&self, mut req: FetchRequest) -> Result<FetchResponse, FetchError> {
        // Replay a cached session if we have one.
        if let Some(session) = self.store.get(&req.provider_slug).await {
            req = apply_session(req, &session);
        }

        let resp = self.inner.get(req.clone()).await?;
        let Some(kind) = detect_challenge(&resp) else {
            return Ok(resp);
        };

        tracing::info!(provider = %req.provider_slug, challenge = kind.as_str(), "challenge detected; delegating to solver");
        let outcome = self
            .solver
            .solve(SolveRequest {
                url: req.url.clone(),
                provider: req.provider_slug.clone(),
                kind: Some(kind),
            })
            .await
            .map_err(|e| FetchError::Solver(e.to_string()))?;

        let session = SolvedSession {
            cookies: outcome.cookies.clone(),
            user_agent: outcome.user_agent.clone(),
            expires_at: OffsetDateTime::now_utc()
                + Duration::seconds(i64::try_from(outcome.ttl_secs).unwrap_or(i64::MAX)),
        };
        self.store.put(&req.provider_slug, session.clone()).await;

        // If the solver already fetched the solved page, use it directly.
        if let Some(html) = outcome.html {
            return Ok(FetchResponse {
                status: 200,
                url: req.url.clone(),
                headers: Vec::new(),
                body: html,
                from_cache: false,
            });
        }

        // Otherwise replay the original request with the fresh session.
        let replay = apply_session(req, &session);
        let resp2 = self.inner.get(replay).await?;
        if detect_challenge(&resp2).is_some() {
            return Err(FetchError::Challenge(kind));
        }
        Ok(resp2)
    }
}
