use std::{collections::BTreeMap, env, fs, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::domain::root_folder::RootFolderHint;

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub server: ServerConfig,
    pub sonarr: SonarrConfig,
    pub llm: LlmConfig,
    pub metadata: MetadataConfig,
    pub classification: ClassificationConfig,
    pub database: DatabaseConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind_address: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct SonarrConfig {
    pub base_url: String,
    pub api_key: String,
    pub webhook_token: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub provider: LlmProvider,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub timeout_seconds: u64,
    pub temperature: f32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum LlmProvider {
    #[default]
    Ollama,
    OpenAiCompatible,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct MetadataConfig {
    pub tmdb_bearer_token: Option<String>,
    pub tvdb_api_key: Option<String>,
    pub tvdb_pin: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ClassificationConfig {
    pub min_confidence: f64,
    pub root_folders: BTreeMap<String, RootFolderHint>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub sqlite_path: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = env::var("ROOTERR_CONFIG").unwrap_or_else(|_| "rooterr.toml".to_string());
        let mut config = if PathBuf::from(&config_path).exists() {
            let raw = fs::read_to_string(&config_path)
                .with_context(|| format!("failed to read config file {config_path}"))?;
            toml::from_str::<Config>(&raw)
                .with_context(|| format!("failed to parse config file {config_path}"))?
        } else {
            Config::default()
        };

        config.apply_env()?;
        config.validate()?;
        Ok(config)
    }

    fn apply_env(&mut self) -> Result<()> {
        set_string("ROOTERR_SERVER_BIND_ADDRESS", &mut self.server.bind_address);
        set_string("ROOTERR_SONARR_BASE_URL", &mut self.sonarr.base_url);
        set_string("ROOTERR_SONARR_API_KEY", &mut self.sonarr.api_key);
        set_option_string(
            "ROOTERR_SONARR_WEBHOOK_TOKEN",
            &mut self.sonarr.webhook_token,
        );

        if let Ok(provider) = env::var("ROOTERR_LLM_PROVIDER") {
            self.llm.provider = match provider.as_str() {
                "ollama" => LlmProvider::Ollama,
                "openai_compatible" => LlmProvider::OpenAiCompatible,
                other => bail!(
                    "ROOTERR_LLM_PROVIDER must be 'ollama' or 'openai_compatible', got '{other}'"
                ),
            };
        }
        set_string("ROOTERR_LLM_BASE_URL", &mut self.llm.base_url);
        set_string("ROOTERR_LLM_MODEL", &mut self.llm.model);
        set_option_string("ROOTERR_LLM_API_KEY", &mut self.llm.api_key);
        set_u64("ROOTERR_LLM_TIMEOUT_SECONDS", &mut self.llm.timeout_seconds)?;
        set_f32("ROOTERR_LLM_TEMPERATURE", &mut self.llm.temperature)?;

        set_option_string(
            "ROOTERR_TMDB_BEARER_TOKEN",
            &mut self.metadata.tmdb_bearer_token,
        );
        set_option_string("ROOTERR_TVDB_API_KEY", &mut self.metadata.tvdb_api_key);
        set_option_string("ROOTERR_TVDB_PIN", &mut self.metadata.tvdb_pin);

        if let Ok(path) = env::var("ROOTERR_DATABASE_SQLITE_PATH") {
            self.database.sqlite_path = PathBuf::from(path);
        }
        set_f64(
            "ROOTERR_CLASSIFICATION_MIN_CONFIDENCE",
            &mut self.classification.min_confidence,
        )?;

        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.sonarr.api_key.trim().is_empty() {
            bail!("sonarr.api_key is required");
        }

        if self.llm.model.trim().is_empty() {
            bail!("llm.model is required");
        }

        if !(0.0..=1.0).contains(&self.classification.min_confidence) {
            bail!("classification.min_confidence must be between 0.0 and 1.0");
        }

        Ok(())
    }
}

impl LlmConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds)
    }
}


impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "0.0.0.0:9898".to_string(),
        }
    }
}

impl Default for SonarrConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8989".to_string(),
            api_key: String::new(),
            webhook_token: None,
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: LlmProvider::Ollama,
            base_url: "http://localhost:11434".to_string(),
            model: "gemma3:270m-it-qat".to_string(),
            api_key: None,
            timeout_seconds: 60,
            temperature: 0.0,
        }
    }
}


impl Default for ClassificationConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.55,
            root_folders: BTreeMap::new(),
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            sqlite_path: PathBuf::from("./rooterr.sqlite3"),
        }
    }
}

fn set_string(key: &str, target: &mut String) {
    if let Ok(value) = env::var(key) {
        *target = value;
    }
}

fn set_option_string(key: &str, target: &mut Option<String>) {
    if let Ok(value) = env::var(key) {
        *target = if value.trim().is_empty() {
            None
        } else {
            Some(value)
        };
    }
}

fn set_u64(key: &str, target: &mut u64) -> Result<()> {
    if let Ok(value) = env::var(key) {
        *target = value
            .parse()
            .with_context(|| format!("{key} must be an integer"))?;
    }
    Ok(())
}

fn set_f32(key: &str, target: &mut f32) -> Result<()> {
    if let Ok(value) = env::var(key) {
        *target = value
            .parse()
            .with_context(|| format!("{key} must be a float"))?;
    }
    Ok(())
}

fn set_f64(key: &str, target: &mut f64) -> Result<()> {
    if let Ok(value) = env::var(key) {
        *target = value
            .parse()
            .with_context(|| format!("{key} must be a float"))?;
    }
    Ok(())
}
