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
use pkcs8::der::pem::PemLabel as _;
use pkcs8::der::zeroize::Zeroizing;
use pkcs8::der::{Decode as _, Encode as _};
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

use tankovault_config::{PeerIdentity, ResolvedTls};

/// How often the certificate files are restated for change.
///
/// Sized for the shortest-lived material this serves, which is a SPIRE X.509-SVID: one hour by
/// default, rewritten by `spiffe-helper` at roughly half its life, so the window between a
/// rotation landing on disk and the old certificate expiring is around thirty minutes. Polling
/// every thirty seconds spends that window sixty times over. The 90-day cert-manager material
/// this also serves is slower by four orders of magnitude and needs nothing tighter.
///
/// Polling rather than watching deliberately: a `notify` watcher is one more dependency and one
/// more failure mode (silently dead watches on some filesystems), and a missed rotation here
/// fails closed at the next handshake rather than quietly serving an expired identity.
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
    #[error("{path} is not a private key this stack can present: {reason}")]
    Key { path: String, reason: String },
    #[error("building the TLS configuration: {0}")]
    Config(String),
}

/// The peer identities carried by a verified client certificate, kept apart by SAN kind.
///
/// Every name of each kind, not just the first: a cert-manager certificate routinely carries
/// `api`, `api.<ns>`, `api.<ns>.svc` and the fully-qualified form, and which one comes first is
/// not something an operator writing `internal.peers.api.san` should have to predict.
///
/// # Why the two kinds never share a list
///
/// A trust bundle may hold more than one authority — an internal CA alongside SPIRE's — and any
/// authority that can issue a `DNSName` can equally issue a `URI` of `spiffe://…`. If both kinds
/// went into one set and a configured value were compared against all of it, a certificate from
/// the weaker authority carrying a forged SPIFFE URI would authenticate as the SPIRE workload.
/// The expectation therefore names its kind ([`PeerIdentity`]) and only that kind is searched.
#[derive(Clone, Debug, Default)]
pub struct PeerSans {
    dns: Arc<[String]>,
    uris: Arc<[String]>,
}

impl PeerSans {
    /// Build from the names of each kind. Public for tests and for callers assembling a peer
    /// outside the TLS listener.
    #[must_use]
    pub fn new(dns: Vec<String>, uris: Vec<String>) -> Self {
        Self {
            dns: dns.into(),
            uris: uris.into(),
        }
    }

