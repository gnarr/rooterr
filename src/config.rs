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
    pub auto_pull: bool,
    pub startup_wait_seconds: u64,
    pub pull_timeout_seconds: u64,
    pub auto_num_ctx: bool,
    pub min_num_ctx: u32,
    pub max_num_ctx: u32,
    pub reserved_output_tokens: u32,
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
        set_bool("ROOTERR_LLM_AUTO_PULL", &mut self.llm.auto_pull)?;
        set_u64(
            "ROOTERR_LLM_STARTUP_WAIT_SECONDS",
            &mut self.llm.startup_wait_seconds,
        )?;
        set_u64(
            "ROOTERR_LLM_PULL_TIMEOUT_SECONDS",
            &mut self.llm.pull_timeout_seconds,
        )?;
        set_bool("ROOTERR_LLM_AUTO_NUM_CTX", &mut self.llm.auto_num_ctx)?;
        set_u32("ROOTERR_LLM_MIN_NUM_CTX", &mut self.llm.min_num_ctx)?;
        set_u32("ROOTERR_LLM_MAX_NUM_CTX", &mut self.llm.max_num_ctx)?;
        set_u32(
            "ROOTERR_LLM_RESERVED_OUTPUT_TOKENS",
            &mut self.llm.reserved_output_tokens,
        )?;
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
        if let Ok(root_folders) = env::var("ROOTERR_CLASSIFICATION_ROOT_FOLDERS_JSON") {
            self.classification.root_folders = serde_json::from_str(&root_folders)
                .context("ROOTERR_CLASSIFICATION_ROOT_FOLDERS_JSON must be a JSON object")?;
        }

        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.sonarr.api_key.trim().is_empty() {
            bail!("sonarr.api_key is required");
        }

        if self.llm.model.trim().is_empty() {
            bail!("llm.model is required");
        }

        if self.llm.min_num_ctx == 0 {
            bail!("llm.min_num_ctx must be greater than 0");
        }

        if self.llm.max_num_ctx > 0 && self.llm.max_num_ctx < self.llm.min_num_ctx {
            bail!("llm.max_num_ctx must be 0 or greater than or equal to llm.min_num_ctx");
        }

        if self.llm.reserved_output_tokens == 0 {
            bail!("llm.reserved_output_tokens must be greater than 0");
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

    pub fn startup_wait_timeout(&self) -> Duration {
        Duration::from_secs(self.startup_wait_seconds)
    }

    pub fn pull_timeout(&self) -> Duration {
        Duration::from_secs(self.pull_timeout_seconds)
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
            auto_pull: false,
            startup_wait_seconds: 60,
            pull_timeout_seconds: 900,
            auto_num_ctx: true,
            min_num_ctx: 4096,
            max_num_ctx: 0,
            reserved_output_tokens: 512,
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
            sqlite_path: PathBuf::from("./data/rooterr.sqlite3"),
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

fn set_bool(key: &str, target: &mut bool) -> Result<()> {
    if let Ok(value) = env::var(key) {
        let normalized = value.trim().to_ascii_lowercase();
        *target = match normalized.as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => bail!("{key} must be a boolean"),
        };
    }
    Ok(())
}

fn set_u64(key: &str, target: &mut u64) -> Result<()> {
    if let Ok(value) = env::var(key) {
        *target = value
            .parse()
            .with_context(|| format!("{key} must be an integer"))?;
    }
    Ok(())
}

fn set_u32(key: &str, target: &mut u32) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn llm_auto_pull_defaults_are_backward_compatible() {
        let config = Config::default();

        assert!(!config.llm.auto_pull);
        assert_eq!(config.llm.startup_wait_seconds, 60);
        assert_eq!(config.llm.pull_timeout_seconds, 900);
        assert!(config.llm.auto_num_ctx);
        assert_eq!(config.llm.min_num_ctx, 4096);
        assert_eq!(config.llm.max_num_ctx, 0);
        assert_eq!(config.llm.reserved_output_tokens, 512);
    }

    #[test]
    fn llm_auto_pull_fields_parse_from_toml() {
        let config = toml::from_str::<Config>(
            r#"
            [sonarr]
            api_key = "test-key"

            [llm]
            auto_pull = true
            startup_wait_seconds = 12
            pull_timeout_seconds = 34
            auto_num_ctx = false
            min_num_ctx = 8192
            max_num_ctx = 32768
            reserved_output_tokens = 1024
            "#,
        )
        .expect("parse config");

        assert!(config.llm.auto_pull);
        assert_eq!(config.llm.startup_wait_seconds, 12);
        assert_eq!(config.llm.pull_timeout_seconds, 34);
        assert!(!config.llm.auto_num_ctx);
        assert_eq!(config.llm.min_num_ctx, 8192);
        assert_eq!(config.llm.max_num_ctx, 32768);
        assert_eq!(config.llm.reserved_output_tokens, 1024);
    }

    #[test]
    fn env_overrides_scalar_config_values() {
        let _guard = lock_env();
        clear_rooterr_env();
        set_env("ROOTERR_SONARR_API_KEY", "env-sonarr-key");
        set_env("ROOTERR_SONARR_WEBHOOK_TOKEN", "");
        set_env("ROOTERR_LLM_PROVIDER", "openai_compatible");
        set_env("ROOTERR_LLM_MODEL", "env-model");
        set_env("ROOTERR_LLM_AUTO_PULL", "true");
        set_env("ROOTERR_LLM_TEMPERATURE", "0.25");
        set_env("ROOTERR_CLASSIFICATION_MIN_CONFIDENCE", "0.7");
        set_env("ROOTERR_DATABASE_SQLITE_PATH", "/tmp/rooterr-env.sqlite3");

        let mut config = toml::from_str::<Config>(
            r#"
            [sonarr]
            api_key = "toml-sonarr-key"
            webhook_token = "toml-token"

            [llm]
            provider = "ollama"
            model = "toml-model"
            auto_pull = false
            temperature = 0.0

            [classification]
            min_confidence = 0.55

            [database]
            sqlite_path = "/tmp/rooterr-toml.sqlite3"
            "#,
        )
        .expect("parse config");

        config.apply_env().expect("apply env");

        assert_eq!(config.sonarr.api_key, "env-sonarr-key");
        assert_eq!(config.sonarr.webhook_token, None);
        assert!(matches!(config.llm.provider, LlmProvider::OpenAiCompatible));
        assert_eq!(config.llm.model, "env-model");
        assert!(config.llm.auto_pull);
        assert_eq!(config.llm.temperature, 0.25);
        assert_eq!(config.classification.min_confidence, 0.7);
        assert_eq!(
            config.database.sqlite_path,
            PathBuf::from("/tmp/rooterr-env.sqlite3")
        );

        clear_rooterr_env();
    }

    #[test]
    fn env_root_folder_json_parses_hints() {
        let _guard = lock_env();
        clear_rooterr_env();
        set_env(
            "ROOTERR_CLASSIFICATION_ROOT_FOLDERS_JSON",
            r#"{
                "/data/kids": {
                    "label": "Kids",
                    "description": "Children's and family-oriented shows."
                },
                "/data/scripted": {
                    "label": "Scripted",
                    "description": "Default scripted television."
                }
            }"#,
        );

        let mut config = Config::default();
        config.apply_env().expect("apply env");

        assert_eq!(
            config.classification.root_folders["/data/kids"]
                .label
                .as_deref(),
            Some("Kids")
        );
        assert_eq!(
            config.classification.root_folders["/data/kids"]
                .description
                .as_deref(),
            Some("Children's and family-oriented shows.")
        );
        assert_eq!(
            config.classification.root_folders["/data/scripted"]
                .label
                .as_deref(),
            Some("Scripted")
        );

        clear_rooterr_env();
    }

    #[test]
    fn invalid_env_root_folder_json_names_variable() {
        let _guard = lock_env();
        clear_rooterr_env();
        set_env("ROOTERR_CLASSIFICATION_ROOT_FOLDERS_JSON", "not-json");

        let mut config = Config::default();
        let err = config.apply_env().expect_err("invalid root folder json");

        assert!(
            err.to_string()
                .contains("ROOTERR_CLASSIFICATION_ROOT_FOLDERS_JSON")
        );

        clear_rooterr_env();
    }

    #[test]
    fn env_root_folder_json_replaces_toml_hints() {
        let _guard = lock_env();
        clear_rooterr_env();
        set_env(
            "ROOTERR_CLASSIFICATION_ROOT_FOLDERS_JSON",
            r#"{
                "/data/env": {
                    "label": "Env",
                    "description": "Configured from environment."
                }
            }"#,
        );

        let mut config = toml::from_str::<Config>(
            r#"
            [sonarr]
            api_key = "test-key"

            [classification.root_folders."/data/toml"]
            label = "TOML"
            description = "Configured from TOML."
            "#,
        )
        .expect("parse config");

        config.apply_env().expect("apply env");

        assert!(
            !config
                .classification
                .root_folders
                .contains_key("/data/toml")
        );
        assert_eq!(
            config.classification.root_folders["/data/env"]
                .description
                .as_deref(),
            Some("Configured from environment.")
        );

        clear_rooterr_env();
    }

    fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().expect("env lock")
    }

    fn set_env(key: &str, value: &str) {
        unsafe { env::set_var(key, value) };
    }

    fn remove_env(key: &str) {
        unsafe { env::remove_var(key) };
    }

    fn clear_rooterr_env() {
        for key in [
            "ROOTERR_CONFIG",
            "ROOTERR_SERVER_BIND_ADDRESS",
            "ROOTERR_SONARR_BASE_URL",
            "ROOTERR_SONARR_API_KEY",
            "ROOTERR_SONARR_WEBHOOK_TOKEN",
            "ROOTERR_LLM_PROVIDER",
            "ROOTERR_LLM_BASE_URL",
            "ROOTERR_LLM_MODEL",
            "ROOTERR_LLM_API_KEY",
            "ROOTERR_LLM_AUTO_PULL",
            "ROOTERR_LLM_STARTUP_WAIT_SECONDS",
            "ROOTERR_LLM_PULL_TIMEOUT_SECONDS",
            "ROOTERR_LLM_AUTO_NUM_CTX",
            "ROOTERR_LLM_MIN_NUM_CTX",
            "ROOTERR_LLM_MAX_NUM_CTX",
            "ROOTERR_LLM_RESERVED_OUTPUT_TOKENS",
            "ROOTERR_LLM_TIMEOUT_SECONDS",
            "ROOTERR_LLM_TEMPERATURE",
            "ROOTERR_TMDB_BEARER_TOKEN",
            "ROOTERR_TVDB_API_KEY",
            "ROOTERR_TVDB_PIN",
            "ROOTERR_DATABASE_SQLITE_PATH",
            "ROOTERR_CLASSIFICATION_MIN_CONFIDENCE",
            "ROOTERR_CLASSIFICATION_ROOT_FOLDERS_JSON",
        ] {
            remove_env(key);
        }
    }
}
