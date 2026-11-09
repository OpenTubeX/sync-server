use config::ConfigError;
use openidconnect::{IssuerUrl, RedirectUrl};

const fn default_true() -> bool {
    true
}

/// One proxy is the common case when `trust_forwarded_for` is enabled.
const fn default_proxy_hops() -> usize {
    1
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
pub struct OidcConfig {
    pub provider_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub app_url: String,
    /// Client URLs the OIDC flows may redirect back to. Login passes the session
    /// token to this URL, so only list clients you trust.
    #[serde(default, deserialize_with = "string_list")]
    pub allowed_redirect_urls: Vec<String>,
    /// Provider name that clients show on their login button, e.g. `Authelia`.
    #[serde(default)]
    pub display_name: Option<String>,
}

/// Accepts a list, or a comma-separated string so that lists can be set
/// through environment variables.
fn string_list<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum StringList {
        List(Vec<String>),
        Joined(String),
    }

    Ok(match serde::Deserialize::deserialize(deserializer)? {
        StringList::List(list) => list,
        StringList::Joined(joined) => joined
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect(),
    })
}

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
    /// Whether to derive the rate limiting and access logging client address from
    /// `X-Forwarded-For` instead of the immediate peer.
    ///
    /// Enable this only when the server is reachable exclusively through a
    /// reverse proxy. Behind a proxy every request arrives from the proxy's own
    /// address, so without this all clients share a single rate limit bucket.
    /// If the server is directly reachable, leaving this off is what stops a
    /// client from forging its own address.
    #[serde(default)]
    pub trust_forwarded_for: bool,
    /// How many proxies sit in front of this server.
    ///
    /// Each appends to `X-Forwarded-For`, so with two hops the last entry is the
    /// inner proxy's address rather than the client's. Only entries appended by
    /// your own proxies can be trusted; anything further left is client-supplied.
    #[serde(default = "default_proxy_hops")]
    pub trusted_proxy_hops: usize,
    #[serde(default = "default_true")]
    pub allow_registration: bool,
    #[serde(default = "default_true")]
    pub validate_submitted_metadata: bool,
    pub database_url: String,
    #[serde(default)]
    pub privacy_policy_url: Option<String>,
    #[serde(default)]
    pub migration_approval: Option<String>,
    #[serde(default)]
    pub oidc: Option<OidcConfig>,
}

impl Config {
    /// Constructs the config using the `config.toml` file and environment variables.
    pub fn build() -> Result<Config, ConfigError> {
        config::Config::builder()
            .add_source(config::File::with_name("config").required(false))
            .add_source(environment())
            .build()?
            .try_deserialize()
    }

