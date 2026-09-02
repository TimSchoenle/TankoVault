//! Authentication for service-to-service calls on the internal network.

use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use terrace_config::schema::Describe;

use crate::ConfigError;

/// How a callee establishes *which* service is calling it.
///
/// The mode changes only that one question. Everything downstream — which caller may reach
/// which route, what a refusal looks like, what gets audited — is identical in all three, so a
/// deployment cannot be authorised differently by virtue of how it proves identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Describe)]
#[serde(rename_all = "snake_case")]
pub enum IdentityMode {
    /// No identity at all: every caller is anonymous and every route is open.
    ///
    /// Local development and tests only; refused under `TANKOVAULT_PROFILE=production`.
    #[default]
    Off,
    /// A per-caller bearer token. The portable mode — compose, bare metal, anything without a
    /// certificate authority.
    Token,
    /// A verified client certificate; the caller is its SAN. Certificates come from files, so
    /// this works anywhere they can be written — cert-manager and trust-manager mount them in
    /// Kubernetes, `openssl` or `step-ca` produce them elsewhere.
    Mtls,
}

impl IdentityMode {
    /// The spelling an operator writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Token => "token",
            Self::Mtls => "mtls",
        }
    }
}

impl std::fmt::Display for IdentityMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who *this* service is when it calls another. Absent on a service that calls nobody.
#[derive(Debug, Clone, Default, Deserialize, Describe)]
pub struct CallerConfig {
    /// The name peers know this service by; must match the key they list it under.
    #[serde(default)]
    pub name: Option<String>,
    /// Presented to peers under `identity = "token"`. Ignored under `mtls`, where the client
    /// certificate is the credential.
    #[config(secret)]
    #[serde(default)]
    pub token: Option<SecretString>,
}

/// One service permitted to call this one. Absent on a service nobody calls.
#[derive(Debug, Clone, Default, Deserialize, Describe)]
pub struct PeerConfig {
    /// The token this peer presents under `identity = "token"`.
    #[config(secret)]
    #[serde(default)]
    pub token: Option<SecretString>,
    /// The client-certificate subject alternative name this peer presents under
    /// `identity = "mtls"`, e.g. `api.tankovault.svc`.
    #[serde(default)]
    pub san: Option<String>,
}

/// Where the mTLS material lives. Paths, not contents: whatever writes them — cert-manager, a
/// mounted Secret, a hand-rolled CA — is outside this process's concern.
#[derive(Debug, Clone, Deserialize, Describe)]
pub struct InternalTlsConfig {
    /// PEM certificate chain this service serves and presents.
    #[serde(default)]
    pub cert: Option<PathBuf>,
    /// PEM private key for [`Self::cert`].
    #[serde(default)]
    pub key: Option<PathBuf>,
    /// PEM bundle of the authorities a peer certificate must chain to.
    #[serde(default)]
    pub ca: Option<PathBuf>,
    /// Address the credential-free `/health` and `/ready` probes bind to, in **plaintext**, on
    /// their own listener (default `0.0.0.0:9091`). Read under `mtls` only.
    ///
    /// An orchestrator probe presents no client certificate, so on the mTLS port its plain
    /// `GET` is answered with a TLS alert rather than a response and the replica is restarted
    /// as unhealthy. `null` drops the listener, which is right only where whatever does the
    /// probing *can* present a certificate.
    #[serde(default = "InternalTlsConfig::default_probe_listen")]
    pub probe_listen: Option<String>,
}

impl InternalTlsConfig {
    // Must return `Option<String>` to match the field's serde-default signature; unwrapping
    // as clippy suggests would break that.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "must match the Option<String> field it defaults"
    )]
    fn default_probe_listen() -> Option<String> {
        Some("0.0.0.0:9091".to_owned())
    }
}

// Hand-written rather than derived so both ways this struct is produced agree: serde applies
// the field default when `internal.tls` is present, `Default` when the whole table is absent,
// and a derived `Default` would quietly mean "no probe listener".
impl Default for InternalTlsConfig {
    fn default() -> Self {
        Self {
            cert: None,
            key: None,
            ca: None,
            probe_listen: Self::default_probe_listen(),
        }
    }
}

