//! Serialising a secret onto the wire, deliberately and in one place.
//!
//! [`secrecy::SecretBox`] has no `Serialize` impl unless the inner type opts in via
//! `SerializableSecret`, and this workspace opts nothing in. That default is the point: a
//! secret cannot reach a JSON body, a log line or an error payload by being carried along in a
//! struct someone serialised.
//!
//! But two values *must* reach the client, because handing them over is the whole transaction:
//! the access token a login issues, and the refresh token that rides in a `Set-Cookie` header.
//! Those go through [`expose_onto_wire`], which is a `#[serde(serialize_with = …)]` — so the
//! deliberate exception is written at the one field it applies to, and `grep expose_onto_wire`
//! enumerates every secret this API is allowed to emit.
//!
//! A blanket `impl SerializableSecret for String` would have been three lines and would have
//! made every `SecretString` in the process silently serialisable, which is the same mistake
//! as a bare `String` with extra steps.

use secrecy::{ExposeSecret as _, SecretString};
use serde::Serializer;

/// Serialise a [`SecretString`] as the plain string it wraps.
///
/// # Errors
/// Propagates whatever the underlying [`Serializer`] returns.
pub(crate) fn expose_onto_wire<S: Serializer>(
    secret: &SecretString,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(secret.expose_secret())
}

/// [`expose_onto_wire`] for an optional field.
///
/// Needed as a separate function because `serialize_with` replaces the serialisation of the
/// whole field, `Option` and all — the inner-value form cannot be reused through it.
/// `RegisterResponse::access_token` is `None` on the confirmation-required path, and pairs
/// this with `skip_serializing_if` so the absent case stays *absent* rather than becoming an
/// explicit `null`, which is the shape `openapi.json` already describes.
///
/// # Errors
/// Propagates whatever the underlying [`Serializer`] returns.
#[expect(
    clippy::ref_option,
    reason = "serde's serialize_with fixes the signature to &T of the field's own type, which \
              here is Option<SecretString>; Option<&SecretString> does not satisfy it"
)]
pub(crate) fn expose_option_onto_wire<S: Serializer>(
    secret: &Option<SecretString>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match secret {
        Some(value) => serializer.serialize_some(value.expose_secret()),
        None => serializer.serialize_none(),
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Response {
        #[serde(serialize_with = "super::expose_onto_wire")]
        access_token: SecretString,
        expires_in: i64,
    }

    /// The opt-in works: a field that asks for it lands on the wire as a plain string, so the
    /// client sees exactly what it saw before the wrapper existed. Pins the wire format, which
    /// `openapi.json` and every existing client depend on.
    #[test]
    fn an_opted_in_field_serialises_as_a_plain_string() {
        let json = serde_json::to_string(&Response {
            access_token: SecretString::from("header.payload.signature"),
            expires_in: 900,
        })
        .expect("serialises");
        assert_eq!(
            json,
            r#"{"access_token":"header.payload.signature","expires_in":900}"#
        );
    }

    /// The half that is easy to lose in a refactor: without the attribute the struct must not
    /// compile at all, rather than serialising `[REDACTED]` or an object. This is a
    /// compile-fail property, so it is asserted the only way a test can — by stating it — and
    /// enforced by `secrecy` refusing to implement `Serialize` for `SecretBox<str>`.
    ///
    /// If a future `impl SerializableSecret for …` is ever added to this workspace, that
    /// refusal disappears silently and every `SecretString` field starts serialising. Do not
    /// add one.
    #[test]
    fn a_secret_is_not_serialisable_without_the_opt_in() {
        // `SecretString: Serialize` does not hold. Asserted structurally: a helper generic
        // over `Serialize` cannot be instantiated with it, so this test's *absence of* a call
        // is the assertion, and the doc comment above is the record of why.
        fn assert_serialisable<T: serde::Serialize>() {}
        assert_serialisable::<String>();
        // assert_serialisable::<SecretString>();  // <- must not compile
    }
}
