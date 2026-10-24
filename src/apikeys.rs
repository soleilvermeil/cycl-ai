//! Authentication of the exposed API: clients hold an RSA private key, sign a
//! short-lived RS256 JWT with it (`kid` = the key's name), and the server only
//! stores the matching public keys (`api_keys` table). Revoking a key takes
//! effect on the next request — keys are read from the database every time.

use jsonwebtoken::{decode, decode_header, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

/// Longest accepted `exp - iat`, so a leaked token expires quickly.
pub const MAX_TOKEN_TTL_S: i64 = 300;
/// Tolerated clock drift between client and server.
const LEEWAY_S: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Read,
    Write,
}

impl Scope {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "read" => Ok(Scope::Read),
            "write" => Ok(Scope::Write),
            other => anyhow::bail!("unknown scope '{other}' (expected read or write)"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Write => "write",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    exp: i64,
    iat: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    /// Bad/expired/unknown token or revoked key — all answered with the same 401.
    Invalid(&'static str),
    /// Valid key, but a read-only key tried to write.
    Forbidden,
}

/// Registers a public key (PEM). Rejects anything that isn't a usable RSA key.
pub async fn add(pool: &SqlitePool, name: &str, public_key_pem: &str, scope: Scope) -> anyhow::Result<()> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        anyhow::bail!("key name must be non-empty and use only letters, digits, '-', '_' or '.'");
    }
    DecodingKey::from_rsa_pem(public_key_pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("not a valid RSA public key in PEM format: {e}"))?;
    sqlx::query("INSERT INTO api_keys (name, public_key_pem, scope) VALUES (?, ?, ?)")
        .bind(name)
        .bind(public_key_pem.trim())
        .bind(scope.as_str())
        .execute(pool)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(ref d) if d.is_unique_violation() => {
                anyhow::anyhow!("a key named '{name}' already exists (revoked keys keep their name)")
            }
            other => other.into(),
        })?;
    Ok(())
}

/// Revokes a key; returns false if there is no active key with that name.
pub async fn revoke(pool: &SqlitePool, name: &str) -> anyhow::Result<bool> {
    let done = sqlx::query("UPDATE api_keys SET revoked_at = datetime('now') WHERE name = ? AND revoked_at IS NULL")
        .bind(name)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() > 0)
}

pub async fn list(pool: &SqlitePool) -> anyhow::Result<Vec<(String, String, String, Option<String>, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT name, scope, created_at, last_used_at, revoked_at FROM api_keys ORDER BY id",
    )
    .fetch_all(pool)
    .await?)
}

/// Checks a bearer token for a request using `method`. On success returns the
/// key's name.
pub async fn authenticate(pool: &SqlitePool, token: &str, is_write: bool) -> Result<String, AuthError> {
    let header = decode_header(token).map_err(|_| AuthError::Invalid("malformed token"))?;
    if header.alg != Algorithm::RS256 {
        return Err(AuthError::Invalid("unsupported algorithm"));
    }
    let kid = header.kid.ok_or(AuthError::Invalid("missing kid"))?;

    let row: Option<(String, String, Option<String>)> =
        sqlx::query_as("SELECT public_key_pem, scope, revoked_at FROM api_keys WHERE name = ?")
            .bind(&kid)
            .fetch_optional(pool)
            .await
            .map_err(|e| {
                tracing::error!(?e, "api key lookup failed");
                AuthError::Invalid("lookup failed")
            })?;
    let (pem, scope, revoked_at) = row.ok_or(AuthError::Invalid("unknown key"))?;
    if revoked_at.is_some() {
        return Err(AuthError::Invalid("revoked key"));
    }
    let key = DecodingKey::from_rsa_pem(pem.as_bytes()).map_err(|_| AuthError::Invalid("stored key unusable"))?;

    let mut validation = Validation::new(Algorithm::RS256);
    validation.leeway = LEEWAY_S;
    validation.set_required_spec_claims(&["exp"]);
    let claims = decode::<Claims>(token, &key, &validation)
        .map_err(|_| AuthError::Invalid("bad signature or expired"))?
        .claims;
    let now = chrono::Utc::now().timestamp();
    if claims.iat > now + LEEWAY_S as i64 || claims.exp - claims.iat > MAX_TOKEN_TTL_S {
        return Err(AuthError::Invalid("token lifetime too long"));
    }

    if is_write && Scope::parse(&scope).ok() != Some(Scope::Write) {
        return Err(AuthError::Forbidden);
    }
    touch(pool, &kid).await;
    Ok(kid)
}

