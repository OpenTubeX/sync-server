use config::ConfigError;

const fn default_true() -> bool {
    true
}

#[derive(serde::Deserialize, Clone)]
pub struct Config {
    #[serde(rename = "secret_key")]
    pub secret: String,
    #[serde(default = "default_true")]
    pub allow_registration: bool,
    #[serde(default = "default_true")]
    pub validate_submitted_metadata: bool,
    pub database_url: String,
    #[serde(default)]
    pub migration_approval: Option<String>,
}

pub fn build_config() -> Result<Config, ConfigError> {
    config::Config::builder()
        .add_source(config::File::with_name("config").required(false))
        .add_source(config::Environment::with_convert_case(config::Case::Snake))
        .build()?
        .try_deserialize()
}
