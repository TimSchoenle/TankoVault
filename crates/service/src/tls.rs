//! Mutually-authenticated TLS for the internal tier.
//!
//! This module knows about three files — a certificate, its key, and a bundle of authorities —
//! and nothing about where they come from. Under Kubernetes cert-manager writes the first two
//! into a Secret and trust-manager writes the third into a `ConfigMap`, both mounted as volumes;
//! elsewhere `openssl` or `step-ca` produce the same three files. Keeping the orchestrator out
//! of the code is what lets `identity = "mtls"` work outside Kubernetes at all.
//!
//! Client authentication is **required**, not optional: a connection that presents no
//! certificate is refused during the handshake, so an unverified request never reaches
//! [`crate::internal_auth`]. Health and readiness stay on the plain listener, since a kubelet
//! probe presents no client certificate.

use arc_swap::ArcSwap;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use tokio_util::sync::CancellationToken;

use tankovault_config::ResolvedTls;

/// How often the certificate files are restated for change.
///
/// cert-manager renews at roughly two thirds of a certificate's lifetime and the kubelet
/// refreshes a mounted Secret within about a minute, so for the 90-day certificates this is
/// sized for, polling is many orders of magnitude faster than it needs to be. Polling rather
/// than watching deliberately: a `notify` watcher is one more dependency and one more failure
/// mode (silently dead watches on some filesystems) for a deadline measured in weeks.
const RELOAD_POLL: Duration = Duration::from_secs(30);

/// How long a client is given to finish its handshake before the connection is dropped.
///
/// Handshakes run on their own tasks so a slow one cannot block the accept loop, but without a
/// deadline they would still accumulate: opening sockets and never speaking is the cheapest
/// possible way to exhaust a server's memory.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How many completed handshakes may queue ahead of the server.
const ACCEPT_BACKLOG: usize = 128;

/// Errors from reading or serving the mTLS material.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("reading {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("{path} contains no {want}")]
    Empty { path: String, want: &'static str },
    #[error("building the TLS configuration: {0}")]
    Config(String),
}

/// The peer identities carried by a verified client certificate.
///
/// Every DNS subject alternative name, not just the first: a cert-manager certificate routinely
/// carries `api`, `api.<ns>`, `api.<ns>.svc` and the fully-qualified form, and which one comes
/// first is not something an operator writing `internal.peers.api.san` should have to predict.
#[derive(Clone, Debug, Default)]
pub struct PeerSans(pub Arc<[String]>);

impl PeerSans {
    /// Whether `candidate` is one of the names this peer proved.
    #[must_use]
    pub fn contains(&self, candidate: &str) -> bool {
        self.0.iter().any(|s| s == candidate)
    }
}

/// What a connection tells the stack about its peer.
///
/// Carries the socket address so the rate limiter keeps working unchanged, plus the verified
/// names when the connection was mutually authenticated.
#[derive(Clone, Debug)]
pub struct InternalPeer {
    pub addr: SocketAddr,
    pub sans: PeerSans,
}

/// A server configuration that reloads itself when its files change on disk.
#[derive(Debug)]
pub struct ReloadingTls {
    current: ArcSwap<ServerConfig>,
    paths: ResolvedTls,
    /// Modification times the loaded configuration was built from.
    stamps: std::sync::Mutex<Stamps>,
}

/// The mtimes of the three inputs, compared to decide whether a reload is due.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
struct Stamps {
    cert: Option<SystemTime>,
    key: Option<SystemTime>,
    ca: Option<SystemTime>,
}

