//! Owns a single, long-lived `chromiumoxide` browser, launched lazily on first render
//! so `/health`/`/ready` come up even without a Chrome binary present, and relaunched once its
//! CDP connection is gone.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::error::CdpError;
use chromiumoxide::page::Page;
use futures::StreamExt;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use crate::config::RenderConfig;
use crate::intercept;

/// Tabs open at once across every caller; a request beyond this waits, inside its own budget.
///
/// Each tab is a renderer process of a few hundred MB, so this, not the CPU, is what the 2 GB
/// container limit runs out of first.
const MAX_TABS: usize = 4;

/// Ceiling on the settle delay, whoever asked for it: the request's `wait_ms` is caller-set.
const MAX_SETTLE_MS: u64 = 30_000;

/// Allowance on top of navigation and settle for launch, queueing for a tab, and capture.
const RENDER_OVERHEAD: Duration = Duration::from_secs(20);

/// How long a tab may take to close before it is abandoned to the browser.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Extra settle delay after navigation/selector, in milliseconds; clamped to
    /// [`MAX_SETTLE_MS`].
    pub(crate) wait_ms: u64,
}

/// Returns the settle delay actually applied: the larger of the caller's and the configured
/// default, never more than [`MAX_SETTLE_MS`].
fn settle_ms(requested: u64, default: u64) -> u64 {
    requested.max(default).min(MAX_SETTLE_MS)
}

/// Returns the wall-clock budget for one render, queueing for a tab included.
fn render_budget(nav_timeout_ms: u64, settle_ms: u64) -> Duration {
    Duration::from_millis(nav_timeout_ms.saturating_add(settle_ms)).saturating_add(RENDER_OVERHEAD)
}

/// Returns true when `error` means the CDP connection itself is gone, not just one bad message.
fn ends_connection(error: &CdpError) -> bool {
    matches!(
        error,
        CdpError::Ws(_) | CdpError::Io(_) | CdpError::ChannelSendError(_)
    )
}

/// A launched browser and whether its CDP connection is still being driven.
struct Instance {
    browser: Arc<Browser>,
    connected: Arc<AtomicBool>,
}

/// Lazily-launched, shared headless browser.
pub(crate) struct BrowserManager {
    cfg: RenderConfig,
    instance: Mutex<Option<Instance>>,
    tabs: Arc<Semaphore>,
}

impl BrowserManager {
    pub(crate) fn new(cfg: RenderConfig) -> Self {
        Self {
            cfg,
            instance: Mutex::new(None),
            tabs: Arc::new(Semaphore::new(MAX_TABS)),
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

    /// Returns the shared browser, launching it on first use and again after it disconnects.
    async fn browser(&self) -> anyhow::Result<Arc<Browser>> {
        let mut guard = self.instance.lock().await;
        if let Some(instance) = guard.as_ref() {
            if instance.connected.load(Ordering::Acquire) {
                return Ok(Arc::clone(&instance.browser));
            }
            tracing::warn!("browser connection lost; relaunching");
            // Dropping the last handle kills the process (`kill_on_drop`); in-flight renders
            // holding one fail on their own.
            *guard = None;
        }

        let (browser, mut handler) = Browser::launch(self.build_config()?).await?;
        let connected = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&connected);
        // The handler drives the CDP connection; when it ends the browser is unusable, and the
        // flag is what makes the next render relaunch instead of reusing a dead handle.
        tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                match event {
                    Err(e) if ends_connection(&e) => {
                        tracing::warn!(error = %e, "cdp connection ended");
                        break;
                    }
                    Err(e) => tracing::debug!(error = %e, "cdp handler event error"),
                    Ok(()) => {}
                }
            }
            flag.store(false, Ordering::Release);
        });

        let browser = Arc::new(browser);
        // Before the browser is cached: a tab opened without it would fetch unchecked.
        intercept::install(&browser).await?;
        *guard = Some(Instance {
            browser: Arc::clone(&browser),
            connected,
        });
        Ok(browser)
    }

    /// Navigates to `opts.url`, waits for it to render, and captures the DOM and session.
    ///
    /// # Errors
    /// Returns an error on a launch, navigation or capture failure, or past the render budget.
    pub(crate) async fn render(&self, opts: RenderOptions) -> anyhow::Result<RenderResult> {
        let settle = settle_ms(opts.wait_ms, self.cfg.default_wait_ms);
        let budget = render_budget(self.cfg.nav_timeout_ms, settle);
        tokio::time::timeout(budget, self.render_within(opts, settle))
            .await
            .map_err(|_| anyhow::anyhow!("render exceeded its {}s budget", budget.as_secs()))?
    }

    async fn render_within(
        &self,
        opts: RenderOptions,
        settle: u64,
    ) -> anyhow::Result<RenderResult> {
        let permit = Arc::clone(&self.tabs).acquire_owned().await?;
        let browser = self.browser().await?;
        let tab = Tab::new(browser.new_page("about:blank").await?, permit);
        let page = tab.page();

        // Before navigating, so the first request already carries it.
        if let Some(ua) = &self.cfg.user_agent {
            page.set_user_agent(ua.as_str()).await?;
        }
        page.goto(opts.url.as_str()).await?;

        if let Some(sel) = &opts.wait_selector {
            // Best-effort: a missing selector must not fail the whole render.
            if let Err(e) = page.find_element(sel).await {
                tracing::debug!(selector = %sel, error = %e, "wait_selector not found");
            }
        }

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

        tab.close().await;
        Ok(RenderResult {
            final_url,
            html,
            cookies,
            user_agent,
        })
    }
}

