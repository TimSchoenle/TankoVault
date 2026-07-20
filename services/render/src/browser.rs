//! Headless-browser management for the render service.
//!
//! Drives a single, long-lived `chromiumoxide` browser (design §9). The browser is
//! expensive to start and is reused across requests; it is launched **lazily** on the
//! first render so the service's `/health`/`/ready` come up even when no Chrome binary
//! is present — a render then fails cleanly rather than the whole tier refusing to boot.

use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::page::Page;
use futures::StreamExt;
use tokio::sync::Mutex;

use crate::config::RenderConfig;

/// A rendered page: the (possibly redirected) final URL, the fully-rendered DOM, the
/// session cookies the browser accumulated, and the user-agent that was in force.
#[derive(Debug, Clone)]
pub(crate) struct RenderResult {
    pub(crate) final_url: String,
    pub(crate) html: String,
    pub(crate) cookies: Vec<(String, String)>,
    pub(crate) user_agent: String,
}

/// Per-request rendering knobs (a subset is overridable by the HTTP caller).
#[derive(Debug, Clone)]
pub(crate) struct RenderOptions {
    pub(crate) url: String,
    /// Optional CSS selector to await before capturing (for client-rendered lists).
    pub(crate) wait_selector: Option<String>,
    /// Extra settle delay after navigation/selector, in milliseconds.
    pub(crate) wait_ms: u64,
}

/// Lazily-launched, shared headless browser.
pub(crate) struct BrowserManager {
    cfg: RenderConfig,
    browser: Mutex<Option<Arc<Browser>>>,
}

impl BrowserManager {
    pub(crate) fn new(cfg: RenderConfig) -> Self {
        Self {
            cfg,
            browser: Mutex::new(None),
        }
    }

    fn build_config(&self) -> anyhow::Result<BrowserConfig> {
        let mut builder = BrowserConfig::builder();
        if let Some(path) = &self.cfg.chrome_path {
            builder = builder.chrome_executable(path);
        }
        if !self.cfg.headless {
            builder = builder.with_head();
        }
        if self.cfg.no_sandbox {
            builder = builder.no_sandbox();
        }
        builder = builder.request_timeout(Duration::from_millis(self.cfg.nav_timeout_ms));
        builder
            .build()
            .map_err(|e| anyhow::anyhow!("invalid browser config: {e}"))
    }

    /// Return the shared browser, launching it (and its event-handler pump) on first use.
    async fn browser(&self) -> anyhow::Result<Arc<Browser>> {
        let mut guard = self.browser.lock().await;
        if let Some(b) = guard.as_ref() {
            return Ok(Arc::clone(b));
        }
        let config = self.build_config()?;
        let (browser, mut handler) = Browser::launch(config).await?;
        // The handler stream drives the CDP connection and must be polled for the life
        // of the browser; detach it — it ends when the browser is dropped.
        tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if let Err(e) = event {
                    tracing::debug!(error = %e, "cdp handler event error");
                }
            }
        });
        let browser = Arc::new(browser);
        *guard = Some(Arc::clone(&browser));
        Ok(browser)
    }

    /// Navigate to `opts.url`, wait for it to render, and capture the DOM + session.
    pub(crate) async fn render(&self, opts: RenderOptions) -> anyhow::Result<RenderResult> {
        let browser = self.browser().await?;
        let page: Page = browser.new_page(opts.url.as_str()).await?;

        if let Some(ua) = &self.cfg.user_agent {
            page.set_user_agent(ua.as_str()).await?;
        }

        page.wait_for_navigation().await?;

        if let Some(sel) = &opts.wait_selector {
            // Best-effort: a missing selector must not fail the whole render.
            if let Err(e) = page.find_element(sel).await {
                tracing::debug!(selector = %sel, error = %e, "wait_selector not found");
            }
        }

        let settle = opts.wait_ms.max(self.cfg.default_wait_ms);
        if settle > 0 {
            tokio::time::sleep(Duration::from_millis(settle)).await;
        }

        let html = page.content().await?;
        let final_url = page.url().await?.unwrap_or_else(|| opts.url.clone());
        let cookies = page
            .get_cookies()
            .await?
            .into_iter()
            .map(|c| (c.name, c.value))
            .collect();
        // Read the effective UA so a solved session's cookies stay paired with it,
        // falling back to any configured override.
        let user_agent = match page.evaluate("navigator.userAgent").await {
            Ok(v) => v.into_value::<String>().unwrap_or_default(),
            Err(_) => self.cfg.user_agent.clone().unwrap_or_default(),
        };

        // Free the tab; a failure to close is non-fatal.
        if let Err(e) = page.close().await {
            tracing::debug!(error = %e, "page close failed");
        }

        Ok(RenderResult {
            final_url,
            html,
            cookies,
            user_agent,
        })
    }
}