impl Stamps {
    fn read(paths: &ResolvedTls) -> Self {
        Self {
            cert: mtime(&paths.cert),
            key: mtime(&paths.key),
            ca: mtime(&paths.ca),
        }
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

impl ReloadingTls {
    /// Read the three files and build the initial configuration.
    ///
    /// # Errors
    /// [`TlsError`] if any file is unreadable, contains nothing usable, or does not produce a
    /// valid client-verifying server configuration.
    pub fn load(paths: &ResolvedTls) -> Result<Self, TlsError> {
        let config = build(paths)?;
        Ok(Self {
            current: ArcSwap::from(config),
            paths: paths.clone(),
            stamps: std::sync::Mutex::new(Stamps::read(paths)),
        })
    }

    /// The configuration in force right now.
    #[must_use]
    pub fn current(&self) -> Arc<ServerConfig> {
        self.current.load_full()
    }

    /// Rebuild if any input changed; keep the running configuration if the rebuild fails.
    ///
    /// A failed reload must never take the listener down: a half-written Secret is a transient
    /// state during rotation, and the certificate already in memory stays valid until its own
    /// expiry. Serving on the old material is strictly better than refusing every connection.
    fn reload_if_changed(&self) {
        let fresh = Stamps::read(&self.paths);
        {
            let mut stamps = self
                .stamps
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *stamps == fresh {
                return;
            }
            *stamps = fresh;
        }

        match build(&self.paths) {
            Ok(config) => {
                self.current.store(config);
                tracing::info!("reloaded the internal TLS material after a change on disk");
            }
            Err(e) => tracing::error!(
                error = %e,
                "the internal TLS material changed but could not be loaded; keeping the \
                 configuration already in force"
            ),
        }
    }

    /// Poll for rotation until `shutdown` is cancelled.
    pub async fn watch(self: Arc<Self>, shutdown: CancellationToken) {
        let mut ticker = tokio::time::interval(RELOAD_POLL);
        ticker.tick().await;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = ticker.tick() => {
                    let this = Arc::clone(&self);
                    // Blocking file I/O off the reactor: three `stat`s and, when something
                    // changed, three reads.
                    if tokio::task::spawn_blocking(move || this.reload_if_changed()).await.is_err() {
                        tracing::warn!("the TLS reload task panicked; retrying next tick");
                    }
                }
            }
        }
    }
}

/// Read the three files and assemble a client-verifying server configuration.
fn build(paths: &ResolvedTls) -> Result<Arc<ServerConfig>, TlsError> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&paths.cert)
        .map_err(|e| TlsError::Read {
            path: paths.cert.display().to_string(),
            source: io::Error::other(e),
        })?
        .collect::<Result<_, _>>()
        .map_err(|e| TlsError::Read {
            path: paths.cert.display().to_string(),
            source: io::Error::other(e),
        })?;
    if certs.is_empty() {
        return Err(TlsError::Empty {
            path: paths.cert.display().to_string(),
            want: "certificates",
        });
    }

    let key = PrivateKeyDer::from_pem_file(&paths.key).map_err(|e| TlsError::Read {
        path: paths.key.display().to_string(),
        source: io::Error::other(e),
    })?;

    let mut roots = RootCertStore::empty();
    for anchor in CertificateDer::pem_file_iter(&paths.ca).map_err(|e| TlsError::Read {
        path: paths.ca.display().to_string(),
        source: io::Error::other(e),
    })? {
        let anchor = anchor.map_err(|e| TlsError::Read {
            path: paths.ca.display().to_string(),
            source: io::Error::other(e),
        })?;
        roots
            .add(anchor)
            .map_err(|e| TlsError::Config(e.to_string()))?;
    }
    if roots.is_empty() {
        return Err(TlsError::Empty {
            path: paths.ca.display().to_string(),
            want: "trust anchors",
        });
    }

    // `builder`, not `builder_with_provider`: whichever provider is installed process-wide is
    // the one the rest of the stack (reqwest, sqlx) already negotiated with.
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| TlsError::Config(e.to_string()))?;

    ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)
        .map(Arc::new)
        .map_err(|e| TlsError::Config(e.to_string()))
}

/// The PEM bytes an outbound client needs to authenticate itself and verify its peer.
///
/// Raw bytes rather than a built client: the two HTTP stacks in this workspace take them
/// differently (`reqwest::Identity::from_pem` wants the chain and key concatenated, `wreq`'s
/// `Identity::from_pkcs8_pem` wants them separate), and neither belongs as a dependency of this
/// crate. Reading the files once, here, is what keeps the *paths* in one place.
#[derive(Clone)]
pub struct ClientMaterial {
    /// The certificate chain, PEM.
    pub cert: Vec<u8>,
    /// The private key, PEM. Not a `secrecy` type: it never leaves this struct as text, and the
    /// TLS builders that consume it take `&[u8]`.
    pub key: Vec<u8>,
    /// The authorities a server certificate must chain to, PEM.
    pub ca: Vec<u8>,
}

impl std::fmt::Debug for ClientMaterial {
    /// Hand-written so a `tracing::debug!(?material)` cannot print the private key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientMaterial")
            .field("cert_bytes", &self.cert.len())
            .field("key", &"[REDACTED]")
            .field("ca_bytes", &self.ca.len())
            .finish()
    }
}

/// Read the three files an outbound mTLS client needs.
///
/// # Errors
/// [`TlsError::Read`] naming whichever path could not be read.
pub fn client_material(paths: &ResolvedTls) -> Result<ClientMaterial, TlsError> {
    let read = |path: &Path| {
        std::fs::read(path).map_err(|source| TlsError::Read {
            path: path.display().to_string(),
            source,
        })
    };
    Ok(ClientMaterial {
        cert: read(&paths.cert)?,
        key: read(&paths.key)?,
        ca: read(&paths.ca)?,
    })
}

