//! Binds a Rust string as Postgres `citext` instead of `text`, for comparisons against the
//! citext `email`/`username` columns.

/// A `&str` bound as Postgres `citext` rather than `text`.
///
/// `citext` has an implicit cast to `text` but only an assignment cast back, so a bare `&str`
/// (bound as `text`) widens the column to `text` and compares case-sensitively — silently, in
/// contradiction of the schema. Without this wrapper, `Alice@x.com` and `alice@x.com` stop
/// matching on lookup even though the unique index treats them as one address.
///
/// Only comparisons need this; an `INSERT` stores through the assignment cast regardless.
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
