use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::time::sleep;
use tracing::{info, warn};

use crate::{
    config::{LlmConfig, LlmProvider},
    domain::{
        classification::{Classification, ClassificationAttempt, parse_classification_response},
        metadata::{ClassificationMetadata, MetadataBundle},
        root_folder::RootFolderChoice,
        status::LlmStatusProbeResult,
    },
    ports::{
        classifier::Classifier, llm_model_provisioner::LlmModelProvisioner,
        llm_status_probe::LlmStatusProbe,
    },
};

const DEFAULT_CONTEXT_LIMIT: u32 = 32_768;
const CONTEXT_BUCKETS: &[u32] = &[4096, 8192, 16_384, 32_768, 65_536, 131_072, 262_144];

#[derive(Clone)]
pub struct LocalLlmClassifier {
    http: Client,
    config: LlmConfig,
}

impl LocalLlmClassifier {
    pub fn new(http: Client, config: &LlmConfig) -> Self {
        Self {
            http,
            config: config.clone(),
        }
    }

    async fn call_ollama(&self, messages: &[ChatMessage]) -> Result<String> {
        let url = format!("{}/api/chat", self.config.base_url.trim_end_matches('/'));
        let mut options = Map::new();
        options.insert("temperature".to_string(), json!(self.config.temperature));
        if let Some(num_ctx) = self.ollama_num_ctx(messages).await? {
            options.insert("num_ctx".to_string(), json!(num_ctx));
        }

        let body = json!({
            "model": self.config.model,
            "stream": false,
            "format": "json",
            "think": self.config.think,
            "messages": messages,
            "options": Value::Object(options)
        });

        let response = self
            .http
            .post(url)
            .json(&body)
            .timeout(self.config.timeout())
            .send()
            .await
            .context("failed to send Ollama chat request")?;

        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read Ollama chat response")?;
        if !status.is_success() {
            bail!("Ollama returned HTTP {status}: {}", text.trim());
        }

        let value: Value =
            serde_json::from_str(&text).context("failed to parse Ollama response")?;
        value
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow!("Ollama response did not include message.content"))
    }

    async fn call_openai_compatible(&self, messages: &[ChatMessage]) -> Result<String> {
        let url = format!(
            "{}/v1/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let body = json!({
            "model": self.config.model,
            "temperature": self.config.temperature,
            "response_format": { "type": "json_object" },
            "messages": messages,
        });

        let mut request = self
            .http
            .post(url)
            .json(&body)
            .timeout(self.config.timeout());
        if let Some(api_key) = self.config.api_key.as_deref() {
            request = request.bearer_auth(api_key);
        }

        let response = request
            .send()
            .await
            .context("failed to send OpenAI-compatible chat request")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read OpenAI-compatible chat response")?;
        if !status.is_success() {
            bail!(
                "OpenAI-compatible endpoint returned HTTP {status}: {}",
                text.trim()
            );
        }

        let value: Value =
            serde_json::from_str(&text).context("failed to parse OpenAI-compatible response")?;
        value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                anyhow!("OpenAI-compatible response did not include choices[0].message.content")
            })
    }

    async fn wait_for_ollama_model_names(&self) -> Result<Vec<String>> {
        let wait_timeout = self.config.startup_wait_timeout();
        let deadline = Instant::now() + wait_timeout;

        loop {
            match self.fetch_ollama_model_names().await {
                Ok(model_names) => return Ok(model_names),
                Err(error) => {
                    if wait_timeout.is_zero() || Instant::now() >= deadline {
                        bail!(
                            "Ollama at {} did not become reachable within {}s: {error}",
                            self.config.base_url,
                            self.config.startup_wait_seconds
                        );
                    }
                }
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            sleep(remaining.min(Duration::from_secs(2))).await;
        }
    }

    async fn fetch_ollama_model_names(&self) -> Result<Vec<String>> {
        let url = format!("{}/api/tags", self.config.base_url.trim_end_matches('/'));
        let response = self
            .http
            .get(url)
            .timeout(self.config.timeout().min(Duration::from_secs(5)))
            .send()
            .await
            .context("failed to send Ollama tags request")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read Ollama tags response")?;
        if !status.is_success() {
            bail!("Ollama tags returned HTTP {status}: {}", text.trim());
        }

        let response: OllamaTagsResponse =
            serde_json::from_str(&text).context("failed to parse Ollama tags response")?;
        Ok(response
            .models
            .into_iter()
            .filter_map(|model| model.name.or(model.model))
            .collect())
    }

    async fn fetch_openai_compatible_model_names(&self) -> Result<Vec<String>> {
        let url = format!("{}/v1/models", self.config.base_url.trim_end_matches('/'));
        let mut request = self
            .http
            .get(url)
            .timeout(self.config.timeout().min(Duration::from_secs(5)));
        if let Some(api_key) = self.config.api_key.as_deref() {
            request = request.bearer_auth(api_key);
        }

        let response = request
            .send()
            .await
            .context("failed to send OpenAI-compatible models request")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read OpenAI-compatible models response")?;
        if !status.is_success() {
            bail!(
                "OpenAI-compatible models returned HTTP {status}: {}",
                text.trim()
            );
        }

        let value: Value = serde_json::from_str(&text)
            .context("failed to parse OpenAI-compatible models response")?;
        let models = value
            .get("data")
            .and_then(Value::as_array)
            .map(|data| {
                data.iter()
                    .filter_map(|item| item.get("id").and_then(Value::as_str))
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(models)
    }

    async fn ollama_num_ctx(&self, messages: &[ChatMessage]) -> Result<Option<u32>> {
        if !self.config.auto_num_ctx {
            return Ok(None);
        }

        let estimated_prompt_tokens = estimate_prompt_tokens(messages);
        let required_tokens =
            estimated_prompt_tokens.saturating_add(self.config.reserved_output_tokens);
        let context_limit = self.ollama_context_limit().await;
        let context_limit = match context_limit {
            Ok(context_limit) => context_limit,
            Err(error) => {
                warn!(
                    error = %format_anyhow_error(&error),
                    fallback_context_limit = DEFAULT_CONTEXT_LIMIT,
                    "failed to detect Ollama model context length; using fallback"
                );
                DEFAULT_CONTEXT_LIMIT
            }
        };

        let num_ctx = choose_num_ctx(
            required_tokens,
            self.config.min_num_ctx,
            context_limit,
            self.config.reserved_output_tokens,
        )?;
        Ok(Some(num_ctx))
    }

    async fn ollama_context_limit(&self) -> Result<u32> {
        if self.config.max_num_ctx > 0 {
            return Ok(self.config.max_num_ctx);
        }

        self.fetch_ollama_model_context_limit().await
    }

    async fn fetch_ollama_model_context_limit(&self) -> Result<u32> {
        let url = format!("{}/api/show", self.config.base_url.trim_end_matches('/'));
        let response = self
            .http
            .post(url)
            .json(&json!({ "model": self.config.model }))
            .timeout(self.config.timeout().min(Duration::from_secs(5)))
            .send()
            .await
            .context("failed to send Ollama show request")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read Ollama show response")?;
        if !status.is_success() {
            bail!("Ollama show returned HTTP {status}: {}", text.trim());
        }

        let value: Value =
            serde_json::from_str(&text).context("failed to parse Ollama show response")?;
        find_context_length(&value)
            .ok_or_else(|| anyhow!("Ollama show response did not include model context length"))
    }

    async fn pull_ollama_model(&self) -> Result<()> {
        let url = format!("{}/api/pull", self.config.base_url.trim_end_matches('/'));
        let body = OllamaPullRequest {
            model: &self.config.model,
            stream: false,
        };
        let response = self
            .http
            .post(url)
            .json(&body)
            .timeout(self.config.pull_timeout())
            .send()
            .await
            .with_context(|| {
                format!(
                    "failed to send Ollama pull request for {}",
                    self.config.model
                )
            })?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("failed to read Ollama pull response")?;
        if !status.is_success() {
            bail!(
                "Ollama pull for {} returned HTTP {status}: {}",
                self.config.model,
                text.trim()
            );
        }

        if let Ok(value) = serde_json::from_str::<Value>(&text)
            && let Some(error) = value.get("error").and_then(Value::as_str)
        {
            bail!("Ollama pull for {} failed: {error}", self.config.model);
        }

        Ok(())
    }
}

#[async_trait]
impl LlmModelProvisioner for LocalLlmClassifier {
    async fn ensure_model_ready(&self) -> Result<()> {
        if !matches!(self.config.provider, LlmProvider::Ollama) {
            return Ok(());
        }

        info!(model = %self.config.model, "checking Ollama model availability");
        let model_names = self.wait_for_ollama_model_names().await?;
        if model_names.iter().any(|name| name == &self.config.model) {
            info!(model = %self.config.model, "Ollama model is already available");
            return Ok(());
        }

        info!(model = %self.config.model, "pulling missing Ollama model");
        self.pull_ollama_model().await?;
        info!(model = %self.config.model, "Ollama model is ready");
        Ok(())
    }
}

#[async_trait]
impl LlmStatusProbe for LocalLlmClassifier {
    fn base_url(&self) -> &str {
        &self.config.base_url
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    async fn probe_status(&self) -> Result<LlmStatusProbeResult> {
        match self.config.provider {
            LlmProvider::Ollama => {
                let model_names = self.fetch_ollama_model_names().await?;
                let model_available = model_names.iter().any(|name| name == &self.config.model);
                Ok(LlmStatusProbeResult {
                    model_available: Some(model_available),
                    detail: Some(if model_available {
                        format!("model '{}' is available in Ollama", self.config.model)
                    } else {
                        format!("model '{}' is not available in Ollama", self.config.model)
                    }),
                })
            }
            LlmProvider::OpenAiCompatible => {
                let model_names = self.fetch_openai_compatible_model_names().await?;
                let model_available = model_names.iter().any(|name| name == &self.config.model);
                let detail = if model_names.is_empty() {
                    "OpenAI-compatible endpoint responded to /v1/models".to_string()
                } else if model_available {
                    format!(
                        "configured model '{}' is listed by the endpoint",
                        self.config.model
                    )
                } else {
                    format!(
                        "configured model '{}' is not listed by the endpoint",
                        self.config.model
                    )
                };
                Ok(LlmStatusProbeResult {
                    model_available: Some(model_available),
                    detail: Some(detail),
                })
            }
        }
    }
}

#[async_trait]
impl Classifier for LocalLlmClassifier {
    fn provider_name(&self) -> &'static str {
        match self.config.provider {
            LlmProvider::Ollama => "ollama",
            LlmProvider::OpenAiCompatible => "openai_compatible",
        }
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    async fn classify(
        &self,
        metadata: &MetadataBundle,
        root_folders: &[RootFolderChoice],
    ) -> Result<ClassificationAttempt> {
        let classification_metadata = metadata.classification_metadata();
        let eligible_root_folders = eligible_root_folders(&classification_metadata, root_folders);
        if eligible_root_folders.is_empty() {
            bail!("no eligible root folders are available for LLM classification");
        }

        let messages = build_messages(&classification_metadata, &eligible_root_folders)?;
        let prompt_hash = hash_prompt(&messages);
        let started = Instant::now();
        let raw_response = match self.config.provider {
            LlmProvider::Ollama => self.call_ollama(&messages).await,
            LlmProvider::OpenAiCompatible => self.call_openai_compatible(&messages).await,
        };
        let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(i64::MAX);
        let raw_response = match raw_response {
            Ok(raw_response) => raw_response,
            Err(error) => {
                return Ok(ClassificationAttempt {
                    classification: None,
                    raw_response: None,
                    parsed_response: None,
                    prompt_hash,
                    duration_ms,
                    error: Some(format_anyhow_error(&error)),
                });
            }
        };
        let allowed_paths = eligible_root_folders
            .iter()
            .map(|folder| folder.path.as_str())
            .collect::<Vec<_>>();
        let parsed = parse_classification_response(&raw_response, &allowed_paths).and_then(
            |classification| {
                validate_grounded_classification(
                    &classification,
                    &classification_metadata,
                    &eligible_root_folders,
                )?;
                let parsed_response = serde_json::to_string(&classification)
                    .context("failed to serialize parsed classification")?;
                Ok((classification, parsed_response))
            },
        );

        let (classification, parsed_response, error) = match parsed {
            Ok((classification, parsed_response)) => {
                (Some(classification), Some(parsed_response), None)
            }
            Err(error) => (None, None, Some(format_anyhow_error(&error))),
        };

        Ok(ClassificationAttempt {
            classification,
            raw_response: Some(raw_response),
            parsed_response,
            prompt_hash,
            duration_ms,
            error,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OllamaTagsResponse {
    #[serde(default)]
    models: Vec<OllamaModel>,
}

#[derive(Debug, Deserialize)]
struct OllamaModel {
    name: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Serialize)]
struct OllamaPullRequest<'a> {
    model: &'a str,
    stream: bool,
}

#[cfg(test)]
fn build_classification_messages(
    metadata: &MetadataBundle,
    root_folders: &[RootFolderChoice],
) -> Result<Vec<ChatMessage>> {
    let compact_metadata = metadata.classification_metadata();
    build_messages(&compact_metadata, root_folders)
}

fn build_messages(
    metadata: &ClassificationMetadata,
    root_folders: &[RootFolderChoice],
) -> Result<Vec<ChatMessage>> {
    let root_folder_json =
        serde_json::to_string(root_folders).context("failed to encode root folders")?;
    let metadata_json = serde_json::to_string(metadata).context("failed to encode metadata")?;

    Ok(vec![
        ChatMessage {
            role: "system",
            content: concat!(
                "You classify TV series into exactly one Sonarr root folder. ",
                "Return only a JSON object. Do not include markdown. ",
                "The root_folder_path must exactly match one of the provided paths. ",
                "Use confidence from 0.0 to 1.0. ",
                "The metadata is a curated classification summary, not the complete provider payload. ",
                "Root folder labels and descriptions define the destination policy. ",
                "Prefer a specific matching folder over a broad scripted/default folder when explicit metadata supports the specific folder. ",
                "Provider series_type values such as standard or scripted are weak format signals; do not choose scripted only because of them. ",
                "When a kids folder exists, explicit children or kids genres, keywords, tags, ratings, or overview signals should choose kids over scripted. ",
                "Only choose reality when provider metadata explicitly labels the series as reality or unscripted; narrative words like reality in an overview or tagline do not count. ",
                "Explicit reality or unscripted metadata should choose reality over sports, talk shows, scripted, or miniseries when a reality folder exists. ",
                "Never invent kids evidence: reality genres or a reality series type should choose reality when there is no explicit kids evidence. ",
                "When a talk-shows folder exists, explicit talk or talk show genres or series types should choose talk shows over scripted or self-cast-only docuseries evidence. ",
                "Only choose sports when provider metadata explicitly labels the series as sport or sports; sports-adjacent subjects, teams, competitions, or network wording alone do not make a series sports. ",
                "Only choose documentary for explicit documentary or docuseries metadata; self-heavy participant cast roles can support docuseries, but history, war, true-story, based-on-book, interviews, or archival source wording alone do not make a scripted series a documentary. ",
                "Treat miniseries as a structural hint, not a content-type override: strong documentary evidence beats reality or miniseries, reality beats scripted or miniseries, and clear scripted evidence can still belong in scripted when miniseries metadata is weak or ambiguous. ",
                "When limited-series metadata is strongly corroborated by short-run structure, limited-series season naming, or cross-provider support, prefer miniseries over generic scripted. ",
                "Animation alone is not a kids signal. ",
                "Prefer obvious categories like anime, documentary, kids, miniseries, reality, scripted, sports, or talkshows when the metadata supports them."
            )
            .to_string(),
        },
        ChatMessage {
            role: "user",
            content: format!(
                "Available root folders:\n{root_folder_json}\n\nSeries metadata:\n{metadata_json}\n\nReturn JSON with keys: root_folder_path, confidence, reason, signals. Reason must be a short non-empty explanation grounded in the metadata. Signals must list the strongest metadata clues."
            ),
        },
    ])
}

fn eligible_root_folders(
    metadata: &ClassificationMetadata,
    root_folders: &[RootFolderChoice],
) -> Vec<RootFolderChoice> {
    let explicit_talk_show = metadata.has_explicit_talk_show_evidence();
    let has_talk_show_root = root_folders.iter().any(is_talk_show_root_folder);
    let explicit_documentary = metadata.has_explicit_documentary_evidence();
    let strong_documentary = metadata.has_strong_explicit_documentary_evidence();
    let has_documentary_root = root_folders.iter().any(is_documentary_root_folder);
    let explicit_reality = metadata.has_explicit_reality_evidence();
    let has_reality_root = root_folders.iter().any(is_reality_root_folder);
    let explicit_sports = metadata.has_explicit_sports_evidence();
    let strong_miniseries = metadata.has_strong_explicit_miniseries_evidence();
    let has_miniseries_root = root_folders.iter().any(is_miniseries_root_folder);

    root_folders
        .iter()
        .filter(|folder| {
            (!is_kids_root_folder(folder) || metadata.has_explicit_kids_evidence())
                && (!is_documentary_root_folder(folder) || explicit_documentary)
                && (!is_reality_root_folder(folder) || explicit_reality)
                && (!is_sports_root_folder(folder) || explicit_sports)
                && (!explicit_talk_show
                    || !has_talk_show_root
                    || !is_scripted_or_miniseries_root_folder(folder)
                    || is_talk_show_root_folder(folder))
                && (!explicit_documentary
                    || !has_documentary_root
                    || !is_scripted_or_miniseries_root_folder(folder)
                    || is_documentary_root_folder(folder))
                && (!strong_documentary
                    || !has_documentary_root
                    || !is_reality_root_folder(folder)
                    || is_documentary_root_folder(folder))
                && (!explicit_reality
                    || !has_reality_root
                    || strong_documentary
                    || !is_documentary_root_folder(folder)
                    || is_reality_root_folder(folder))
                && (!explicit_reality
                    || !has_reality_root
                    || (!is_talk_show_root_folder(folder)
                        && !is_scripted_or_miniseries_root_folder(folder))
                    || is_reality_root_folder(folder))
                && (!strong_miniseries
                    || !has_miniseries_root
                    || !is_scripted_or_miniseries_root_folder(folder)
                    || is_miniseries_root_folder(folder))
        })
        .cloned()
        .collect()
}

fn validate_grounded_classification(
    classification: &Classification,
    metadata: &ClassificationMetadata,
    root_folders: &[RootFolderChoice],
) -> Result<()> {
    let Some(folder) = root_folders
        .iter()
        .find(|folder| folder.path == classification.root_folder_path)
    else {
        return Ok(());
    };

    if is_kids_root_folder(folder) && !metadata.has_explicit_kids_evidence() {
        bail!(
            "LLM selected kids root folder '{}' without explicit kids metadata evidence",
            folder.path
        );
    }

    if is_sports_root_folder(folder) && !metadata.has_explicit_sports_evidence() {
        bail!(
            "LLM selected sports root folder '{}' without explicit sports metadata evidence",
            folder.path
        );
    }

    let has_documentary_root = root_folders.iter().any(is_documentary_root_folder);
    if is_documentary_root_folder(folder)
        && !is_reality_root_folder(folder)
        && !is_talk_show_root_folder(folder)
        && !metadata.has_explicit_documentary_evidence()
    {
        bail!(
            "LLM selected documentary root folder '{}' without explicit documentary metadata evidence",
            folder.path
        );
    }

    if is_reality_root_folder(folder)
        && !is_documentary_root_folder(folder)
        && metadata.has_strong_explicit_documentary_evidence()
        && has_documentary_root
    {
        bail!(
            "LLM selected reality root '{}' despite explicit documentary metadata evidence",
            folder.path
        );
    }

    if is_scripted_root_folder(folder)
        && metadata.has_explicit_documentary_evidence()
        && has_documentary_root
    {
        bail!(
            "LLM selected scripted root '{}' despite explicit documentary metadata evidence",
            folder.path
        );
    }

    if is_miniseries_root_folder(folder)
        && metadata.has_explicit_documentary_evidence()
        && has_documentary_root
    {
        bail!(
            "LLM selected miniseries root '{}' despite explicit documentary metadata evidence",
            folder.path
        );
    }

    let has_reality_root = root_folders.iter().any(is_reality_root_folder);
    if is_documentary_root_folder(folder)
        && !is_reality_root_folder(folder)
        && metadata.has_explicit_reality_evidence()
        && has_reality_root
        && !metadata.has_strong_explicit_documentary_evidence()
    {
        bail!(
            "LLM selected documentary root '{}' despite stronger explicit reality metadata evidence",
            folder.path
        );
    }

    if is_talk_show_root_folder(folder)
        && !is_reality_root_folder(folder)
        && metadata.has_explicit_reality_evidence()
        && has_reality_root
    {
        bail!(
            "LLM selected talk show root '{}' despite explicit reality metadata evidence",
            folder.path
        );
    }

    if is_scripted_root_folder(folder)
        && metadata.has_explicit_reality_evidence()
        && has_reality_root
    {
        bail!(
            "LLM selected scripted root '{}' despite explicit reality metadata evidence",
            folder.path
        );
    }

    let has_miniseries_root = root_folders.iter().any(is_miniseries_root_folder);
    if is_scripted_root_folder(folder)
        && metadata.has_strong_explicit_miniseries_evidence()
        && has_miniseries_root
    {
        bail!(
            "LLM selected scripted root '{}' despite strong explicit miniseries metadata evidence",
            folder.path
        );
    }

    Ok(())
}

fn is_kids_root_folder(folder: &RootFolderChoice) -> bool {
    folder
        .label
        .as_deref()
        .is_some_and(contains_kids_folder_term)
        || folder
            .description
            .as_deref()
            .is_some_and(contains_kids_folder_term)
        || contains_kids_folder_term(&folder.path)
}

fn is_talk_show_root_folder(folder: &RootFolderChoice) -> bool {
    folder
        .label
        .as_deref()
        .is_some_and(contains_talk_show_folder_term)
        || folder
            .description
            .as_deref()
            .is_some_and(contains_talk_show_folder_term)
        || contains_talk_show_folder_term(&folder.path)
}

fn is_documentary_root_folder(folder: &RootFolderChoice) -> bool {
    folder
        .label
        .as_deref()
        .is_some_and(contains_documentary_folder_term)
        || folder
            .description
            .as_deref()
            .is_some_and(contains_documentary_folder_term)
        || contains_documentary_folder_term(&folder.path)
}

fn is_reality_root_folder(folder: &RootFolderChoice) -> bool {
    folder
        .label
        .as_deref()
        .is_some_and(contains_reality_folder_term)
        || folder
            .description
            .as_deref()
            .is_some_and(contains_reality_folder_term)
        || contains_reality_folder_term(&folder.path)
}

fn is_sports_root_folder(folder: &RootFolderChoice) -> bool {
    folder
        .label
        .as_deref()
        .is_some_and(contains_sports_folder_term)
        || folder
            .description
            .as_deref()
            .is_some_and(contains_sports_folder_term)
        || contains_sports_folder_term(&folder.path)
}

fn is_miniseries_root_folder(folder: &RootFolderChoice) -> bool {
    folder
        .label
        .as_deref()
        .is_some_and(contains_miniseries_folder_term)
        || folder
            .description
            .as_deref()
            .is_some_and(contains_miniseries_folder_term)
        || contains_miniseries_folder_term(&folder.path)
}

fn is_scripted_root_folder(folder: &RootFolderChoice) -> bool {
    folder
        .label
        .as_deref()
        .is_some_and(contains_scripted_folder_term)
        || folder
            .description
            .as_deref()
            .is_some_and(contains_scripted_folder_term)
        || contains_scripted_folder_term(&folder.path)
}

fn is_scripted_or_miniseries_root_folder(folder: &RootFolderChoice) -> bool {
    is_scripted_root_folder(folder) || is_miniseries_root_folder(folder)
}

fn contains_kids_folder_term(value: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|part| {
            matches!(
                part.to_ascii_lowercase().as_str(),
                "kid" | "kids" | "children"
            )
        })
}

fn contains_talk_show_folder_term(value: &str) -> bool {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .contains("talkshow")
}

fn contains_documentary_folder_term(value: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|part| {
            matches!(
                part.to_ascii_lowercase().as_str(),
                "documentary" | "documentaries" | "docuseries"
            )
        })
}

fn contains_reality_folder_term(value: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|part| matches!(part.to_ascii_lowercase().as_str(), "reality" | "unscripted"))
}

fn contains_sports_folder_term(value: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|part| matches!(part.to_ascii_lowercase().as_str(), "sport" | "sports"))
}

fn contains_miniseries_folder_term(value: &str) -> bool {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .contains("miniseries")
}

fn contains_scripted_folder_term(value: &str) -> bool {
    value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|part| part.eq_ignore_ascii_case("scripted"))
}

