use config::ConfigError;

const fn default_true() -> bool {
    true
}

/// Minimum length for any configured secret.
const MIN_SECRET_LENGTH: usize = 32;

/// Placeholder values shipped in the example configuration and documentation.
/// A server started with one of these has no authentication at all, because
/// anyone can forge tokens for any account.
const PLACEHOLDER_SECRETS: [&str; 5] = [
    "changeme",
    "changeit",
    "secret",
    "someverylongstring64",
    "supersecret",
];

#[derive(serde::Deserialize, Clone)]
pub struct Config {
    #[serde(rename = "secret_key")]
    pub secret: String,
    /// Secret used to derive account name hashes. Kept separate from
    /// `secret_key` so that the token signing secret can be rotated without
    /// making every existing account unreachable. Falls back to `secret_key`
    /// for deployments created before the two were split.
    #[serde(default)]
    username_secret: Option<String>,
    /// Whether to derive the rate limiting client address from
    /// `X-Forwarded-For` instead of the immediate peer.
    ///
    /// Enable this only when the server is reachable exclusively through a
    /// reverse proxy. Behind a proxy every request arrives from the proxy's own
    /// address, so without this all clients share a single rate limit bucket.
    /// If the server is directly reachable, leaving this off is what stops a
    /// client from forging its own address.
    #[serde(default)]
    pub trust_forwarded_for: bool,
    #[serde(default = "default_true")]
    pub allow_registration: bool,
    #[serde(default = "default_true")]
    pub validate_submitted_metadata: bool,
    pub database_url: String,
    #[serde(default)]
    pub migration_approval: Option<String>,
}

impl Config {
    /// Secret used to derive account name hashes.
    ///
    /// Note that changing this value makes all existing accounts unreachable,
    /// since accounts are looked up by `HMAC(name)`. Only `secret_key` can be
    /// rotated safely, and only once `username_secret` is set explicitly.
    pub fn username_secret(&self) -> &str {
        self.dedicated_username_secret().unwrap_or(&self.secret)
    }

    /// The account name secret, if pinned independently of `secret_key`.
    ///
    /// A blank value counts as unset, so that an empty `username_secret=` in an
    /// env file or a commented-out sample config falls back to `secret_key`
    /// rather than failing validation.
    fn dedicated_username_secret(&self) -> Option<&str> {
        self.username_secret
            .as_deref()
            .map(str::trim)
            .filter(|secret| !secret.is_empty())
    }

    /// Whether the account name secret is pinned independently of `secret_key`.
    pub fn has_dedicated_username_secret(&self) -> bool {
        self.dedicated_username_secret().is_some()
    }
}

fn validate_secret(name: &str, secret: &str) -> Result<(), ConfigError> {
    if PLACEHOLDER_SECRETS.contains(&secret.to_ascii_lowercase().trim()) {
        return Err(ConfigError::Message(format!(
            "{name} is set to the placeholder value {secret:?}. Generate a unique secret, \
             e.g. with `openssl rand -hex 32`, before exposing this server."
        )));
    }

    if secret.len() < MIN_SECRET_LENGTH {
        return Err(ConfigError::Message(format!(
            "{name} is too short ({} bytes); at least {MIN_SECRET_LENGTH} are required. \
             Generate one with `openssl rand -hex 32`.",
            secret.len()
        )));
    }

    Ok(())
}

fn validate_config(config: &Config) -> Result<(), ConfigError> {
    validate_secret("secret_key", &config.secret)?;
    if let Some(username_secret) = config.dedicated_username_secret() {
        validate_secret("username_secret", username_secret)?;
    }

    Ok(())
}

pub fn build_config() -> Result<Config, ConfigError> {
    let config: Config = config::Config::builder()
        .add_source(config::File::with_name("config").required(false))
        .add_source(config::Environment::with_convert_case(config::Case::Snake))
        .build()?
        .try_deserialize()?;

    validate_config(&config)?;

    if !config.has_dedicated_username_secret() {
        log::warn!(
            "username_secret is not set, so account name hashes are derived from secret_key. \
             This means secret_key cannot be rotated without locking out every account. \
             Set username_secret to the current secret_key value to pin it."
        );
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::{Config, validate_config, validate_secret};

    fn config_with(secret: &str, username_secret: Option<&str>) -> Config {
        Config {
            secret: secret.to_owned(),
            username_secret: username_secret.map(str::to_owned),
            trust_forwarded_for: false,
            allow_registration: true,
            validate_submitted_metadata: true,
            database_url: "./db.sqlite".to_owned(),
            migration_approval: None,
        }
    }

    const STRONG: &str = "6f1c2f0e8a4b5d3c7e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d";

    #[test]
    fn placeholder_secrets_are_rejected() {
        assert!(validate_secret("secret_key", "changeme").is_err());
        // rejected regardless of casing, and even though it is long enough
        assert!(validate_secret("secret_key", "SomeVeryLongString64").is_err());
    }

    #[test]
    fn short_secrets_are_rejected() {
        assert!(validate_secret("secret_key", "short").is_err());
        assert!(validate_secret("secret_key", &"a".repeat(31)).is_err());
        assert!(validate_secret("secret_key", &"a".repeat(32)).is_ok());
    }

    #[test]
    fn username_secret_falls_back_to_secret_key() {
        let config = config_with(STRONG, None);
        assert_eq!(config.username_secret(), STRONG);
        assert!(!config.has_dedicated_username_secret());
    }

    /// A blank value must fall back rather than fail validation, so that an
    /// empty `USERNAME_SECRET=` in an env file is not a startup error.
    #[test]
    fn blank_username_secret_counts_as_unset() {
        for blank in ["", "   "] {
            let config = config_with(STRONG, Some(blank));
            assert_eq!(config.username_secret(), STRONG);
            assert!(!config.has_dedicated_username_secret());
            assert!(validate_config(&config).is_ok());
        }
    }

    #[test]
    fn username_secret_is_used_and_validated_when_set() {
        let other = "0d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d7c6b5a4f3e2d1c";
        let config = config_with(STRONG, Some(other));
        assert_eq!(config.username_secret(), other);
        assert!(config.has_dedicated_username_secret());
        assert!(validate_config(&config).is_ok());

        assert!(validate_config(&config_with(STRONG, Some("changeme"))).is_err());
    }
}
