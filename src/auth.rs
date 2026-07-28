use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::OsRng;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use hmac::{Hmac, KeyInit, Mac as _};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use sha2::Sha256;

use crate::dto::JwtClaims;
use crate::models::Account;

pub fn bytes_to_hex_string(bytes: &[u8]) -> String {
    String::from("0x")
        + &bytes
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<String>>()
            .join("")
}

/// How long a freshly minted token stays valid. Should be enough in most cases.
const TOKEN_TTL: Duration = Duration::from_hours(365 * 24);

/// Allowance for clock skew between minting and verifying.
const EXPIRY_LEEWAY: Duration = Duration::from_secs(5 * 60);

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn generate_jwt(account: &Account, secret_key: &[u8]) -> jsonwebtoken::errors::Result<String> {
    let key = EncodingKey::from_secret(secret_key);
    // `exp` is defined in seconds since the epoch, not milliseconds.
    let expiration_date = unix_now().saturating_add(TOKEN_TTL.as_secs());

    let claims = JwtClaims {
        sub: account.id.clone(),
        exp: expiration_date as usize,
    };
    encode(&Header::default(), &claims, &key)
}

/// Whether `exp` is close enough to now that this server could have minted it.
///
/// `Validation::default()` only rejects expiries in the past, with no upper
/// bound. Tokens minted before `exp` was corrected from milliseconds to seconds
/// carry values around the year 59000, so without this check they would stay
/// valid effectively forever and the expiry fix would only apply to new logins.
fn expiry_is_plausible(exp: u64, now: u64) -> bool {
    exp <= now
        .saturating_add(TOKEN_TTL.as_secs())
        .saturating_add(EXPIRY_LEEWAY.as_secs())
}

/// Returns the User ID on success.
pub fn verify_jwt(encoded_jwt: &str, secret_key: &[u8]) -> jsonwebtoken::errors::Result<String> {
    let key = DecodingKey::from_secret(secret_key);
    let claims: JwtClaims = decode(encoded_jwt.as_bytes(), &key, &Validation::default())?.claims;

    if !expiry_is_plausible(claims.exp as u64, unix_now()) {
        return Err(jsonwebtoken::errors::ErrorKind::InvalidToken.into());
    }

    Ok(claims.sub)
}

fn argon2_instance<'a>() -> Argon2<'a> {
    Argon2::default()
}

pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    argon2_instance()
        .hash_password(password.as_bytes(), &salt)
        .unwrap()
        .to_string()
}

pub fn verify_password(password: &str, password_hash: &str) -> bool {
    let Ok(password_hash) = PasswordHash::new(password_hash) else {
        return false;
    };
    argon2_instance()
        .verify_password(password.as_bytes(), &password_hash)
        .is_ok()
}

/// Generate HMAC of accountname. Usernames are not stored in plaintext for better anonymity.
pub fn hash_accountname(accountname: &str, secret_key: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(accountname.as_bytes()).unwrap();
    mac.update(secret_key);

    let result = &mac.finalize().into_bytes();
    bytes_to_hex_string(result)
}

#[cfg(test)]
mod tests {
    use super::{EXPIRY_LEEWAY, TOKEN_TTL, expiry_is_plausible};

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn normal_expiries_are_accepted() {
        assert!(expiry_is_plausible(NOW + 60, NOW));
        assert!(expiry_is_plausible(NOW + TOKEN_TTL.as_secs(), NOW));
        // a little clock skew is tolerated
        assert!(expiry_is_plausible(
            NOW + TOKEN_TTL.as_secs() + EXPIRY_LEEWAY.as_secs(),
            NOW
        ));
    }

    /// Tokens minted before `exp` was corrected carry milliseconds, which
    /// `Validation::default()` would happily accept for the next 57000 years.
    #[test]
    fn legacy_millisecond_expiries_are_rejected() {
        let legacy_exp = (NOW + TOKEN_TTL.as_secs()) * 1000;
        assert!(!expiry_is_plausible(legacy_exp, NOW));
    }

    #[test]
    fn expiries_beyond_the_ttl_are_rejected() {
        assert!(!expiry_is_plausible(
            NOW + TOKEN_TTL.as_secs() + EXPIRY_LEEWAY.as_secs() + 1,
            NOW
        ));
        assert!(!expiry_is_plausible(u64::MAX, NOW));
    }
}