fn format_anyhow_error(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

fn estimate_prompt_tokens(messages: &[ChatMessage]) -> u32 {
    let serialized = serde_json::to_string(messages).unwrap_or_else(|_| {
        messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("")
    });
    let chars = serialized.chars().count();
    let estimated = chars.div_ceil(3);
    estimated.try_into().unwrap_or(u32::MAX)
}

fn choose_num_ctx(
    required_tokens: u32,
    min_num_ctx: u32,
    context_limit: u32,
    reserved_output_tokens: u32,
) -> Result<u32> {
    let min_num_ctx = min_num_ctx.max(1);
    let required_tokens = required_tokens.max(min_num_ctx);
    if required_tokens > context_limit {
        bail!(
            "classification prompt requires about {required_tokens} tokens including {reserved_output_tokens} reserved output tokens, but the Ollama context limit is {context_limit}; prune metadata further or switch to a larger-context model"
        );
    }

    let bucket = CONTEXT_BUCKETS
        .iter()
        .copied()
        .find(|bucket| *bucket >= required_tokens)
        .unwrap_or(required_tokens);
    Ok(bucket.min(context_limit))
}

fn find_context_length(value: &Value) -> Option<u32> {
    let model_info = value.get("model_info").unwrap_or(value);
    find_context_length_in_value(model_info)
}

fn find_context_length_in_value(value: &Value) -> Option<u32> {
    match value {
        Value::Object(map) => map.iter().find_map(|(key, value)| {
            if key.ends_with("context_length") || key == "context_length" {
                value
                    .as_u64()
                    .and_then(|value| value.try_into().ok())
                    .or_else(|| value.as_i64().and_then(|value| value.try_into().ok()))
            } else {
                find_context_length_in_value(value)
            }
        }),
        Value::Array(values) => values.iter().find_map(find_context_length_in_value),
        _ => None,
    }
}

fn hash_prompt(messages: &[ChatMessage]) -> String {
    let mut hasher = Sha256::new();
    for message in messages {
        hasher.update(message.role.as_bytes());
        hasher.update([0]);
        hasher.update(message.content.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, method, path},
    };

    use crate::ports::llm_model_provisioner::LlmModelProvisioner;

    use super::*;

    fn ollama_config(base_url: String) -> LlmConfig {
        LlmConfig {
            base_url,
            model: "qwen3:0.6b".to_string(),
            auto_pull: true,
            startup_wait_seconds: 0,
            pull_timeout_seconds: 5,
            timeout_seconds: 1,
            ..LlmConfig::default()
        }
    }

    #[test]
    fn context_sizing_uses_stable_buckets() {
        assert_eq!(choose_num_ctx(1200, 4096, 32_768, 512).expect("ctx"), 4096);
        assert_eq!(choose_num_ctx(5000, 4096, 32_768, 512).expect("ctx"), 8192);
        assert_eq!(
            choose_num_ctx(30_000, 4096, 32_768, 512).expect("ctx"),
            32_768
        );
    }

    #[test]
    fn context_sizing_rejects_impossible_prompts() {
        let error = choose_num_ctx(40_000, 4096, 32_768, 512).expect_err("too large");

        assert!(error.to_string().contains("larger-context model"));
    }

    #[test]
    fn context_length_is_parsed_from_ollama_show_response() {
        let value = json!({
            "model_info": {
                "qwen3.context_length": 32768
            }
        });

        assert_eq!(find_context_length(&value), Some(32_768));
    }

    #[tokio::test]
    async fn ensure_model_ready_skips_pull_when_model_exists() {
        let server = MockServer::start().await;
        let classifier = LocalLlmClassifier::new(Client::new(), &ollama_config(server.uri()));

        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": [
                    { "name": "qwen3:0.6b" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        classifier.ensure_model_ready().await.expect("model ready");

        let requests = server.received_requests().await.expect("request recording");
        let pull_count = requests
            .iter()
            .filter(|request| {
                request.method.as_str() == "POST" && request.url.path() == "/api/pull"
            })
            .count();
        assert_eq!(pull_count, 0);
    }

    #[tokio::test]
    async fn ensure_model_ready_pulls_missing_model() {
        let server = MockServer::start().await;
        let classifier = LocalLlmClassifier::new(Client::new(), &ollama_config(server.uri()));

        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/pull"))
            .and(body_json(json!({
                "model": "qwen3:0.6b",
                "stream": false
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "success"
            })))
            .expect(1)
            .mount(&server)
            .await;

        classifier.ensure_model_ready().await.expect("model ready");

        let requests = server.received_requests().await.expect("request recording");
        let pull_request = requests
            .iter()
            .find(|request| request.method.as_str() == "POST" && request.url.path() == "/api/pull")
            .expect("pull request");
        let body: Value = pull_request.body_json().expect("pull request body");
        assert_eq!(
            body,
            json!({
                "model": "qwen3:0.6b",
                "stream": false
            })
        );
    }

    #[tokio::test]
    async fn ensure_model_ready_fails_when_ollama_is_unreachable() {
        let classifier =
            LocalLlmClassifier::new(Client::new(), &ollama_config("http://127.0.0.1:9".into()));

        let error = classifier
            .ensure_model_ready()
            .await
            .expect_err("unreachable Ollama should fail");

        assert!(error.to_string().contains("did not become reachable"));
    }

    #[tokio::test]
    async fn ensure_model_ready_returns_pull_errors() {
        let server = MockServer::start().await;
        let classifier = LocalLlmClassifier::new(Client::new(), &ollama_config(server.uri()));

        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/pull"))
            .respond_with(ResponseTemplate::new(500).set_body_string("registry unavailable"))
            .expect(1)
            .mount(&server)
            .await;

        let error = classifier
            .ensure_model_ready()
            .await
            .expect_err("pull failure should fail");

        assert!(error.to_string().contains("Ollama pull"));
    }

    #[tokio::test]
    async fn ollama_chat_includes_dynamic_num_ctx_from_detected_context_limit() {
        let server = MockServer::start().await;
        let mut config = ollama_config(server.uri());
        config.timeout_seconds = 5;
        config.max_num_ctx = 0;
        let classifier = LocalLlmClassifier::new(Client::new(), &config);

        Mock::given(method("POST"))
            .and(path("/api/show"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model_info": {
                    "qwen3.context_length": 32768
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": { "content": "{}" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        classifier
            .call_ollama(&[ChatMessage {
                role: "user",
                content: "short prompt".to_string(),
            }])
            .await
            .expect("ollama response");

        let requests = server.received_requests().await.expect("request recording");
        let chat_request = requests
            .iter()
            .find(|request| request.method.as_str() == "POST" && request.url.path() == "/api/chat")
            .expect("chat request");
        let body: Value = chat_request.body_json().expect("chat body");
        assert_eq!(body["think"], json!(false));
        assert_eq!(body["options"]["num_ctx"], json!(4096));
    }

    #[tokio::test]
    async fn configured_max_num_ctx_overrides_show_detection() {
        let server = MockServer::start().await;
        let mut config = ollama_config(server.uri());
        config.timeout_seconds = 5;
        config.max_num_ctx = 8192;
        let classifier = LocalLlmClassifier::new(Client::new(), &config);

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": { "content": "{}" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        classifier
            .call_ollama(&[ChatMessage {
                role: "user",
                content: "x".repeat(15_000),
            }])
            .await
            .expect("ollama response");

        let requests = server.received_requests().await.expect("request recording");
        assert!(
            requests
                .iter()
                .all(|request| request.url.path() != "/api/show")
        );
        let chat_request = requests
            .iter()
            .find(|request| request.method.as_str() == "POST" && request.url.path() == "/api/chat")
            .expect("chat request");
        let body: Value = chat_request.body_json().expect("chat body");
        assert_eq!(body["think"], json!(false));
        assert_eq!(body["options"]["num_ctx"], json!(8192));
    }

    #[tokio::test]
    async fn ollama_chat_sends_configured_think_flag() {
        let server = MockServer::start().await;
        let mut config = ollama_config(server.uri());
        config.think = true;
        config.timeout_seconds = 5;
        let classifier = LocalLlmClassifier::new(Client::new(), &config);

        Mock::given(method("POST"))
            .and(path("/api/show"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model_info": {
                    "qwen3.context_length": 32768
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": { "content": "{}" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        classifier
            .call_ollama(&[ChatMessage {
                role: "user",
                content: "short prompt".to_string(),
            }])
            .await
            .expect("ollama response");

        let requests = server.received_requests().await.expect("request recording");
        let chat_request = requests
            .iter()
            .find(|request| request.method.as_str() == "POST" && request.url.path() == "/api/chat")
            .expect("chat request");
        let body: Value = chat_request.body_json().expect("chat body");
        assert_eq!(body["think"], json!(true));
    }

    #[tokio::test]
    async fn probe_status_reports_missing_ollama_model_as_warning_data() {
        let server = MockServer::start().await;
        let classifier = LocalLlmClassifier::new(Client::new(), &ollama_config(server.uri()));

        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": [
                    { "name": "different-model" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let result = classifier.probe_status().await.expect("probe result");
        assert_eq!(result.model_available, Some(false));
    }

    #[tokio::test]
    async fn probe_status_checks_openai_compatible_models() {
        let server = MockServer::start().await;
        let config = LlmConfig {
            provider: LlmProvider::OpenAiCompatible,
            base_url: server.uri(),
            model: "gpt-test".to_string(),
            timeout_seconds: 5,
            ..LlmConfig::default()
        };
        let classifier = LocalLlmClassifier::new(Client::new(), &config);

        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [
                    { "id": "gpt-test" }
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let result = classifier.probe_status().await.expect("probe result");
        assert_eq!(result.model_available, Some(true));
    }

    #[tokio::test]
    async fn openai_compatible_chat_does_not_send_num_ctx() {
        let server = MockServer::start().await;
        let mut config = ollama_config(server.uri());
        config.provider = LlmProvider::OpenAiCompatible;
        config.timeout_seconds = 5;
        let classifier = LocalLlmClassifier::new(Client::new(), &config);

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": { "content": "{}" }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        classifier
            .call_openai_compatible(&[ChatMessage {
                role: "user",
                content: "short prompt".to_string(),
            }])
            .await
            .expect("openai-compatible response");

        let requests = server.received_requests().await.expect("request recording");
        let request = requests
            .iter()
            .find(|request| request.url.path() == "/v1/chat/completions")
            .expect("chat request");
        let body: Value = request.body_json().expect("chat body");
        assert!(body.get("options").is_none());
        assert!(body.get("num_ctx").is_none());
        assert!(body.get("think").is_none());
    }

    #[test]
    fn classification_messages_use_compact_metadata() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "Baywatch",
                "year": 1989,
                "overview": "Lifeguards protect crowded Los Angeles beaches.",
                "genres": ["Action", "Drama"],
                "seriesType": "standard",
                "images": [{ "remoteUrl": "https://example.test/poster.jpg" }]
            }),
            tmdb: Some(json!({
                "name": "Baywatch",
                "overview": "Join the Baywatch lifeguards on their adventures.",
                "type": "Scripted",
                "genres": [{ "name": "Drama" }],
                "keywords": { "results": [{ "name": "lifeguard" }] },
                "aggregate_credits": {
                    "cast": [{
                        "name": "Actor",
                        "profile_path": "/actor.jpg",
                        "roles": [{ "character": "Character" }]
                    }]
                },
                "seasons": [{
                    "name": "Season 1",
                    "poster_path": "/season.jpg"
                }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };
        let root_folders = vec![RootFolderChoice {
            path: "/tv/scripted".to_string(),
            label: Some("Scripted".to_string()),
            description: Some("Scripted drama and comedy.".to_string()),
        }];

        let messages = build_classification_messages(&metadata, &root_folders).expect("messages");
        let prompt = messages
            .iter()
            .find(|message| message.role == "user")
            .expect("user prompt")
            .content
            .as_str();
        let full_prompt = messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(full_prompt.contains("curated"));
        assert!(prompt.contains("Baywatch"));
        assert!(prompt.contains("lifeguard"));
        assert!(prompt.len() < 12_000);
        assert!(!prompt.contains("aggregate_credits"));
        assert!(!prompt.contains("profile_path"));
        assert!(!prompt.contains("poster_path"));
        assert!(!prompt.contains("seasons"));
        assert!(!prompt.contains("remoteUrl"));
    }

    #[test]
    fn classification_messages_treat_scripted_as_fallback_for_explicit_kids_signals() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "Bluey",
                "genres": ["Animation", "Children", "Comedy", "Family"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "Bluey",
                "type": "Scripted",
                "keywords": { "results": [{ "name": "kids" }] }
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/kids".to_string(),
                label: Some("Kids".to_string()),
                description: Some("Children's and family-oriented shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/scripted".to_string(),
                label: Some("Scripted".to_string()),
                description: Some("Default scripted shows.".to_string()),
            },
        ];

        let messages = build_classification_messages(&metadata, &root_folders).expect("messages");
        let system_prompt = messages
            .iter()
            .find(|message| message.role == "system")
            .expect("system prompt")
            .content
            .as_str();

        assert!(system_prompt.contains("Prefer a specific matching folder"));
        assert!(system_prompt.contains("series_type values such as standard or scripted"));
        assert!(system_prompt.contains("should choose kids over scripted"));
        assert!(system_prompt.contains("Never invent kids evidence"));
        assert!(system_prompt.contains("choose reality over sports"));
        assert!(system_prompt.contains("Treat miniseries as a structural hint"));
        assert!(system_prompt.contains("strongly corroborated"));
    }

    #[test]
    fn kids_classification_requires_explicit_metadata_evidence() {
        let reality_metadata = MetadataBundle {
            sonarr: json!({
                "title": "90 Day Fiancé",
                "genres": ["Reality", "Romance"],
                "seriesType": "standard",
                "overview": "International couples face family drama."
            }),
            tmdb: Some(json!({
                "name": "90 Day Fiancé",
                "type": "Reality",
                "genres": [{ "name": "Reality" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/kids".to_string(),
                label: Some("Kids".to_string()),
                description: Some("Children's shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/reality".to_string(),
                label: Some("Reality".to_string()),
                description: Some("Reality and unscripted shows.".to_string()),
            },
        ];
        let classification = Classification {
            root_folder_path: "/tv/kids".to_string(),
            confidence: 0.9,
            reason: "Explicit children genre signal.".to_string(),
            signals: vec!["Reality".to_string()],
        };

        let error =
            validate_grounded_classification(&classification, &reality_metadata, &root_folders)
                .expect_err("ungrounded kids classification");

        assert!(error.to_string().contains("without explicit kids metadata"));
    }

    #[test]
    fn kids_root_folder_is_not_offered_without_explicit_metadata_evidence() {
        let reality_metadata = MetadataBundle {
            sonarr: json!({
                "title": "90 Day Fiancé",
                "genres": ["Reality", "Romance"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "90 Day Fiancé",
                "type": "Reality",
                "genres": [{ "name": "Reality" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/kids".to_string(),
                label: Some("Kids".to_string()),
                description: Some("Children's shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/reality".to_string(),
                label: Some("Reality".to_string()),
                description: Some("Reality and unscripted shows.".to_string()),
            },
        ];

        let eligible = eligible_root_folders(&reality_metadata, &root_folders);
        let messages = build_messages(&reality_metadata, &eligible).expect("messages");
        let prompt = messages
            .iter()
            .find(|message| message.role == "user")
            .expect("user prompt")
            .content
            .as_str();

        assert_eq!(eligible, vec![root_folders[1].clone()]);
        assert!(!prompt.contains("/tv/kids"));
        assert!(prompt.contains("/tv/reality"));
    }

    #[test]
    fn reality_root_is_not_offered_without_explicit_reality_metadata() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "Shining Girls",
                "genres": ["Crime", "Drama", "Mini-Series", "Science Fiction", "Thriller"],
                "seriesType": "standard",
                "overview": "Years after a brutal attack left her in a constantly shifting reality, Kirby learns that a recent murder is linked to her assault."
            }),
            tmdb: Some(json!({
                "name": "Shining Girls",
                "type": "Miniseries",
                "genres": [{ "name": "Crime" }, { "name": "Drama" }, { "name": "Mystery" }],
                "tagline": "Reality is a matter of perspective.",
                "aggregate_credits": {
                    "cast": [
                        { "roles": [{ "character": "Kirby Mazrachi" }] },
                        { "roles": [{ "character": "Dan Velazquez" }] }
                    ]
                }
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();

        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert!(!eligible.iter().any(|folder| folder.path == "/tv/reality"));
        assert!(eligible.iter().any(|folder| folder.path == "/tv/scripted"));
        assert!(
            eligible
                .iter()
                .any(|folder| folder.path == "/tv/miniseries")
        );
    }

    #[test]
    fn sports_root_is_not_offered_without_explicit_sports_metadata() {
        let reality_metadata = MetadataBundle {
            sonarr: json!({
                "title": "Teen Mom: The Next Chapter",
                "genres": ["Reality"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "Teen Mom: The Next Chapter",
                "type": "Reality",
                "genres": [{ "name": "Reality" }, { "name": "Documentary" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/reality".to_string(),
                label: Some("Reality".to_string()),
                description: Some("Reality and unscripted shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/sports".to_string(),
                label: Some("Sports".to_string()),
                description: Some("Sports programming and competition broadcasts.".to_string()),
            },
        ];

        let eligible = eligible_root_folders(&reality_metadata, &root_folders);

        assert_eq!(eligible, vec![root_folders[0].clone()]);
    }

    #[test]
    fn sports_classification_requires_explicit_sports_metadata() {
        let reality_metadata = MetadataBundle {
            sonarr: json!({
                "title": "Teen Mom: The Next Chapter",
                "genres": ["Reality"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "Teen Mom: The Next Chapter",
                "type": "Reality",
                "genres": [{ "name": "Reality" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![RootFolderChoice {
            path: "/tv/sports".to_string(),
            label: Some("Sports".to_string()),
            description: Some("Sports programming and competition broadcasts.".to_string()),
        }];
        let classification = Classification {
            root_folder_path: "/tv/sports".to_string(),
            confidence: 0.95,
            reason: "Competition wording.".to_string(),
            signals: vec!["competition".to_string()],
        };

        let error =
            validate_grounded_classification(&classification, &reality_metadata, &root_folders)
                .expect_err("ungrounded sports classification");

        assert!(error.to_string().contains("without explicit sports metadata"));
    }

    #[test]
    fn explicit_sports_metadata_keeps_sports_root() {
        let sports_metadata = MetadataBundle {
            sonarr: json!({
                "title": "Match of the Day",
                "genres": ["Sports"],
                "seriesType": "standard"
            }),
            tmdb: None,
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![RootFolderChoice {
            path: "/tv/sports".to_string(),
            label: Some("Sports".to_string()),
            description: Some("Sports programming and competition broadcasts.".to_string()),
        }];

        assert_eq!(
            eligible_root_folders(&sports_metadata, &root_folders),
            root_folders
        );
    }

    #[test]
    fn explicit_reality_metadata_only_offers_reality_over_talk_show_scripted_and_miniseries() {
        let reality_metadata = MetadataBundle {
            sonarr: json!({
                "title": "Teen Mom: The Next Chapter",
                "genres": ["Reality"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "Teen Mom: The Next Chapter",
                "type": "Talk Show",
                "genres": [{ "name": "Reality" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/reality".to_string(),
                label: Some("Reality".to_string()),
                description: Some("Reality and unscripted shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/talkshows".to_string(),
                label: Some("Talk Shows".to_string()),
                description: Some("Talk shows, interviews, and late-night shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/miniseries".to_string(),
                label: Some("Miniseries".to_string()),
                description: Some("Limited series and miniseries.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/scripted".to_string(),
                label: Some("Scripted".to_string()),
                description: Some("General scripted television.".to_string()),
            },
        ];

        let eligible = eligible_root_folders(&reality_metadata, &root_folders);

        assert_eq!(eligible, vec![root_folders[0].clone()]);
    }

    #[test]
    fn grounded_kids_classification_is_allowed() {
        let kids_metadata = MetadataBundle {
            sonarr: json!({
                "title": "Bluey",
                "genres": ["Animation", "Children", "Family"]
            }),
            tmdb: None,
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![RootFolderChoice {
            path: "/tv/kids".to_string(),
            label: Some("Kids".to_string()),
            description: None,
        }];
        let classification = Classification {
            root_folder_path: "/tv/kids".to_string(),
            confidence: 0.9,
            reason: "Children genre.".to_string(),
            signals: vec!["Children".to_string()],
        };

        validate_grounded_classification(&classification, &kids_metadata, &root_folders)
            .expect("grounded kids classification");
        assert_eq!(
            eligible_root_folders(&kids_metadata, &root_folders),
            root_folders
        );
    }

    #[test]
    fn explicit_talk_show_metadata_only_offers_talk_show_roots() {
        let talk_show_metadata = MetadataBundle {
            sonarr: json!({
                "title": "Late Show with David Letterman",
                "genres": ["Comedy", "Talk Show"],
                "seriesType": "daily"
            }),
            tmdb: Some(json!({
                "name": "Late Show with David Letterman",
                "type": "Talk Show",
                "genres": [{ "name": "Talk" }, { "name": "Comedy" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/talkshows".to_string(),
                label: Some("Talk Shows".to_string()),
                description: Some("Talk shows, interviews, and late-night shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/scripted".to_string(),
                label: Some("Scripted".to_string()),
                description: Some("General scripted television.".to_string()),
            },
        ];

        let eligible = eligible_root_folders(&talk_show_metadata, &root_folders);
        let messages = build_messages(&talk_show_metadata, &eligible).expect("messages");
        let prompt = messages
            .iter()
            .find(|message| message.role == "user")
            .expect("user prompt")
            .content
            .as_str();

        assert_eq!(eligible, vec![root_folders[0].clone()]);
        assert!(prompt.contains("/tv/talkshows"));
        assert!(!prompt.contains("/tv/scripted"));
    }

    #[test]
    fn explicit_talk_show_metadata_keeps_specific_non_scripted_roots_available() {
        let talk_show_metadata = MetadataBundle {
            sonarr: json!({
                "title": "The Interview Show",
                "genres": ["Documentary", "Talk Show"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "The Interview Show",
                "type": "Talk Show",
                "genres": [{ "name": "Documentary" }, { "name": "Talk" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/documentary".to_string(),
                label: Some("Documentary".to_string()),
                description: Some("Documentaries and docuseries.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/talkshows".to_string(),
                label: Some("Talk Shows".to_string()),
                description: Some("Talk shows, interviews, and late-night shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/scripted".to_string(),
                label: Some("Scripted".to_string()),
                description: Some("General scripted television.".to_string()),
            },
        ];

        let eligible = eligible_root_folders(&talk_show_metadata, &root_folders);

        assert_eq!(
            eligible,
            vec![root_folders[0].clone(), root_folders[1].clone()]
        );
    }

    #[test]
    fn documentary_root_is_not_offered_without_explicit_documentary_evidence() {
        let historical_drama = MetadataBundle {
            sonarr: json!({
                "title": "Band of Brothers",
                "genres": ["Action", "Drama", "History", "Mini-Series", "War"],
                "overview": "Based on interviews with survivors and soldiers' journals."
            }),
            tmdb: Some(json!({
                "name": "Band of Brothers",
                "type": "Miniseries",
                "genres": [{ "name": "Drama" }, { "name": "War & Politics" }],
                "keywords": { "results": [{ "name": "historical drama" }] }
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/documentary".to_string(),
                label: Some("Documentary".to_string()),
                description: Some("Documentaries, factual series, and docuseries.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/miniseries".to_string(),
                label: Some("Miniseries".to_string()),
                description: Some("Limited series and miniseries.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/scripted".to_string(),
                label: Some("Scripted".to_string()),
                description: Some("General scripted television.".to_string()),
            },
        ];

        let eligible = eligible_root_folders(&historical_drama, &root_folders);
        let messages = build_messages(&historical_drama, &eligible).expect("messages");
        let prompt = messages
            .iter()
            .find(|message| message.role == "user")
            .expect("user prompt")
            .content
            .as_str();

        assert_eq!(
            eligible,
            vec![root_folders[1].clone(), root_folders[2].clone()]
        );
        assert!(!prompt.contains("/tv/documentary"));
        assert!(prompt.contains("/tv/miniseries"));
        assert!(prompt.contains("/tv/scripted"));
    }

    #[test]
    fn documentary_classification_requires_explicit_documentary_metadata() {
        let talk_show_metadata = MetadataBundle {
            sonarr: json!({
                "title": "The Traitors: Uncloaked",
                "genres": ["Talk Show"]
            }),
            tmdb: Some(json!({
                "name": "The Traitors: Uncloaked",
                "type": "Talk Show",
                "genres": [{ "name": "Talk" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![RootFolderChoice {
            path: "/tv/documentary".to_string(),
            label: Some("Documentary".to_string()),
            description: Some("Documentaries and docuseries.".to_string()),
        }];
        let classification = Classification {
            root_folder_path: "/tv/documentary".to_string(),
            confidence: 0.95,
            reason: "Self cast roles.".to_string(),
            signals: vec!["Self".to_string()],
        };

        let error =
            validate_grounded_classification(&classification, &talk_show_metadata, &root_folders)
                .expect_err("ungrounded documentary classification");

        assert!(
            error
                .to_string()
                .contains("without explicit documentary metadata")
        );
    }

    #[test]
    fn explicit_documentary_metadata_keeps_documentary_root() {
        let documentary = MetadataBundle {
            sonarr: json!({
                "title": "Planet Earth",
                "genres": ["Documentary"]
            }),
            tmdb: None,
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![RootFolderChoice {
            path: "/tv/documentary".to_string(),
            label: Some("Documentary".to_string()),
            description: Some("Documentaries and docuseries.".to_string()),
        }];

        assert_eq!(
            eligible_root_folders(&documentary, &root_folders),
            root_folders
        );
    }

    #[test]
    fn explicit_documentary_metadata_only_offers_documentary_over_reality() {
        let documentary_metadata = MetadataBundle {
            sonarr: json!({
                "title": "America's Sweethearts: Dallas Cowboys Cheerleaders",
                "genres": ["Documentary", "Reality"],
                "seriesType": "standard",
                "network": "Netflix"
            }),
            tmdb: Some(json!({
                "name": "America's Sweethearts: Dallas Cowboys Cheerleaders",
                "type": "Documentary",
                "genres": [{ "name": "Documentary" }, { "name": "Reality" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/documentary".to_string(),
                label: Some("Documentary".to_string()),
                description: Some("Documentaries and docuseries.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/reality".to_string(),
                label: Some("Reality".to_string()),
                description: Some("Reality and unscripted shows.".to_string()),
            },
        ];

        assert_eq!(
            eligible_root_folders(&documentary_metadata, &root_folders),
            vec![root_folders[0].clone()]
        );
    }

    #[test]
    fn weak_documentary_metadata_does_not_beat_explicit_reality() {
        let reality_metadata = MetadataBundle {
            sonarr: json!({
                "title": "Teen Mom: The Next Chapter",
                "genres": ["Reality"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "Teen Mom: The Next Chapter",
                "type": "Reality",
                "genres": [{ "name": "Reality" }, { "name": "Documentary" }]
            })),
            tmdb_error: None,
            tvdb: Some(json!({
                "extended": {
                    "data": {
                        "name": "Teen Mom: The Next Chapter",
                        "genres": [{ "name": "Reality" }]
                    }
                }
            })),
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/documentary".to_string(),
                label: Some("Documentary".to_string()),
                description: Some("Documentaries and docuseries.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/reality".to_string(),
                label: Some("Reality".to_string()),
                description: Some("Reality and unscripted shows.".to_string()),
            },
        ];
        let classification = Classification {
            root_folder_path: "/tv/documentary".to_string(),
            confidence: 0.95,
            reason: "TMDB includes Documentary.".to_string(),
            signals: vec!["Documentary".to_string()],
        };

        assert_eq!(
            eligible_root_folders(&reality_metadata, &root_folders),
            vec![root_folders[1].clone()]
        );
        let error =
            validate_grounded_classification(&classification, &reality_metadata, &root_folders)
                .expect_err("ungrounded documentary classification");
        assert!(
            error
                .to_string()
                .contains("stronger explicit reality metadata")
        );
    }

    #[test]
    fn reality_classification_is_rejected_when_documentary_is_explicit() {
        let documentary_metadata = MetadataBundle {
            sonarr: json!({
                "title": "America's Sweethearts: Dallas Cowboys Cheerleaders",
                "genres": ["Documentary", "Reality"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "America's Sweethearts: Dallas Cowboys Cheerleaders",
                "type": "Documentary",
                "genres": [{ "name": "Documentary" }, { "name": "Reality" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();
        let classification = Classification {
            root_folder_path: "/tv/reality".to_string(),
            confidence: 0.95,
            reason: "Reality genre.".to_string(),
            signals: vec!["Reality".to_string()],
        };

        let error =
            validate_grounded_classification(&classification, &documentary_metadata, &root_folders)
                .expect_err("ungrounded reality classification");

        assert!(error.to_string().contains("explicit documentary metadata"));
    }

    #[test]
    fn mixed_documentary_reality_root_is_allowed_for_documentary_metadata() {
        let documentary_metadata = MetadataBundle {
            sonarr: json!({
                "title": "America's Sweethearts: Dallas Cowboys Cheerleaders",
                "genres": ["Documentary", "Reality"]
            }),
            tmdb: None,
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![RootFolderChoice {
            path: "/tv/documentary-reality".to_string(),
            label: Some("Documentary Reality".to_string()),
            description: Some("Documentaries, docuseries, and unscripted reality.".to_string()),
        }];
        let classification = Classification {
            root_folder_path: "/tv/documentary-reality".to_string(),
            confidence: 0.95,
            reason: "Documentary genre.".to_string(),
            signals: vec!["Documentary".to_string()],
        };

        assert_eq!(
            eligible_root_folders(&documentary_metadata, &root_folders),
            root_folders
        );
        validate_grounded_classification(&classification, &documentary_metadata, &root_folders)
            .expect("mixed documentary/reality root is grounded by documentary metadata");
    }

    #[test]
    fn scripted_classification_is_rejected_when_documentary_is_explicit() {
        let documentary_metadata = MetadataBundle {
            sonarr: json!({
                "title": "Love You to Death: The Kelly Cochran Story",
                "genres": ["Crime", "Documentary"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "Love You to Death: The Kelly Cochran Story",
                "type": "Scripted",
                "genres": [{ "name": "Crime" }, { "name": "Documentary" }]
            })),
            tmdb_error: None,
            tvdb: Some(json!({
                "extended": {
                    "data": {
                        "name": "Love You to Death: The Kelly Cochran Story",
                        "genres": [
                            { "name": "Documentary" },
                            { "name": "Crime" }
                        ]
                    }
                }
            })),
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();
        let classification = Classification {
            root_folder_path: "/tv/scripted".to_string(),
            confidence: 0.95,
            reason: "TMDB type says scripted.".to_string(),
            signals: vec!["type: Scripted".to_string()],
        };

        let error =
            validate_grounded_classification(&classification, &documentary_metadata, &root_folders)
                .expect_err("ungrounded scripted classification");

        assert!(error.to_string().contains("explicit documentary metadata"));
    }

    #[test]
    fn scripted_classification_is_rejected_when_reality_is_explicit() {
        let reality_metadata = MetadataBundle {
            sonarr: json!({
                "title": "90 Day: The Last Resort Between The Sheets",
                "genres": ["Reality"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "90 Day: The Last Resort Between The Sheets",
                "type": "Scripted",
                "genres": [{ "name": "Reality" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/reality".to_string(),
                label: Some("Reality".to_string()),
                description: Some("Reality and unscripted shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/scripted".to_string(),
                label: Some("Scripted".to_string()),
                description: Some("General scripted television.".to_string()),
            },
        ];
        let classification = Classification {
            root_folder_path: "/tv/scripted".to_string(),
            confidence: 0.95,
            reason: "Type is scripted.".to_string(),
            signals: vec!["type: Scripted".to_string()],
        };

        let error =
            validate_grounded_classification(&classification, &reality_metadata, &root_folders)
                .expect_err("ungrounded scripted classification");

        assert!(error.to_string().contains("explicit reality metadata"));
    }

    #[test]
    fn talk_show_classification_is_rejected_when_reality_is_explicit() {
        let reality_metadata = MetadataBundle {
            sonarr: json!({
                "title": "Teen Mom: The Next Chapter",
                "genres": ["Reality"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "Teen Mom: The Next Chapter",
                "type": "Talk Show",
                "genres": [{ "name": "Reality" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/reality".to_string(),
                label: Some("Reality".to_string()),
                description: Some("Reality and unscripted shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/talkshows".to_string(),
                label: Some("Talk Shows".to_string()),
                description: Some("Talk shows, interviews, and late-night shows.".to_string()),
            },
        ];
        let classification = Classification {
            root_folder_path: "/tv/talkshows".to_string(),
            confidence: 0.95,
            reason: "TMDB type says talk show.".to_string(),
            signals: vec!["type: Talk Show".to_string()],
        };

        let error =
            validate_grounded_classification(&classification, &reality_metadata, &root_folders)
                .expect_err("ungrounded talk show classification");

        assert!(error.to_string().contains("explicit reality metadata"));
    }

    #[test]
    fn mixed_reality_talk_show_root_is_allowed_for_reality_metadata() {
        let reality_metadata = MetadataBundle {
            sonarr: json!({
                "title": "Teen Mom: The Next Chapter",
                "genres": ["Reality"],
                "seriesType": "standard"
            }),
            tmdb: Some(json!({
                "name": "Teen Mom: The Next Chapter",
                "type": "Talk Show",
                "genres": [{ "name": "Reality" }]
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![RootFolderChoice {
            path: "/tv/reality-talkshows".to_string(),
            label: Some("Reality Talk Shows".to_string()),
            description: Some("Reality, unscripted, and talk show formats.".to_string()),
        }];
        let classification = Classification {
            root_folder_path: "/tv/reality-talkshows".to_string(),
            confidence: 0.95,
            reason: "Reality genre.".to_string(),
            signals: vec!["Reality".to_string()],
        };

        assert_eq!(
            eligible_root_folders(&reality_metadata, &root_folders),
            root_folders
        );
        validate_grounded_classification(&classification, &reality_metadata, &root_folders)
            .expect("mixed reality/talk-show root is grounded by reality metadata");
    }

    #[test]
    fn miniseries_classification_is_rejected_when_documentary_is_explicit() {
        let documentary_metadata = MetadataBundle {
            sonarr: json!({
                "title": "The Yogurt Shop Murders",
                "genres": ["Crime", "Documentary", "Mini-Series"]
            }),
            tmdb: Some(json!({
                "name": "The Yogurt Shop Murders",
                "type": "Miniseries",
                "aggregate_credits": {
                    "cast": [
                        { "roles": [{ "character": "Self - Lead Investigator" }] },
                        { "roles": [{ "character": "Self - Austin Filmmaker" }] },
                        { "roles": [{ "character": "Self - Amy's Mother" }] }
                    ]
                }
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/documentary".to_string(),
                label: Some("Documentary".to_string()),
                description: Some("Documentaries and docuseries.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/miniseries".to_string(),
                label: Some("Miniseries".to_string()),
                description: Some("Limited series and miniseries.".to_string()),
            },
        ];
        let classification = Classification {
            root_folder_path: "/tv/miniseries".to_string(),
            confidence: 0.99,
            reason: "TMDB type says miniseries.".to_string(),
            signals: vec!["type: Miniseries".to_string()],
        };

        let error =
            validate_grounded_classification(&classification, &documentary_metadata, &root_folders)
                .expect_err("ungrounded miniseries classification");

        assert!(error.to_string().contains("explicit documentary metadata"));
    }

    #[test]
    fn strong_miniseries_metadata_only_offers_miniseries_over_scripted() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "The Witness (2026)",
                "genres": ["Drama", "Mini-Series"],
                "seriesType": "standard",
                "statistics": { "seasonCount": 1, "totalEpisodeCount": 3 }
            }),
            tmdb: Some(json!({
                "name": "The Witness",
                "type": "Miniseries",
                "genres": [{ "name": "Drama" }, { "name": "Crime" }],
                "keywords": { "results": [{ "name": "miniseries" }, { "name": "murder" }] },
                "number_of_episodes": 3,
                "seasons": [{ "name": "Limited Series", "season_number": 1, "episode_count": 3 }]
            })),
            tmdb_error: None,
            tvdb: Some(json!({
                "extended": {
                    "data": {
                        "name": "The Witness (2026)",
                        "type": { "name": "Mini-Series" },
                        "genres": [{ "name": "Mini-Series" }, { "name": "Drama" }]
                    }
                }
            })),
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();

        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert!(
            eligible
                .iter()
                .any(|folder| folder.path == "/tv/miniseries")
        );
        assert!(!eligible.iter().any(|folder| folder.path == "/tv/scripted"));
    }

    #[test]
    fn scripted_classification_is_rejected_when_miniseries_is_strong() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "The Witness (2026)",
                "genres": ["Drama", "Mini-Series"],
                "seriesType": "standard",
                "statistics": { "seasonCount": 1, "totalEpisodeCount": 3 }
            }),
            tmdb: Some(json!({
                "name": "The Witness",
                "type": "Miniseries",
                "number_of_episodes": 3,
                "seasons": [{ "name": "Limited Series", "season_number": 1, "episode_count": 3 }]
            })),
            tmdb_error: None,
            tvdb: Some(json!({
                "extended": {
                    "data": {
                        "type": { "name": "Mini-Series" },
                        "genres": [{ "name": "Mini-Series" }]
                    }
                }
            })),
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();
        let classification = Classification {
            root_folder_path: "/tv/scripted".to_string(),
            confidence: 0.25,
            reason: "Standard scripted format.".to_string(),
            signals: vec!["seriesType: standard".to_string()],
        };

        let error = validate_grounded_classification(&classification, &metadata, &root_folders)
            .expect_err("ungrounded scripted classification");

        assert!(
            error
                .to_string()
                .contains("strong explicit miniseries metadata")
        );
    }

    #[test]
    fn regression_between_the_sheets_prefers_reality() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "90 Day: The Last Resort Between The Sheets",
                "genres": ["Reality"],
                "seriesType": "standard",
                "network": "TLC",
                "overview": "An after-show for 90 Day: The Last Resort."
            }),
            tmdb: Some(json!({
                "name": "90 Day: The Last Resort Between The Sheets",
                "type": "Scripted",
                "genres": [{ "name": "Reality" }],
                "overview": "Cast members provide behind-the-scenes commentary."
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();
        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert_eq!(eligible, vec![root_folders[3].clone()]);
    }

    #[test]
    fn regression_teen_mom_next_chapter_prefers_reality() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "Teen Mom: The Next Chapter",
                "genres": ["Reality"],
                "seriesType": "standard",
                "network": "MTV"
            }),
            tmdb: Some(json!({
                "name": "Teen Mom: The Next Chapter",
                "type": "Reality",
                "genres": [{ "name": "Reality" }, { "name": "Documentary" }],
                "number_of_episodes": 66,
                "number_of_seasons": 2
            })),
            tmdb_error: None,
            tvdb: Some(json!({
                "extended": {
                    "data": {
                        "name": "Teen Mom: The Next Chapter",
                        "genres": [{ "name": "Reality" }]
                    }
                }
            })),
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/documentary".to_string(),
                label: Some("Documentary".to_string()),
                description: Some("Documentaries and docuseries.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/reality".to_string(),
                label: Some("Reality".to_string()),
                description: Some("Reality and unscripted shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/sports".to_string(),
                label: Some("Sports".to_string()),
                description: Some("Sports programming and competition broadcasts.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/talkshows".to_string(),
                label: Some("Talk Shows".to_string()),
                description: Some("Talk shows, interviews, and late-night shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/scripted".to_string(),
                label: Some("Scripted".to_string()),
                description: Some("Default scripted shows.".to_string()),
            },
        ];
        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert_eq!(eligible, vec![root_folders[1].clone()]);
    }

    #[test]
    fn regression_traitors_uncloaked_prefers_talk_shows() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "The Traitors: Uncloaked",
                "genres": ["Talk Show"],
                "seriesType": "standard",
                "network": "BBC One",
                "overview": "Catch up from the castle. The latest banished and murdered have their say with Ed Gamble."
            }),
            tmdb: Some(json!({
                "name": "The Traitors: Uncloaked",
                "type": "Talk Show",
                "genres": [{ "name": "Talk" }],
                "keywords": {
                    "results": [
                        { "name": "game show" },
                        { "name": "behind the scenes" },
                        { "name": "reality tv" },
                        { "name": "podcast" }
                    ]
                },
                "aggregate_credits": {
                    "cast": [
                        { "roles": [{ "character": "Self - Host" }] },
                        { "roles": [{ "character": "Self" }] },
                        { "roles": [{ "character": "Self" }] }
                    ]
                }
            })),
            tmdb_error: None,
            tvdb: Some(json!({
                "extended": {
                    "data": {
                        "name": "The Traitors: Uncloaked",
                        "type": "Talk Show",
                        "genres": [{ "name": "Talk Show" }]
                    }
                }
            })),
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = vec![
            RootFolderChoice {
                path: "/tv/documentary".to_string(),
                label: Some("Documentary".to_string()),
                description: Some("Documentaries and docuseries.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/reality".to_string(),
                label: Some("Reality".to_string()),
                description: Some("Reality and unscripted shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/talkshows".to_string(),
                label: Some("Talk Shows".to_string()),
                description: Some("Talk shows, interviews, and late-night shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/scripted".to_string(),
                label: Some("Scripted".to_string()),
                description: Some("Default scripted shows.".to_string()),
            },
        ];
        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert_eq!(eligible, vec![root_folders[2].clone()]);
    }

    #[test]
    fn regression_cape_fear_keeps_scripted_available() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "Cape Fear",
                "genres": ["Crime", "Drama", "Mini-Series", "Suspense", "Thriller"],
                "seriesType": "standard",
                "network": "Apple TV",
                "statistics": { "totalEpisodeCount": 10 }
            }),
            tmdb: Some(json!({
                "name": "Cape Fear",
                "type": "Miniseries",
                "genres": [{ "name": "Crime" }, { "name": "Drama" }],
                "aggregate_credits": {
                    "cast": [
                        { "roles": [{ "character": "Max Cady" }] },
                        { "roles": [{ "character": "Amanda Bowden" }] }
                    ]
                }
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();
        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert!(eligible.iter().any(|folder| folder.path == "/tv/scripted"));
        assert!(
            eligible
                .iter()
                .any(|folder| folder.path == "/tv/miniseries")
        );
        assert!(
            !eligible
                .iter()
                .any(|folder| folder.path == "/tv/documentary")
        );
    }

    #[test]
    fn regression_yogurt_shop_prefers_documentary() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "The Yogurt Shop Murders",
                "genres": ["Crime", "Documentary", "Mini-Series"],
                "seriesType": "standard",
                "network": "HBO"
            }),
            tmdb: Some(json!({
                "name": "The Yogurt Shop Murders",
                "type": "Miniseries",
                "genres": [{ "name": "Documentary" }, { "name": "Crime" }],
                "aggregate_credits": {
                    "cast": [
                        { "roles": [{ "character": "Self - Lead Investigator" }] },
                        { "roles": [{ "character": "Self - Austin Filmmaker" }] },
                        { "roles": [{ "character": "Self - Amy's Mother" }] }
                    ]
                }
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();
        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert!(
            eligible
                .iter()
                .any(|folder| folder.path == "/tv/documentary")
        );
        let classification = Classification {
            root_folder_path: "/tv/miniseries".to_string(),
            confidence: 0.99,
            reason: "Miniseries label.".to_string(),
            signals: vec!["type: Miniseries".to_string()],
        };
        assert!(
            validate_grounded_classification(&classification, &metadata, &root_folders).is_err()
        );
    }

    #[test]
    fn regression_kelly_cochran_story_prefers_documentary() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "Love You to Death: The Kelly Cochran Story",
                "genres": ["Crime", "Documentary"],
                "seriesType": "standard",
                "network": "Fox Nation"
            }),
            tmdb: Some(json!({
                "name": "Love You to Death: The Kelly Cochran Story",
                "type": "Scripted",
                "genres": [{ "name": "Crime" }, { "name": "Documentary" }],
                "number_of_episodes": 6,
                "number_of_seasons": 1
            })),
            tmdb_error: None,
            tvdb: Some(json!({
                "extended": {
                    "data": {
                        "name": "Love You to Death: The Kelly Cochran Story",
                        "genres": [
                            { "name": "Documentary" },
                            { "name": "Crime" }
                        ]
                    }
                }
            })),
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();
        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert_eq!(eligible, vec![root_folders[0].clone()]);
    }

    #[test]
    fn regression_americas_sweethearts_prefers_documentary() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "America's Sweethearts: Dallas Cowboys Cheerleaders",
                "genres": ["Documentary", "Reality"],
                "seriesType": "standard",
                "network": "Netflix"
            }),
            tmdb: Some(json!({
                "name": "America's Sweethearts: Dallas Cowboys Cheerleaders",
                "type": "Documentary",
                "genres": [{ "name": "Documentary" }, { "name": "Reality" }],
                "number_of_episodes": 7,
                "number_of_seasons": 1
            })),
            tmdb_error: None,
            tvdb: Some(json!({
                "extended": {
                    "data": {
                        "name": "America's Sweethearts: Dallas Cowboys Cheerleaders",
                        "genres": [
                            { "name": "Documentary" },
                            { "name": "Reality" }
                        ]
                    }
                }
            })),
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();
        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert_eq!(eligible, vec![root_folders[0].clone()]);
    }

    #[test]
    fn regression_life_larry_keeps_scripted_available() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "Life, Larry and the Pursuit of Unhappiness",
                "genres": ["Comedy"],
                "seriesType": "standard",
                "network": "HBO",
                "statistics": { "totalEpisodeCount": 7 }
            }),
            tmdb: Some(json!({
                "name": "Life, Larry and the Pursuit of Unhappiness",
                "type": "Miniseries",
                "keywords": { "results": [{ "name": "miniseries" }, { "name": "sketch comedy" }] },
                "aggregate_credits": {
                    "cast": [
                        { "roles": [{ "character": "Various Characters" }] }
                    ]
                }
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();
        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert!(eligible.iter().any(|folder| folder.path == "/tv/scripted"));
        assert!(
            eligible
                .iter()
                .any(|folder| folder.path == "/tv/miniseries")
        );
        assert!(
            !eligible
                .iter()
                .any(|folder| folder.path == "/tv/documentary")
        );
    }

    #[test]
    fn regression_the_witness_prefers_miniseries() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "The Witness (2026)",
                "genres": ["Drama", "Mini-Series"],
                "seriesType": "standard",
                "network": "Netflix",
                "statistics": { "seasonCount": 1, "totalEpisodeCount": 3 }
            }),
            tmdb: Some(json!({
                "name": "The Witness",
                "type": "Miniseries",
                "genres": [{ "name": "Drama" }, { "name": "Crime" }],
                "keywords": { "results": [{ "name": "murder" }, { "name": "miniseries" }] },
                "number_of_seasons": 1,
                "number_of_episodes": 3,
                "seasons": [{ "name": "Limited Series", "season_number": 1, "episode_count": 3 }],
                "aggregate_credits": {
                    "cast": [
                        { "roles": [{ "character": "Andre Hanscombe" }] },
                        { "roles": [{ "character": "Alex Hanscombe" }] }
                    ]
                }
            })),
            tmdb_error: None,
            tvdb: Some(json!({
                "extended": {
                    "data": {
                        "name": "The Witness (2026)",
                        "type": { "name": "Mini-Series" },
                        "genres": [{ "name": "Mini-Series" }, { "name": "Drama" }],
                        "status": { "name": "Continuing" }
                    }
                }
            })),
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();
        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert!(
            eligible
                .iter()
                .any(|folder| folder.path == "/tv/miniseries")
        );
        assert!(!eligible.iter().any(|folder| folder.path == "/tv/scripted"));
    }

    #[test]
    fn regression_shining_girls_does_not_offer_reality() {
        let metadata = MetadataBundle {
            sonarr: json!({
                "title": "Shining Girls",
                "genres": ["Crime", "Drama", "Mini-Series", "Science Fiction", "Thriller"],
                "seriesType": "standard",
                "network": "Apple TV",
                "overview": "Years after a brutal attack left her in a constantly shifting reality, Kirby Mazrachi learns that a recent murder is linked to her assault.",
                "statistics": { "seasonCount": 1, "totalEpisodeCount": 8 }
            }),
            tmdb: Some(json!({
                "name": "Shining Girls",
                "type": "Miniseries",
                "genres": [{ "name": "Crime" }, { "name": "Drama" }, { "name": "Mystery" }, { "name": "Thriller" }],
                "tagline": "Reality is a matter of perspective.",
                "aggregate_credits": {
                    "cast": [
                        { "roles": [{ "character": "Kirby Mazrachi" }] },
                        { "roles": [{ "character": "Dan Velazquez" }] },
                        { "roles": [{ "character": "Harper" }] }
                    ]
                }
            })),
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
        .classification_metadata();
        let root_folders = default_regression_root_folders();
        let eligible = eligible_root_folders(&metadata, &root_folders);

        assert!(!eligible.iter().any(|folder| folder.path == "/tv/reality"));
        assert!(eligible.iter().any(|folder| folder.path == "/tv/scripted"));
    }
    fn default_regression_root_folders() -> Vec<RootFolderChoice> {
        vec![
            RootFolderChoice {
                path: "/tv/documentary".to_string(),
                label: Some("Documentary".to_string()),
                description: Some(
                    "Documentaries, factual, nature, history, science, and docuseries.".to_string(),
                ),
            },
            RootFolderChoice {
                path: "/tv/kids".to_string(),
                label: Some("Kids".to_string()),
                description: Some("Children and family-oriented shows.".to_string()),
            },
            RootFolderChoice {
                path: "/tv/miniseries".to_string(),
                label: Some("Miniseries".to_string()),
                description: Some(
                    "Limited series, short-run event series, and single-season miniseries."
                        .to_string(),
                ),
            },
            RootFolderChoice {
                path: "/tv/reality".to_string(),
                label: Some("Reality".to_string()),
                description: Some(
                    "Reality, competition, lifestyle, and unscripted entertainment.".to_string(),
                ),
            },
            RootFolderChoice {
                path: "/tv/scripted".to_string(),
                label: Some("Scripted".to_string()),
                description: Some(
                    "Default scripted drama, comedy, action, sci-fi, and general TV.".to_string(),
                ),
            },
        ]
    }

    #[test]
    fn error_formatting_preserves_context_chain() {
        let error = anyhow!("operation timed out").context("failed to send Ollama chat request");
        let formatted = format_anyhow_error(&error);

        assert!(formatted.contains("failed to send Ollama chat request"));
        assert!(formatted.contains("operation timed out"));
    }
}
