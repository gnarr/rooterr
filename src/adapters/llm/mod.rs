use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::Client;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    config::{LlmConfig, LlmProvider},
    domain::{
        classification::{ClassificationAttempt, parse_classification_response},
        metadata::MetadataBundle,
        root_folder::RootFolderChoice,
    },
    ports::classifier::Classifier,
};

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
        let body = json!({
            "model": self.config.model,
            "stream": false,
            "format": "json",
            "messages": messages,
            "options": {
                "temperature": self.config.temperature
            }
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
        let messages = build_messages(metadata, root_folders)?;
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
                    error: Some(error.to_string()),
                });
            }
        };
        let allowed_paths = root_folders
            .iter()
            .map(|folder| folder.path.as_str())
            .collect::<Vec<_>>();
        let parsed = parse_classification_response(&raw_response, &allowed_paths).and_then(
            |classification| {
                let parsed_response = serde_json::to_string(&classification)
                    .context("failed to serialize parsed classification")?;
                Ok((classification, parsed_response))
            },
        );

        let (classification, parsed_response, error) = match parsed {
            Ok((classification, parsed_response)) => {
                (Some(classification), Some(parsed_response), None)
            }
            Err(error) => (None, None, Some(error.to_string())),
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

fn build_messages(
    metadata: &MetadataBundle,
    root_folders: &[RootFolderChoice],
) -> Result<Vec<ChatMessage>> {
    let root_folder_json =
        serde_json::to_string_pretty(root_folders).context("failed to encode root folders")?;
    let metadata_json =
        serde_json::to_string_pretty(metadata).context("failed to encode metadata")?;

    Ok(vec![
        ChatMessage {
            role: "system",
            content: concat!(
                "You classify TV series into exactly one Sonarr root folder. ",
                "Return only a JSON object. Do not include markdown. ",
                "The root_folder_path must exactly match one of the provided paths. ",
                "Use confidence from 0.0 to 1.0. ",
                "Prefer obvious categories like anime, documentary, kids, miniseries, reality, scripted, sports, or talkshows when the metadata supports them."
            )
            .to_string(),
        },
        ChatMessage {
            role: "user",
            content: format!(
                "Available root folders:\n{root_folder_json}\n\nSeries metadata:\n{metadata_json}\n\nReturn JSON with keys: root_folder_path, confidence, reason, signals."
            ),
        },
    ])
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