/// Records the last use, at most once a minute per key so reads don't turn
/// into writes competing with the dashboard process.
async fn touch(pool: &SqlitePool, name: &str) {
    let _ = sqlx::query(
        "UPDATE api_keys SET last_used_at = datetime('now') \
         WHERE name = ? AND (last_used_at IS NULL OR last_used_at < datetime('now', '-1 minute'))",
    )
    .bind(name)
    .execute(pool)
    .await;
}

/// Signs a token the way a client would (`cyclai api-key token`).
pub fn mint_token(private_key_pem: &str, kid: &str, ttl_s: i64) -> anyhow::Result<String> {
    if !(1..=MAX_TOKEN_TTL_S).contains(&ttl_s) {
        anyhow::bail!("ttl must be between 1 and {MAX_TOKEN_TTL_S} seconds");
    }
    let now = chrono::Utc::now().timestamp();
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.to_string());
    let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("not a valid RSA private key in PEM format: {e}"))?;
    Ok(encode(&header, &Claims { iat: now, exp: now + ttl_s }, &key)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    const PRIVATE: &str = include_str!("../tests/fixtures/test_rsa_private.pem");
    const PUBLIC: &str = include_str!("../tests/fixtures/test_rsa_public.pem");

    async fn pool_with_key(scope: Scope) -> SqlitePool {
        let pool = SqlitePoolOptions::new().connect("sqlite::memory:").await.unwrap();
        crate::db::migrate(&pool).await.unwrap();
        add(&pool, "laptop", PUBLIC, scope).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn valid_token_is_accepted() {
        let pool = pool_with_key(Scope::Read).await;
        let token = mint_token(PRIVATE, "laptop", 60).unwrap();
        assert_eq!(authenticate(&pool, &token, false).await, Ok("laptop".to_string()));
    }

    #[tokio::test]
    async fn unknown_kid_wrong_key_and_garbage_are_rejected() {
        let pool = pool_with_key(Scope::Read).await;
        let other_kid = mint_token(PRIVATE, "nobody", 60).unwrap();
        assert!(matches!(authenticate(&pool, &other_kid, false).await, Err(AuthError::Invalid(_))));
        assert!(matches!(authenticate(&pool, "not.a.jwt", false).await, Err(AuthError::Invalid(_))));

        // Same kid, but registered under a different public key: signature mismatch.
        let other_pem = include_str!("../tests/fixtures/test_rsa_public_other.pem");
        sqlx::query("UPDATE api_keys SET public_key_pem = ?").bind(other_pem).execute(&pool).await.unwrap();
        let token = mint_token(PRIVATE, "laptop", 60).unwrap();
        assert!(matches!(authenticate(&pool, &token, false).await, Err(AuthError::Invalid(_))));
    }

    #[tokio::test]
    async fn expired_and_overlong_tokens_are_rejected() {
        let pool = pool_with_key(Scope::Read).await;
        let now = chrono::Utc::now().timestamp();
        let sign = |iat: i64, exp: i64| {
            let mut h = Header::new(Algorithm::RS256);
            h.kid = Some("laptop".into());
            encode(&h, &Claims { iat, exp }, &EncodingKey::from_rsa_pem(PRIVATE.as_bytes()).unwrap()).unwrap()
        };
        assert!(authenticate(&pool, &sign(now - 600, now - 300), false).await.is_err());
        assert!(authenticate(&pool, &sign(now, now + 3600), false).await.is_err());
        assert!(authenticate(&pool, &sign(now + 600, now + 660), false).await.is_err());
    }

    #[tokio::test]
    async fn revoked_key_is_rejected_immediately() {
        let pool = pool_with_key(Scope::Read).await;
        let token = mint_token(PRIVATE, "laptop", 60).unwrap();
        assert!(authenticate(&pool, &token, false).await.is_ok());
        assert!(revoke(&pool, "laptop").await.unwrap());
        assert!(authenticate(&pool, &token, false).await.is_err());
        assert!(!revoke(&pool, "laptop").await.unwrap());
    }

    #[tokio::test]
    async fn read_key_cannot_write() {
        let pool = pool_with_key(Scope::Read).await;
        let token = mint_token(PRIVATE, "laptop", 60).unwrap();
        assert_eq!(authenticate(&pool, &token, true).await, Err(AuthError::Forbidden));
        sqlx::query("UPDATE api_keys SET scope = 'write'").execute(&pool).await.unwrap();
        assert!(authenticate(&pool, &token, true).await.is_ok());
    }

    #[tokio::test]
    async fn add_rejects_bad_names_bad_pem_and_duplicates() {
        let pool = pool_with_key(Scope::Read).await;
        assert!(add(&pool, "bad name", PUBLIC, Scope::Read).await.is_err());
        assert!(add(&pool, "ok", "not a pem", Scope::Read).await.is_err());
        assert!(add(&pool, "laptop", PUBLIC, Scope::Read).await.is_err());
    }
}