/// Every DNS subject alternative name in `cert`.
fn dns_sans(cert: &CertificateDer<'_>) -> Vec<String> {
    use x509_parser::extensions::GeneralName;
    let Ok((_, parsed)) = x509_parser::parse_x509_certificate(cert) else {
        return Vec::new();
    };
    let Ok(Some(san)) = parsed.subject_alternative_name() else {
        return Vec::new();
    };
    san.value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::DNSName(dns) => Some((*dns).to_owned()),
            _ => None,
        })
        .collect()
}

/// A listener that yields only connections whose handshake has already completed.
///
/// Handshakes run on their own tasks and are bounded by [`HANDSHAKE_TIMEOUT`]: performing them
/// inside `accept` would let one slow client stall every other connection to the service.
pub struct TlsListener {
    rx: tokio::sync::mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
}

impl TlsListener {
    /// Bind `addr` and start accepting and handshaking in the background.
    ///
    /// # Errors
    /// Propagates the bind failure.
    pub async fn bind(addr: &str, tls: Arc<ReloadingTls>) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let local = listener.local_addr()?;
        let (tx, rx) = tokio::sync::mpsc::channel(ACCEPT_BACKLOG);

        tokio::spawn(async move {
            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    // A per-connection accept error (a peer that vanished, a transient fd
                    // limit) must not end the loop and take the listener with it.
                    Err(e) => {
                        tracing::warn!(error = %e, "accepting an internal connection failed");
                        continue;
                    }
                };

                let acceptor = TlsAcceptor::from(tls.current());
                let tx = tx.clone();
                tokio::spawn(async move {
                    match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(tls_stream)) => {
                            let _ = tx.send((tls_stream, peer)).await;
                        }
                        // Both arms are ordinary on an internal listener that requires client
                        // certificates: a probe or a scanner produces one per connection, so
                        // this is `debug`, not `warn`.
                        Ok(Err(e)) => tracing::debug!(%peer, error = %e, "TLS handshake failed"),
                        Err(_) => tracing::debug!(%peer, "TLS handshake timed out"),
                    }
                });
            }
        });

        Ok(Self { rx, local })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            if let Some(accepted) = self.rx.recv().await {
                return accepted;
            }
            // The accept task owns the only sender and never returns, so this is unreachable in
            // practice; yielding beats a busy loop if it ever becomes reachable.
            tokio::task::yield_now().await;
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, TlsListener>>
    for InternalPeer
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, TlsListener>) -> Self {
        let addr = *stream.remote_addr();
        // `peer_certificates` is populated only after a successful handshake, and the verifier
        // requires one, so an empty list here means the connection was not mutually
        // authenticated — which `internal_auth` then refuses.
        let sans = stream
            .io()
            .get_ref()
            .1
            .peer_certificates()
            .and_then(<[CertificateDer<'_>]>::first)
            .map(dns_sans)
            .unwrap_or_default();

        Self {
            addr,
            sans: PeerSans(sans.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_sans_matches_any_name_the_certificate_proved() {
        let sans = PeerSans(
            vec![
                "api".to_owned(),
                "api.tankovault".to_owned(),
                "api.tankovault.svc".to_owned(),
            ]
            .into(),
        );
        assert!(sans.contains("api"));
        assert!(sans.contains("api.tankovault.svc"));
        assert!(!sans.contains("worker"));
        assert!(!sans.contains("api.tankovault.svc.cluster.local"));
    }

    /// A certificate carries several names and the useful one is rarely first. Matching only
    /// `[0]` would make `internal.peers.<name>.san` depend on the order cert-manager happened
    /// to emit, which is not something an operator can see from the manifest they wrote.
    #[test]
    fn an_empty_or_unparsable_certificate_yields_no_names_rather_than_panicking() {
        assert!(dns_sans(&CertificateDer::from(vec![])).is_empty());
        assert!(dns_sans(&CertificateDer::from(vec![0x30, 0x00])).is_empty());
    }

    /// Missing files must surface as a named error rather than a panic: this runs at boot, and
    /// the operator needs to know *which* of the three paths is wrong.
    #[test]
    fn a_missing_file_names_itself() {
        let err = ReloadingTls::load(&ResolvedTls {
            cert: "/nonexistent/tls.crt".into(),
            key: "/nonexistent/tls.key".into(),
            ca: "/nonexistent/ca.crt".into(),
        })
        .expect_err("nothing is readable there");
        assert!(err.to_string().contains("tls.crt"), "{err}");
    }
}