    /// Whether the certificate proved the name `expected` demands, of the kind it demands.
    ///
    /// Compared whole, never by prefix: `spiffe://td/ns/tankovault/sa/worker-debug` starts with
    /// `spiffe://td/ns/tankovault/sa/worker`, and a prefix match would let the first answer for
    /// the second.
    #[must_use]
    pub fn matches(&self, expected: &PeerIdentity) -> bool {
        match expected {
            PeerIdentity::Dns(name) => self.dns.iter().any(|s| s == name),
            PeerIdentity::Spiffe(id) => self.uris.iter().any(|s| s == id),
        }
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
    // Before the first rustls call in the function, not left to the caller: every `main`
    // installs the provider too, but this is the one rustls object the workspace builds itself,
    // and reaching `ServerConfig::builder()` without one is a panic rather than an error.
    crate::crypto::install_default_provider();

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

    // `builder`, not `builder_with_provider`: the provider installed process-wide above is the
    // one the rest of the stack (reqwest, sqlx) already negotiated with.
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
    /// The private key, **PKCS#8 PEM** whatever the mounted file held — see [`pkcs8_pem`]. Not a
    /// `secrecy` type: it never leaves this struct as text, and the TLS builders that consume it
    /// take `&[u8]`.
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
/// [`TlsError::Read`] naming whichever path could not be read, or [`TlsError::Key`] when the key
/// is not one [`pkcs8_pem`] can normalise.
pub fn client_material(paths: &ResolvedTls) -> Result<ClientMaterial, TlsError> {
    let read = |path: &Path| {
        std::fs::read(path).map_err(|source| TlsError::Read {
            path: path.display().to_string(),
            source,
        })
    };
    Ok(ClientMaterial {
        cert: read(&paths.cert)?,
        key: pkcs8_pem(&read(&paths.key)?, &paths.key)?,
        ca: read(&paths.ca)?,
    })
}

/// `rsaEncryption`, RFC 8017 appendix A.1.
const RSA_ENCRYPTION: pkcs8::ObjectIdentifier =
    pkcs8::ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");

/// `id-ecPublicKey`, RFC 5480 §2.1.1.
const ID_EC_PUBLIC_KEY: pkcs8::ObjectIdentifier =
    pkcs8::ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");

/// Re-encode a PEM private key as PKCS#8, whatever encoding the operator mounted.
///
/// The three TLS stacks that consume this one file disagree about what a private key may look
/// like. rustls (the internal listener) and reqwest (the api tier) accept PKCS#8, PKCS#1 and
/// SEC1; `wreq::tls::trust::Identity::from_pkcs8_pem`, which the worker's solver hop in
/// `tankovault-fetch` runs on, tests the *first bytes* against the `PRIVATE KEY` banner PKCS#8
/// carries and refuses everything else. cert-manager's default `privateKey.encoding` is PKCS#1, as is
/// `openssl genrsa`, and `openssl ecparam -genkey` writes SEC1 — so a mount that satisfied the
/// documented contract and every other consumer took the worker down at boot with `expected
/// PKCS#8 PEM`. Normalising once, where the file is read, is what keeps the stacks agreeing.
///
/// Re-encoding an already-PKCS#8 key is deliberate, not a wasted round trip: it also strips a
/// leading `Bag Attributes` preamble or comment, which some tooling emits and which defeats the
/// same prefix test.
///
/// # Errors
/// [`TlsError::Key`] if `pem` holds no private key, or an elliptic-curve key that names no curve
/// (RFC 5480 forbids the implicit and explicit forms, and there is nothing to wrap it with).
fn pkcs8_pem(pem: &[u8], path: &Path) -> Result<Vec<u8>, TlsError> {
    fn encode(info: &pkcs8::PrivateKeyInfo<'_>) -> Result<Zeroizing<String>, String> {
        let doc = pkcs8::SecretDocument::try_from(info).map_err(|e| e.to_string())?;
        doc.to_pem(pkcs8::PrivateKeyInfo::PEM_LABEL, pkcs8::LineEnding::LF)
            .map_err(|e| e.to_string())
    }

    /// RFC 5915 §1: the curve moves into the PKCS#8 algorithm identifier, and repeating it
    /// inside the wrapped `ECPrivateKey` is redundant, so it is dropped on the way through.
    fn wrap_sec1(der: &[u8]) -> Result<Zeroizing<String>, String> {
        let mut ec = sec1::EcPrivateKey::from_der(der).map_err(|e| e.to_string())?;
        let curve = ec
            .parameters
            .and_then(sec1::EcParameters::named_curve)
            .ok_or_else(|| "the elliptic-curve key names no curve".to_owned())?;
        ec.parameters = None;
        let inner = Zeroizing::new(ec.to_der().map_err(|e| e.to_string())?);
        encode(&pkcs8::PrivateKeyInfo {
            algorithm: pkcs8::AlgorithmIdentifierRef {
                oid: ID_EC_PUBLIC_KEY,
                parameters: Some((&curve).into()),
            },
            private_key: &inner,
            public_key: None,
        })
    }

    let key = PrivateKeyDer::from_pem_slice(pem).map_err(|e| TlsError::Key {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;

    let normalised = match &key {
        PrivateKeyDer::Pkcs8(key) => pkcs8::PrivateKeyInfo::try_from(key.secret_pkcs8_der())
            .map_err(|e| e.to_string())
            .and_then(|info| encode(&info)),
        PrivateKeyDer::Pkcs1(key) => encode(&pkcs8::PrivateKeyInfo {
            algorithm: pkcs8::AlgorithmIdentifierRef {
                oid: RSA_ENCRYPTION,
                parameters: Some(pkcs8::der::asn1::AnyRef::NULL),
            },
            private_key: key.secret_pkcs1_der(),
            public_key: None,
        }),
        PrivateKeyDer::Sec1(key) => wrap_sec1(key.secret_sec1_der()),
        // `PrivateKeyDer` is `#[non_exhaustive]`; a variant added upstream is not silently
        // mis-wrapped under one of the algorithm identifiers above.
        _ => Err("unrecognised private key encoding".to_owned()),
    };

    normalised
        .map(|pem| pem.as_bytes().to_vec())
        .map_err(|reason| TlsError::Key {
            path: path.display().to_string(),
            reason,
        })
}

/// Every DNS and URI subject alternative name in `cert`, kept apart.
///
/// URI names are read because that is where SPIRE puts a workload identity: an X.509-SVID
/// carries `spiffe://<trust-domain>/<path>` in a URI SAN, and no DNS SAN at all unless one was
/// requested at registration.
fn peer_sans(cert: &CertificateDer<'_>) -> PeerSans {
    use x509_parser::extensions::GeneralName;
    let Ok((_, parsed)) = x509_parser::parse_x509_certificate(cert) else {
        return PeerSans::default();
    };
    let Ok(Some(san)) = parsed.subject_alternative_name() else {
        return PeerSans::default();
    };

    let mut dns = Vec::new();
    let mut uris = Vec::new();
    for name in &san.value.general_names {
        match name {
            GeneralName::DNSName(value) => dns.push((*value).to_owned()),
            GeneralName::URI(value) => uris.push((*value).to_owned()),
            _ => {}
        }
    }
    PeerSans::new(dns, uris)
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
            .map(peer_sans)
            .unwrap_or_default();

        Self { addr, sans }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dns_only(names: &[&str]) -> PeerSans {
        PeerSans::new(names.iter().map(|s| (*s).to_owned()).collect(), Vec::new())
    }

    /// A certificate carries several names and the useful one is rarely first. Matching only
    /// `[0]` would make `internal.peers.<name>.san` depend on the order cert-manager happened
    /// to emit, which is not something an operator can see from the manifest they wrote.
    #[test]
    fn peer_sans_matches_any_name_the_certificate_proved() {
        let sans = dns_only(&["api", "api.tankovault", "api.tankovault.svc"]);
        assert!(sans.matches(&PeerIdentity::Dns("api".to_owned())));
        assert!(sans.matches(&PeerIdentity::Dns("api.tankovault.svc".to_owned())));
        assert!(!sans.matches(&PeerIdentity::Dns("worker".to_owned())));
        assert!(!sans.matches(&PeerIdentity::Dns(
            "api.tankovault.svc.cluster.local".to_owned()
        )));
    }

    /// A SPIFFE ID is matched against URI names, and a DNS name against DNS names — never
    /// across.
    ///
    /// The bug this pins: with both kinds in one list, a certificate from *any* authority in the
    /// trust bundle could carry `spiffe://…` as a URI (or, worse, as a DNS name) and answer for
    /// a SPIRE workload. The trust bundle is not a single authority — it is every authority a
    /// peer is allowed to chain to — so "some SAN matched" is not the same claim as "the SPIFFE
    /// ID matched".
    #[test]
    fn a_name_of_the_wrong_kind_never_satisfies_a_peer() {
        const ID: &str = "spiffe://tankovault.prod/ns/tankovault/sa/api";

        // The SPIFFE ID smuggled in as a DNS name must not satisfy the SPIFFE expectation.
        let forged = dns_only(&[ID]);
        assert!(!forged.matches(&PeerIdentity::Spiffe(ID.to_owned())));

        // Nor does a genuine SVID satisfy a DNS expectation it never proved.
        let svid = PeerSans::new(Vec::new(), vec![ID.to_owned()]);
        assert!(svid.matches(&PeerIdentity::Spiffe(ID.to_owned())));
        assert!(!svid.matches(&PeerIdentity::Dns("api.tankovault.svc".to_owned())));
    }

    /// SPIFFE IDs are hierarchical, so one is routinely a prefix of another. Matching by prefix
    /// would let `…/sa/worker-debug` authenticate as `…/sa/worker`.
    #[test]
    fn a_spiffe_id_is_matched_whole_not_by_prefix() {
        let svid = PeerSans::new(
            Vec::new(),
            vec!["spiffe://tankovault.prod/ns/tankovault/sa/worker-debug".to_owned()],
        );
        assert!(!svid.matches(&PeerIdentity::Spiffe(
            "spiffe://tankovault.prod/ns/tankovault/sa/worker".to_owned()
        )));
    }

    #[test]
    fn an_empty_or_unparsable_certificate_yields_no_names_rather_than_panicking() {
        for bytes in [vec![], vec![0x30, 0x00]] {
            let sans = peer_sans(&CertificateDer::from(bytes));
            assert!(!sans.matches(&PeerIdentity::Dns("api".to_owned())));
            assert!(!sans.matches(&PeerIdentity::Spiffe("spiffe://td/x".to_owned())));
        }
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

    /// `prime256v1`, RFC 5480 §2.1.1.1.1 — the curve of the generated key below.
    const PRIME256V1: pkcs8::ObjectIdentifier =
        pkcs8::ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");

    fn pem(label: &str, der: &[u8]) -> Vec<u8> {
        pkcs8::der::pem::encode_string(label, pkcs8::LineEnding::LF, der)
            .expect("the label is valid PEM")
            .into_bytes()
    }

    fn normalise(key: &[u8]) -> Vec<u8> {
        pkcs8_pem(key, Path::new("/tls/tls.key")).expect("the key normalises")
    }

    /// The opening banner `wreq` tests for, assembled from the label rather than written out: a
    /// PEM private-key header spelled in full anywhere in the tree is what the secret scan is
    /// looking for, and it cannot tell this one from a leaked key.
    fn banner() -> Vec<u8> {
        format!("-----BEGIN {}-----", pkcs8::PrivateKeyInfo::PEM_LABEL).into_bytes()
    }

    /// A SEC1 elliptic-curve key — what `openssl ecparam -genkey` and cert-manager's default
    /// `privateKey.encoding` write — used to take the worker down at boot: the solver client
    /// runs on `wreq`, whose `Identity::from_pkcs8_pem` tests the first bytes of the key against
    /// the PKCS#8 banner and panicked the process with `expected PKCS#8 PEM`, while rustls and
    /// reqwest had already accepted the very same mount.
    ///
    /// The fixture is a real generated key rather than a committed one, and the assertion is
    /// byte equality against the PKCS#8 that produced it: an independent implementation decides
    /// what the rewrap should have emitted, down to the algorithm identifier and the stripped
    /// parameters. The result is then loaded through rustls, so it is a key that signs and not
    /// merely a blob with the right header.
    #[test]
    fn a_sec1_elliptic_curve_key_is_rewrapped_into_the_pkcs8_it_came_from() {
        let generated = ring::signature::EcdsaKeyPair::generate_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_ASN1_SIGNING,
            &ring::rand::SystemRandom::new(),
        )
        .expect("a P-256 key generates");
        let expected = pem("PRIVATE KEY", generated.as_ref());

        // The SEC1 file an operator mounts carries the curve inside the structure, which is
        // where `openssl ec` and cert-manager put it and where PKCS#8 does not.
        let info = pkcs8::PrivateKeyInfo::try_from(generated.as_ref()).expect("a PKCS#8 key");
        let mut inner = sec1::EcPrivateKey::from_der(info.private_key).expect("a SEC1 key inside");
        inner.parameters = Some(sec1::EcParameters::NamedCurve(PRIME256V1));
        let normalised = normalise(&pem(
            "EC PRIVATE KEY",
            &inner.to_der().expect("the fixture encodes"),
        ));

        assert!(
            normalised.starts_with(&banner()),
            "wreq tests exactly this prefix: {}",
            String::from_utf8_lossy(&normalised)
        );
        assert_eq!(
            normalised, expected,
            "the rewrap is not the original PKCS#8"
        );

        crate::crypto::install_default_provider();
        let parsed = PrivateKeyDer::from_pem_slice(&normalised).expect("the output is a key");
        rustls::crypto::ring::sign::any_supported_type(&parsed).expect("the key is usable");
    }

    /// The RSA half of the same bug: `openssl genrsa` writes PKCS#1, and so does cert-manager
    /// for an RSA certificate. The key bytes are opaque to the rewrap — the algorithm identifier
    /// is the whole of what it decides, and naming the wrong one produces a key no peer can use.
    #[test]
    fn a_pkcs1_rsa_key_is_rewrapped_under_the_rsa_encryption_algorithm_identifier() {
        let pkcs1 = b"an opaque RSAPrivateKey body";
        let normalised = normalise(&pem("RSA PRIVATE KEY", pkcs1));

        assert!(normalised.starts_with(&banner()));
        let parsed = PrivateKeyDer::from_pem_slice(&normalised).expect("the output is a key");
        let info = pkcs8::PrivateKeyInfo::try_from(match &parsed {
            PrivateKeyDer::Pkcs8(key) => key.secret_pkcs8_der(),
            other => panic!("expected PKCS#8, got {other:?}"),
        })
        .expect("the output is a PrivateKeyInfo");
        assert_eq!(info.algorithm.oid, RSA_ENCRYPTION);
        assert_eq!(
            info.algorithm.parameters,
            Some(pkcs8::der::asn1::AnyRef::NULL),
            "RFC 8017 A.1 requires the explicit NULL"
        );
        assert_eq!(info.private_key, pkcs1);
    }

    /// A key that is already PKCS#8 still goes through the rewrap, because the prefix test is on
    /// the *first bytes* of the file: a `Bag Attributes` preamble or a leading comment — both of
    /// which some tooling emits and every PEM parser skips — fails it just as a PKCS#1 key does.
    #[test]
    fn an_already_pkcs8_key_loses_a_preamble_that_would_defeat_the_prefix_test() {
        let inner = pkcs8::PrivateKeyInfo {
            algorithm: pkcs8::AlgorithmIdentifierRef {
                oid: RSA_ENCRYPTION,
                parameters: Some(pkcs8::der::asn1::AnyRef::NULL),
            },
            private_key: b"an opaque RSAPrivateKey body",
            public_key: None,
        };
        let canonical = normalise(
            pkcs8::SecretDocument::try_from(&inner)
                .expect("the fixture encodes")
                .to_pem(pkcs8::PrivateKeyInfo::PEM_LABEL, pkcs8::LineEnding::LF)
                .expect("the fixture encodes")
                .as_bytes(),
        );

        let mut with_preamble = b"Bag Attributes\n    friendlyName: worker\n".to_vec();
        with_preamble.extend_from_slice(&canonical);

        assert_eq!(normalise(&with_preamble), canonical);
        assert!(canonical.starts_with(&banner()));
    }

    /// The failure has to name the path and stay an error. It is reached while the configuration
    /// resolves, where the operator can still be told which of three mounts is wrong; a panic
    /// there is a crash-looping replica with a message about PEM and no file name in it.
    #[test]
    fn a_key_that_cannot_be_normalised_names_its_path_rather_than_panicking() {
        let err = pkcs8_pem(b"-----BEGIN CERTIFICATE-----\n", Path::new("/tls/tls.key"))
            .expect_err("a certificate is not a private key");
        assert!(err.to_string().contains("/tls/tls.key"), "{err}");
    }
}