/// Authentication for service-to-service calls on the internal network.
///
/// Privileged internal routes (sync state, scan triggers, arbitrary-URL fetch) are reachable by
/// service name from anywhere on the network, so every call carries a per-caller credential and
/// every callee decides, per route, which callers it accepts. See [`InternalAuthConfig::resolve`].
#[derive(Debug, Clone, Default, Deserialize, Describe)]
pub struct InternalAuthConfig {
    /// How callers are identified. See [`IdentityMode`].
    #[config(values)]
    #[serde(default)]
    pub identity: IdentityMode,
    /// Who this service is when it calls a peer.
    #[config(nested)]
    #[serde(default)]
    pub caller: CallerConfig,
    /// Which services may call this one, keyed by caller name.
    #[config(element)]
    #[serde(default)]
    pub peers: BTreeMap<String, PeerConfig>,
    /// Certificate material for `identity = "mtls"`.
    #[config(nested)]
    #[serde(default)]
    pub tls: InternalTlsConfig,
    /// The retired tier-wide shared secret. Present only so a deployment that still sets it is
    /// told what to do instead; see [`InternalAuthConfig::resolve`].
    #[config(secret)]
    #[serde(default)]
    pub token: Option<SecretString>,
}

/// The shortest token accepted. 32 bytes of hex is the documented recipe; anything
/// materially shorter is guessable at internal-network request rates.
pub const MIN_INTERNAL_TOKEN_LEN: usize = 32;

/// This service's own identity when calling a peer.
#[derive(Debug, Clone)]
pub struct ResolvedCaller {
    /// The name this service announces itself under, which a peer matches its allowlist on.
    pub name: String,
    /// `None` under `mtls`, where the client certificate is the credential.
    pub token: Option<SecretString>,
}

/// One caller this service accepts.
#[derive(Debug, Clone)]
pub struct ResolvedPeer {
    /// The name the caller announces, matched exactly.
    pub name: String,
    /// Set under `token`; `None` under `mtls`.
    pub token: Option<SecretString>,
    /// Set under `mtls`; `None` under `token`.
    pub san: Option<String>,
}

/// Certificate paths, all three present.
#[derive(Debug, Clone)]
pub struct ResolvedTls {
    /// This service's own certificate, PEM.
    pub cert: PathBuf,
    /// Its private key, PEM, unencrypted.
    pub key: PathBuf,
    /// The authorities a client certificate is verified against, PEM, one or many.
    pub ca: PathBuf,
}

/// A validated internal-auth configuration: the mode plus exactly the material that mode needs.
#[derive(Debug, Clone)]
pub struct ResolvedInternalAuth {
    /// Which of the three schemes this deployment settled on.
    pub mode: IdentityMode,
    /// This service's outbound identity, absent for a service that calls no peer.
    pub caller: Option<ResolvedCaller>,
    /// Who may call this service. Empty accepts nobody, which is the correct posture for a
    /// service exposing no internal route.
    pub peers: Vec<ResolvedPeer>,
    /// The certificate material, present under `mtls` and only there.
    pub tls: Option<ResolvedTls>,
    /// Plaintext address for the health probes, set under `mtls` only — every other mode
    /// already answers them on the main listener.
    pub probe_listen: Option<String>,
}

impl ResolvedInternalAuth {
    /// The peer configuration for `name`, if it is permitted to call this service.
    #[must_use]
    pub fn peer(&self, name: &str) -> Option<&ResolvedPeer> {
        self.peers.iter().find(|p| p.name == name)
    }
}

