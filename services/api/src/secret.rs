//! Serialising a secret onto the wire, deliberately and in one place — no type here opts into
//! `SerializableSecret`, so a secret can't reach a JSON body by riding along in a struct
//! someone serialised. The two values that must reach the client (the access and refresh
//! tokens) go through [`expose_onto_wire`], the one exception `grep expose_onto_wire`
//! enumerates in full.

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

/// [`expose_onto_wire`] for an optional field — needed separately since `serialize_with`
/// replaces the whole field's serialisation, `Option` included, so the inner-value form can't
/// be reused. Paired with `skip_serializing_if` so an absent value stays absent rather than
/// becoming an explicit `null`.
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

/// [`expose_onto_wire`] for a list, as recovery codes are handed to their single display.
///
/// A separate helper for the same reason [`expose_option_onto_wire`] is one: `serialize_with`
/// replaces the whole field's serialisation, so the scalar form cannot be reused inside a `Vec`.
///
/// # Errors
/// Propagates whatever the underlying [`Serializer`] returns.
pub(crate) fn expose_list_onto_wire<S: Serializer>(
    secrets: &[SecretString],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq as _;
    let mut seq = serializer.serialize_seq(Some(secrets.len()))?;
    for secret in secrets {
        seq.serialize_element(secret.expose_secret())?;
    }
    seq.end()
}

/// [`expose_list_onto_wire`] for an optional list — recovery codes ride along only with the
/// registration that was the account's *first* factor.
///
/// # Errors
/// Propagates whatever the underlying [`Serializer`] returns.
#[expect(
    clippy::ref_option,
    reason = "serde's serialize_with fixes the signature to &T of the field's own type, which \
              here is Option<Vec<SecretString>>; Option<&[SecretString]> does not satisfy it"
)]
pub(crate) fn expose_optional_list_onto_wire<S: Serializer>(
    secrets: &Option<Vec<SecretString>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match secrets {
        Some(values) => expose_list_onto_wire(values, serializer),
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

    /// The opt-in works: a field that asks for it lands on the wire as a plain string. Pins
    /// the wire format that `openapi.json` and every client depend on.
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

    #[derive(Serialize)]
    struct Codes {
        #[serde(serialize_with = "super::expose_list_onto_wire")]
        codes: Vec<SecretString>,
    }

    /// A list opts in the same way, and lands as plain strings rather than as objects.
    #[test]
    fn an_opted_in_list_serialises_as_plain_strings() {
        let json = serde_json::to_string(&Codes {
            codes: vec![
                SecretString::from("AAAA-BBBB"),
                SecretString::from("CCCC-DDDD"),
            ],
        })
        .expect("serialises");
        assert_eq!(json, r#"{"codes":["AAAA-BBBB","CCCC-DDDD"]}"#);
    }

    /// The half easy to lose in a refactor: without the attribute the struct must not compile
    /// at all, rather than serialising `[REDACTED]` or an object — enforced by `secrecy`
    /// refusing `Serialize` for `SecretBox<str>`.
    ///
    /// A future `impl SerializableSecret for …` would make this refusal disappear silently.
    /// Do not add one.
    #[test]
    fn a_secret_is_not_serialisable_without_the_opt_in() {
        // `SecretString: Serialize` does not hold — asserted structurally: a helper generic
        // over `Serialize` can't be instantiated with it, so the absence of a call is the test.
        fn assert_serialisable<T: serde::Serialize>() {}
        assert_serialisable::<String>();
        // assert_serialisable::<SecretString>();  // <- must not compile
    }
}
