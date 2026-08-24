//! A reader's global provider order — the fallback the per-series pin overrides.

use crate::error::DbResult;
use sqlx::{PgExecutor, PgPool};
use tankovault_domain::{ProviderId, UserId};
use uuid::Uuid;

/// One provider in a reader's priority list, richest end of the list first.
#[derive(Debug, Clone)]
pub struct RankedProvider {
    /// The provider's id.
    pub id: Uuid,
    /// Its stable slug, which is what a stored preference is keyed on.
    pub slug: String,
    /// Its display name, as an operator set it.
    pub name: String,
}

/// Read a reader's provider priority, most preferred first.
///
/// Disabled providers are dropped rather than returned: they carry nothing a reader can open,
/// so listing them would invite ranking a source that can never win.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn get_provider_priority<'e, E: PgExecutor<'e>>(
    exec: E,
    user_id: UserId,
) -> DbResult<Vec<RankedProvider>> {
    let rows = sqlx::query_as!(
        RankedProvider,
        "SELECT p.id, p.slug, p.name \
         FROM user_provider_priority upp \
         JOIN providers p ON p.id = upp.provider_id \
         WHERE upp.user_id = $1 AND p.state <> 'disabled' \
         ORDER BY upp.position",
        user_id.as_uuid(),
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Replace a reader's provider priority with `providers`, in the order given.
///
/// Replace rather than merge: the list *is* the preference, and an empty slice clears it. The
/// delete and the insert share one transaction because the unique index on `(user_id, position)`
/// is checked per statement — a partial write would leave two providers claiming one rank.
///
/// Callers are expected to have rejected unknown or duplicated ids already; both reach here as
/// a constraint violation (a 500) rather than something this layer can explain.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn set_provider_priority(
    pool: &PgPool,
    user_id: UserId,
    providers: &[ProviderId],
) -> DbResult<()> {
    let ids: Vec<Uuid> = providers.iter().map(|p| p.as_uuid()).collect();
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "DELETE FROM user_provider_priority WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO user_provider_priority (user_id, provider_id, position) \
         SELECT $1, t.provider_id, (t.ordinality - 1)::int \
         FROM unnest($2::uuid[]) WITH ORDINALITY AS t(provider_id, ordinality)",
        user_id.as_uuid(),
        &ids,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The providers this reader has opted into early access for.
///
/// Presence in `user_provider_early_access` is the whole preference — see migration 0047 on why
/// there is no boolean column and therefore no way for the table to disagree with itself.
///
/// # Errors
/// [`crate::DbError::Sqlx`] only.
pub async fn get_early_access_providers(pool: &PgPool, user_id: UserId) -> DbResult<Vec<Uuid>> {
    let rows = sqlx::query_scalar!(
        "SELECT provider_id FROM user_provider_early_access WHERE user_id = $1 \
         ORDER BY provider_id",
        user_id.as_uuid(),
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Replace the reader's early-access opt-ins with exactly `providers`.
///
/// Wholesale replacement inside one transaction, like the priority list above: the request body
/// *is* the preference, so a provider left out of it is opted out afterwards. Doing it as a diff
/// would need the client to know the current state to express "turn this one off".
///
/// # Errors
/// [`crate::DbError::Sqlx`] only. Callers validate the ids against the public provider list
/// first, so a foreign-key violation here means the provider was retired mid-request.
pub async fn set_early_access_providers(
    pool: &PgPool,
    user_id: UserId,
    providers: &[ProviderId],
) -> DbResult<()> {
    let ids: Vec<Uuid> = providers.iter().map(|p| p.as_uuid()).collect();
    let mut tx = pool.begin().await?;
    sqlx::query!(
        "DELETE FROM user_provider_early_access WHERE user_id = $1",
        user_id.as_uuid(),
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO user_provider_early_access (user_id, provider_id) \
         SELECT $1, p FROM unnest($2::uuid[]) AS p",
        user_id.as_uuid(),
        &ids,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