impl InternalAuthConfig {
    /// Validate into the material the active mode actually needs.
    ///
    /// Each mode is checked for its *own* requirements rather than a union of all three, so a
    /// half-migrated deployment — mTLS paths set but peers still carrying tokens, say — fails at
    /// boot naming the missing key instead of running with an identity nobody verified.
    ///
    /// # Errors
    /// [`ConfigError::Invalid`] when the retired `internal.token` is still set, when the mode is
    /// `off` under the production profile, when a token is shorter than
    /// [`MIN_INTERNAL_TOKEN_LEN`], or when the active mode's required fields are missing.
    pub fn resolve(&self, production: bool) -> Result<ResolvedInternalAuth, ConfigError> {
        if self.token.is_some() {
            return Err(ConfigError::Invalid(
                "internal.token is no longer read: one secret shared by every service meant any \
                 one of them could call all the others' privileged routes. Give each caller its \
                 own credential instead — set internal.identity (token|mtls), internal.caller.\
                 {name,token} on services that call a peer, and internal.peers.<name>.{token|san} \
                 on services that are called. docs/CONFIGURATION.md has the per-service table."
                    .to_owned(),
            ));
        }

        if self.identity == IdentityMode::Off && production {
            return Err(ConfigError::Invalid(
                "internal.identity=off leaves every privileged internal route reachable by \
                 anything that can open a socket, which TANKOVAULT_PROFILE=production refuses. \
                 Set internal.identity to `token` or `mtls`."
                    .to_owned(),
            ));
        }

        let caller = self.resolve_caller()?;
        let peers = self.resolve_peers()?;
        let tls = self.resolve_tls()?;
        // Tied to the mode, not merely to the key being set: outside `mtls` the probes are
        // already reachable on the main listener, and a second copy of them would be a
        // surface nobody asked for.
        let probe_listen = tls
            .is_some()
            .then(|| self.tls.probe_listen.clone())
            .flatten();

        Ok(ResolvedInternalAuth {
            mode: self.identity,
            caller,
            peers,
            tls,
            probe_listen,
        })
    }

    /// This service's outbound identity, or `None` when it calls nobody.
    fn resolve_caller(&self) -> Result<Option<ResolvedCaller>, ConfigError> {
        let name = self
            .caller
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty());
        let token = self.caller.token.as_ref();

        match (self.identity, name, token) {
            // Two ways to have no outbound identity, and neither is an error: `off` has none by
            // definition, and a service that calls nobody (control-plane, sync, the two solver
            // hosts) configures none.
            (IdentityMode::Off, _, _) | (_, None, None) => Ok(None),
            (_, None, Some(_)) => Err(ConfigError::Invalid(
                "internal.caller.token is set but internal.caller.name is not; a peer matches an \
                 inbound token to a name, so an unnamed caller can never be authorised"
                    .to_owned(),
            )),
            (IdentityMode::Mtls, Some(name), _) => Ok(Some(ResolvedCaller {
                name: name.to_owned(),
                token: None,
            })),
            (IdentityMode::Token, Some(name), Some(token)) => Ok(Some(ResolvedCaller {
                name: name.to_owned(),
                token: Some(check_token(
                    token,
                    &format!("internal.caller.token ({name})"),
                )?),
            })),
            (IdentityMode::Token, Some(name), None) => Err(ConfigError::Invalid(format!(
                "internal.caller.name is `{name}` but internal.caller.token is unset, and \
                 internal.identity=token has nothing else to present. Generate one with \
                 `openssl rand -hex 32`."
            ))),
        }
    }

    /// The callers this service accepts, in a stable order.
    fn resolve_peers(&self) -> Result<Vec<ResolvedPeer>, ConfigError> {
        if self.identity == IdentityMode::Off {
            return Ok(Vec::new());
        }

        let mut peers = Vec::with_capacity(self.peers.len());
        for (raw_name, peer) in &self.peers {
            let name = raw_name.trim();
            if name.is_empty() {
                return Err(ConfigError::Invalid(
                    "internal.peers has an entry with an empty name".to_owned(),
                ));
            }

            match self.identity {
                IdentityMode::Token => {
                    let token = peer.token.as_ref().ok_or_else(|| {
                        ConfigError::Invalid(format!(
                            "internal.peers.{name}.token is unset and internal.identity=token \
                             has no other way to recognise that caller"
                        ))
                    })?;
                    peers.push(ResolvedPeer {
                        name: name.to_owned(),
                        token: Some(check_token(token, &format!("internal.peers.{name}.token"))?),
                        san: None,
                    });
                }
                IdentityMode::Mtls => {
                    let san = peer
                        .san
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| {
                            ConfigError::Invalid(format!(
                                "internal.peers.{name}.san is unset and internal.identity=mtls \
                                 recognises a caller only by its certificate's subject \
                                 alternative name"
                            ))
                        })?;
                    peers.push(ResolvedPeer {
                        name: name.to_owned(),
                        token: None,
                        san: Some(san.to_owned()),
                    });
                }
                IdentityMode::Off => unreachable!("returned above"),
            }
        }
        Ok(peers)
    }

    /// Certificate paths, required together under `mtls` and ignored otherwise.
    fn resolve_tls(&self) -> Result<Option<ResolvedTls>, ConfigError> {
        if self.identity != IdentityMode::Mtls {
            return Ok(None);
        }
        let missing: Vec<&str> = [
            ("internal.tls.cert", self.tls.cert.is_none()),
            ("internal.tls.key", self.tls.key.is_none()),
            ("internal.tls.ca", self.tls.ca.is_none()),
        ]
        .into_iter()
        .filter_map(|(key, absent)| absent.then_some(key))
        .collect();

        if !missing.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "internal.identity=mtls needs all of internal.tls.{{cert,key,ca}}; missing: {}",
                missing.join(", ")
            )));
        }

        Ok(Some(ResolvedTls {
            cert: self.tls.cert.clone().expect("checked above"),
            key: self.tls.key.clone().expect("checked above"),
            ca: self.tls.ca.clone().expect("checked above"),
        }))
    }
}