    /// Validates the config, collecting every problem so they can be reported at once.
    pub fn validate(&self) -> Result<(), Vec<ConfigValidationError>> {
        let mut errors = Vec::new();

        if let Some(value) = &self.privacy_policy_url {
            let valid = url::Url::parse(value).is_ok_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.has_host()
                    && url.username().is_empty()
                    && url.password().is_none()
            });
            if !valid {
                errors.push(ConfigValidationError::InvalidPrivacyPolicyUrl);
            }
        }

        errors.extend(validate_secret("secret_key", &self.secret).err());
        if let Some(username_secret) = self.dedicated_username_secret() {
            errors.extend(validate_secret("username_secret", username_secret).err());
        }

        if let Some(oidc) = &self.oidc {
            if is_placeholder(&oidc.client_id) {
                errors.push(ConfigValidationError::DefaultSecret("oidc.client_id"));
            }
            if is_placeholder(&oidc.client_secret) {
                errors.push(ConfigValidationError::DefaultSecret("oidc.client_secret"));
            }
            if IssuerUrl::new(oidc.provider_url.clone()).is_err() {
                errors.push(ConfigValidationError::InvalidUrl("oidc.provider_url"));
            }
            if RedirectUrl::new(oidc.app_url.clone()).is_err() {
                errors.push(ConfigValidationError::InvalidUrl("oidc.app_url"));
            }
        }

        if self.database_url.trim().is_empty() {
            errors.push(ConfigValidationError::InvalidUrl("database_url"));
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

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

/// Environment variable source. Nested options use `__` as the separator,
/// e.g. `OIDC__CLIENT_ID` for `oidc.client_id`.
fn environment() -> config::Environment {
    config::Environment::with_convert_case(config::Case::Snake).separator("__")
}

/// Invalid config errors.
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum ConfigValidationError {
    /// A secret is blank or left at a placeholder value.
    #[error(
        "`{0}` is unset or a placeholder value. Generate a unique secret, \
         e.g. with `openssl rand -hex 32`, before exposing this server."
    )]
    DefaultSecret(&'static str),
    /// A secret is too short to resist brute force.
    #[error(
        "`{0}` is too short ({1} bytes); at least {MIN_SECRET_LENGTH} are required. \
         Generate one with `openssl rand -hex 32`."
    )]
    ShortSecret(&'static str, usize),
    /// The set URL is not valid.
    #[error("`{0}` is not a valid URL")]
    InvalidUrl(&'static str),
    #[error("`privacy_policy_url` must be an absolute HTTP(S) URL without credentials")]
    InvalidPrivacyPolicyUrl,
}

fn is_placeholder(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || PLACEHOLDER_SECRETS.contains(&value.to_ascii_lowercase().as_str())
}

fn validate_secret(name: &'static str, secret: &str) -> Result<(), ConfigValidationError> {
    if is_placeholder(secret) {
        return Err(ConfigValidationError::DefaultSecret(name));
    }

    if secret.len() < MIN_SECRET_LENGTH {
        return Err(ConfigValidationError::ShortSecret(name, secret.len()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Config, ConfigValidationError, OidcConfig, environment, validate_secret};

    fn config_with(secret: &str, username_secret: Option<&str>) -> Config {
        Config {
            secret: secret.to_owned(),
            username_secret: username_secret.map(str::to_owned),
            trust_forwarded_for: false,
            trusted_proxy_hops: 1,
            allow_registration: true,
            validate_submitted_metadata: true,
            database_url: "./db.sqlite".to_owned(),
            privacy_policy_url: None,
            migration_approval: None,
            oidc: None,
        }
    }

    const STRONG: &str = "6f1c2f0e8a4b5d3c7e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0c1d";

    #[test]
    fn privacy_policy_url_is_optional_and_must_be_a_web_url() {
        let mut config = config_with(STRONG, None);
        assert!(config.validate().is_ok());
        for value in ["https://example.org/privacy", "http://localhost/privacy"] {
            config.privacy_policy_url = Some(value.to_owned());
            assert!(config.validate().is_ok());
        }
        for value in [
            "",
            "/privacy",
            "javascript:alert(1)",
            "file:///tmp/policy",
            "https://user:secret@example.org",
        ] {
            config.privacy_policy_url = Some(value.to_owned());
            assert!(config.validate().is_err(), "{value}");
        }
    }

    /// Asserts on the variant, not just that validation failed: every
    /// placeholder is also shorter than the minimum, so `is_err()` alone would
    /// still pass if the placeholder matching were removed entirely.
    #[test]
    fn placeholder_secrets_are_rejected() {
        for placeholder in [
            "",
            "   ",
            "changeme",
            "ChAnGeMe",
            "SomeVeryLongString64",
            "SUPERSECRET",
        ] {
            assert_eq!(
                validate_secret("secret_key", placeholder),
                Err(ConfigValidationError::DefaultSecret("secret_key")),
                "{placeholder:?}"
            );
        }
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
            assert!(config.validate().is_ok());
        }
    }

    #[test]
    fn username_secret_is_used_and_validated_when_set() {
        let other = "0d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d7c6b5a4f3e2d1c";
        let config = config_with(STRONG, Some(other));
        assert_eq!(config.username_secret(), other);
        assert!(config.has_dedicated_username_secret());
        assert!(config.validate().is_ok());

        assert!(config_with(STRONG, Some("changeme")).validate().is_err());
    }

    #[test]
    fn all_errors_are_reported_together() {
        let mut config = config_with("changeme", None);
        config.database_url = String::new();
        config.oidc = Some(OidcConfig {
            provider_url: "not a url".to_owned(),
            client_id: "changeme".to_owned(),
            client_secret: String::new(),
            app_url: String::new(),
            allowed_redirect_urls: Vec::new(),
            display_name: None,
        });
        assert_eq!(
            config.validate(),
            Err(vec![
                ConfigValidationError::DefaultSecret("secret_key"),
                ConfigValidationError::DefaultSecret("oidc.client_id"),
                ConfigValidationError::DefaultSecret("oidc.client_secret"),
                ConfigValidationError::InvalidUrl("oidc.provider_url"),
                ConfigValidationError::InvalidUrl("oidc.app_url"),
                ConfigValidationError::InvalidUrl("database_url"),
            ])
        );
    }

    #[test]
    fn valid_oidc_config_is_accepted() {
        let mut config = config_with(STRONG, None);
        config.oidc = Some(OidcConfig {
            provider_url: "https://auth.example.com".to_owned(),
            client_id: "sync-server".to_owned(),
            client_secret: STRONG.to_owned(),
            app_url: "https://sync.example.com".to_owned(),
            allowed_redirect_urls: Vec::new(),
            display_name: None,
        });
        assert!(config.validate().is_ok());
    }

    /// Builds a config from the given variables instead of the process
    /// environment, so tests cannot race on shared state.
    fn config_from_env(vars: &[(&str, &str)]) -> Config {
        let vars = vars
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        config::Config::builder()
            .add_source(environment().source(Some(vars)))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap()
    }

    #[test]
    fn environment_variables_configure_nested_oidc_options() {
        let config = config_from_env(&[
            ("SECRET_KEY", STRONG),
            ("DATABASE_URL", "./data/db.sqlite"),
            ("ALLOW_REGISTRATION", "false"),
            ("TRUSTED_PROXY_HOPS", "2"),
            ("OIDC__PROVIDER_URL", "https://auth.example.com"),
            ("OIDC__CLIENT_ID", "sync-server"),
            ("OIDC__CLIENT_SECRET", STRONG),
            ("OIDC__APP_URL", "https://sync.example.com"),
            (
                "OIDC__ALLOWED_REDIRECT_URLS",
                "myapp://oidc, https://app.example.com/oidc",
            ),
            ("OIDC__DISPLAY_NAME", "Authelia"),
        ]);

        assert_eq!(config.secret, STRONG);
        assert_eq!(config.database_url, "./data/db.sqlite");
        assert!(!config.allow_registration);
        assert_eq!(config.trusted_proxy_hops, 2);
        let oidc = config.oidc.expect("OIDC__* variables should set `oidc`");
        assert_eq!(oidc.provider_url, "https://auth.example.com");
        assert_eq!(oidc.client_id, "sync-server");
        assert_eq!(oidc.client_secret, STRONG);
        assert_eq!(oidc.app_url, "https://sync.example.com");
        assert_eq!(
            oidc.allowed_redirect_urls,
            ["myapp://oidc", "https://app.example.com/oidc"]
        );
        assert_eq!(oidc.display_name.as_deref(), Some("Authelia"));
    }

    /// Variables override single options of a `config.toml` section, and the
    /// file's TOML array form for lists keeps working.
    #[test]
    fn environment_variables_override_the_config_file() {
        let file = r#"
            secret_key = "from-file"
            database_url = "./db.sqlite"

            [oidc]
            provider_url = "https://auth.example.com"
            client_id = "from-file"
            client_secret = "from-file"
            app_url = "https://sync.example.com"
            allowed_redirect_urls = ["myapp://oidc", "https://app.example.com/oidc"]
        "#;
        let vars = [("OIDC__CLIENT_ID".to_owned(), "from-env".to_owned())]
            .into_iter()
            .collect();
        let config: Config = config::Config::builder()
            .add_source(config::File::from_str(file, config::FileFormat::Toml))
            .add_source(environment().source(Some(vars)))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();

        let oidc = config.oidc.unwrap();
        assert_eq!(oidc.client_id, "from-env");
        assert_eq!(oidc.client_secret, "from-file");
        assert_eq!(
            oidc.allowed_redirect_urls,
            ["myapp://oidc", "https://app.example.com/oidc"]
        );
    }
}