/// One open tab and the permit it occupies.
///
/// The tab is closed on every exit, including the ones that never reach a `close().await`: an
/// early `?`, a render timeout, or the HTTP caller hanging up and dropping the future. Without
/// this each of those would leak a renderer process until the browser died.
struct Tab {
    page: Page,
    /// Taken by whichever close runs, so the tab is closed exactly once.
    permit: Option<OwnedSemaphorePermit>,
}

impl Tab {
    fn new(page: Page, permit: OwnedSemaphorePermit) -> Self {
        Self {
            page,
            permit: Some(permit),
        }
    }

    fn page(&self) -> &Page {
        &self.page
    }

    /// Closes the tab now, releasing its permit once it is gone.
    async fn close(mut self) {
        let permit = self.permit.take();
        close_page(self.page.clone()).await;
        drop(permit);
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        // `None` once `close` has run.
        let Some(permit) = self.permit.take() else {
            return;
        };
        // The permit moves into the task, so the cap counts tabs that are still closing.
        let page = self.page.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                close_page(page).await;
                drop(permit);
            });
        }
    }
}

async fn close_page(page: Page) {
    match tokio::time::timeout(CLOSE_TIMEOUT, page.close()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::debug!(error = %e, "page close failed"),
        Err(_) => tracing::debug!("page close timed out"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `wait_ms` came straight from the request, so one caller could hold a tab
    /// open for as long as it liked.
    #[test]
    fn settle_is_clamped_whoever_asks() {
        assert_eq!(settle_ms(u64::MAX, 0), MAX_SETTLE_MS);
        assert_eq!(settle_ms(0, u64::MAX), MAX_SETTLE_MS);
        assert_eq!(settle_ms(500, 2_000), 2_000);
        assert_eq!(settle_ms(0, 0), 0);
    }

    #[test]
    fn budget_covers_navigation_settle_and_overhead() {
        assert_eq!(
            render_budget(30_000, 5_000),
            Duration::from_secs(35) + RENDER_OVERHEAD
        );
        // Saturates instead of overflowing on a hostile configuration.
        assert!(render_budget(u64::MAX, MAX_SETTLE_MS) >= RENDER_OVERHEAD);
    }

    /// Regression: a crashed browser stayed cached, so every later render failed against a
    /// dead connection until the process restarted.
    #[test]
    fn transport_errors_end_the_connection_and_protocol_errors_do_not() {
        assert!(ends_connection(&CdpError::Io(std::io::Error::other("eof"))));
        assert!(!ends_connection(&CdpError::Timeout));
        assert!(!ends_connection(&CdpError::NotFound));
    }
}