/// Trim, reject empty, and enforce the length floor, naming `key` in the error.
///
/// The *length* of a secret is not itself secret — it is what the operator must change — so it
/// stays in the message. The value never does.
fn check_token(token: &SecretString, key: &str) -> Result<SecretString, ConfigError> {
    let trimmed = token.expose_secret().trim();
    if trimmed.is_empty() {
        return Err(ConfigError::Invalid(format!("{key} is empty")));
    }
    if trimmed.len() < MIN_INTERNAL_TOKEN_LEN {
        return Err(ConfigError::Invalid(format!(
            "{key} must be at least {MIN_INTERNAL_TOKEN_LEN} characters, got {}",
            trimmed.len()
        )));
    }
    Ok(SecretString::from(trimmed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_of(len: usize) -> SecretString {
        SecretString::from("t".repeat(len))
    }

    fn with_peer(identity: IdentityMode, peer: PeerConfig) -> InternalAuthConfig {
        InternalAuthConfig {
            identity,
            peers: BTreeMap::from([("api".to_owned(), peer)]),
            ..Default::default()
        }
    }

    /// The retired key must not be silently ignored. A deployment that upgrades while still
    /// setting `internal.token` would otherwise boot with `identity=off` — every privileged
    /// route open — while its config still looks like it is authenticating callers.
    #[test]
    fn the_retired_shared_token_is_refused_rather_than_ignored() {
        let cfg = InternalAuthConfig {
            token: Some(token_of(40)),
            ..Default::default()
        };
        let err = cfg.resolve(false).expect_err("the retired key must refuse");
        let msg = err.to_string();
        assert!(msg.contains("internal.identity"), "{msg}");
        assert!(msg.contains("internal.peers"), "{msg}");
    }

    #[test]
    fn off_is_refused_in_production_only() {
        let cfg = InternalAuthConfig::default();
        assert!(cfg.resolve(false).is_ok());
        assert!(cfg.resolve(true).is_err());
    }

    /// Each mode needs its own material. Carrying a token into `mtls` (or a SAN into `token`)
    /// is a half-finished migration, and booting on it would authorise nobody while looking
    /// configured.
    #[test]
    fn a_peer_must_carry_the_credential_its_mode_verifies() {
        let token_peer = PeerConfig {
            token: Some(token_of(40)),
            san: None,
        };
        let san_peer = PeerConfig {
            token: None,
            san: Some("api.tankovault.svc".to_owned()),
        };

        assert!(
            with_peer(IdentityMode::Token, token_peer.clone())
                .resolve(false)
                .is_ok()
        );
        assert!(
            with_peer(IdentityMode::Mtls, san_peer.clone())
                .resolve(false)
                .is_err()
        );
        assert!(
            with_peer(IdentityMode::Token, san_peer)
                .resolve(false)
                .is_err()
        );

        // ...and mTLS additionally needs the certificate paths.
        let mut mtls = with_peer(IdentityMode::Mtls, token_peer);
        assert!(mtls.resolve(false).is_err(), "a token is not a SAN");
        mtls.peers.get_mut("api").expect("seeded above").san = Some("api.svc".to_owned());
        let err = mtls
            .resolve(false)
            .expect_err("cert paths are still missing");
        assert!(err.to_string().contains("internal.tls"), "{err}");
    }

    #[test]
    fn a_short_token_is_refused_and_the_error_names_the_key() {
        let cfg = with_peer(
            IdentityMode::Token,
            PeerConfig {
                token: Some(token_of(8)),
                san: None,
            },
        );
        let err = cfg
            .resolve(false)
            .expect_err("8 characters is under the floor");
        let msg = err.to_string();
        assert!(msg.contains("internal.peers.api.token"), "{msg}");
        assert!(msg.contains("32"), "{msg}");
    }

    /// A caller with a credential but no name can never be authorised: peers match an inbound
    /// token to a name, so this is a misconfiguration that would fail only at request time.
    #[test]
    fn a_credential_without_a_name_is_refused() {
        let cfg = InternalAuthConfig {
            identity: IdentityMode::Token,
            caller: CallerConfig {
                name: None,
                token: Some(token_of(40)),
            },
            ..Default::default()
        };
        assert!(cfg.resolve(false).is_err());
    }

    /// `off` short-circuits every other requirement, so tests and local runs need no material
    /// at all — but it must also not half-resolve one.
    #[test]
    fn off_resolves_to_no_material() {
        let resolved = InternalAuthConfig {
            identity: IdentityMode::Off,
            caller: CallerConfig {
                name: Some("api".to_owned()),
                token: Some(token_of(40)),
            },
            ..Default::default()
        }
        .resolve(false)
        .expect("off needs nothing");
        assert!(resolved.caller.is_none());
        assert!(resolved.peers.is_empty());
        assert!(resolved.tls.is_none());
        assert!(resolved.probe_listen.is_none());
    }

    /// The probes get a plaintext listener of their own under `mtls`, and under nothing else.
    ///
    /// A kubelet probe presents no client certificate, so on the mTLS listener its plain `GET
    /// /health` is answered with a TLS alert — `malformed HTTP response "\x15\x03\x03…"` — and
    /// every replica of every internal service was killed by its own startup probe.
    #[test]
    fn mtls_resolves_a_plaintext_probe_address_and_no_other_mode_does() {
        let mtls = InternalAuthConfig {
            identity: IdentityMode::Mtls,
            tls: InternalTlsConfig {
                cert: Some("/tls/tls.crt".into()),
                key: Some("/tls/tls.key".into()),
                ca: Some("/tls/ca.crt".into()),
                ..InternalTlsConfig::default()
            },
            ..Default::default()
        };
        let resolved = mtls.resolve(false).expect("all three paths are set");
        assert_eq!(resolved.probe_listen.as_deref(), Some("0.0.0.0:9091"));

        // The same certificate material under `token` resolves no probe listener: those probes
        // are answered on the main port, and a second copy would be surface nobody asked for.
        let token = InternalAuthConfig {
            identity: IdentityMode::Token,
            ..mtls.clone()
        };
        assert!(
            token
                .resolve(false)
                .expect("token needs no tls")
                .probe_listen
                .is_none()
        );

        // …and an operator whose probes can present a certificate opts out with `null`.
        let opted_out = InternalAuthConfig {
            tls: InternalTlsConfig {
                probe_listen: None,
                ..mtls.tls.clone()
            },
            ..mtls
        };
        assert!(
            opted_out
                .resolve(false)
                .expect("still valid")
                .probe_listen
                .is_none()
        );
    }
}
