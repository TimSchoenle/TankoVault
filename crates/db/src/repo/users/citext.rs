//! Binding a Rust string as Postgres `citext` instead of `text`.
//!
//! One type, in its own module because four of the six sibling modules compare against a
//! `citext` column and every one of them was wrong without it.

/// A `&str` bound as Postgres `citext` rather than `text`.
///
/// **Without this wrapper, every lookup by email or username is case-sensitive** — silently,
/// and in contradiction of the schema, which made both columns `citext` precisely so that
/// `Alice@Example.com` and `alice@example.com` are one address (migration `0001`, `0004`).
///
/// The mechanism is operator resolution. `citext` ships an *implicit* cast to `text` but only
/// an *assignment* cast back, so when a `text` value meets a `citext` column Postgres has one
/// legal choice: widen the column to `text` and use `text = text`. Comparison then honours
/// case. `sqlx` binds a Rust `&str` as `text`, so `WHERE email = $1` — which reads as
/// case-insensitive and which the offline `.sqlx` cache even records with a `citext`
/// parameter, because a *describe* leaves the parameter unspecified and lets Postgres infer
/// it — degrades to case-sensitive the moment it actually runs.
///
/// The damage was a total lockout rather than an inconvenience: registration refuses a second
/// casing (the unique index *is* `citext`), sign-in refuses the first (`find_credentials`),
/// and password reset and resend-confirmation both answer with the deliberate
/// anti-enumeration silence, so a user who typed their address with different capitalisation
/// than they registered it could neither sign in, nor recover, nor register again, and got no
/// message saying why.
///
/// Only *comparisons* need this. An `INSERT` binds `text` into a `citext` column through the
/// assignment cast, which stores the value verbatim and indexes it case-insensitively — there
/// is no operator to resolve, so nothing changes.
#[derive(Debug, Clone, Copy)]
pub struct CiText<'a>(pub &'a str);

impl sqlx::Type<sqlx::Postgres> for CiText<'_> {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("citext")
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        *ty == Self::type_info() || <&str as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for CiText<'_> {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as sqlx::Encode<'_, sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}
