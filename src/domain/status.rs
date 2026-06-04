use std::path::PathBuf;

use serde::Serialize;

use crate::{config::LlmProvider, domain::decision::DecisionStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StatusLevel {
    Ok,
    Warn,
    Error,
    NotConfigured,
}

impl StatusLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::NotConfigured => "not_configured",
        }
    }

    pub fn badge_class(&self) -> &'static str {
        match self {
            Self::Ok => "status status-ok",
            Self::Warn => "status status-warn",
            Self::Error => "status status-error",
            Self::NotConfigured => "status",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataServiceType {
    Tmdb,
    Tvdb,
}

#[derive(Debug, Clone)]
pub struct LlmStatusProbeResult {
    pub model_available: Option<bool>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MetadataServiceProbeResult {
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusSection {
    pub level: StatusLevel,
    pub summary: String,
    pub details: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusRootFolderView {
    pub path: String,
    pub label: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecentDecisionSummary {
    pub sample_size: usize,
    pub latest_updated_at: Option<String>,
    pub completed: usize,
    pub failed: usize,
    pub skipped_low_confidence: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusOperationalSummary {
    pub version: String,
    pub bind_address: String,
    pub sqlite_path: PathBuf,
    pub webhook_auth_configured: bool,
    pub recent_decisions: RecentDecisionSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusPageView {
    pub checked_at: String,
    pub sonarr_base_url: String,
    pub sonarr: StatusSection,
    pub llm_provider: String,
    pub llm_base_url: String,
    pub llm_model: String,
    pub llm: StatusSection,
    pub tmdb: StatusSection,
    pub tvdb: StatusSection,
    pub configured_root_folders: Vec<StatusRootFolderView>,
    pub sonarr_root_folders: Vec<StatusRootFolderView>,
    pub operational: StatusOperationalSummary,
}

impl From<LlmProvider> for String {
    fn from(value: LlmProvider) -> Self {
        match value {
            LlmProvider::Ollama => "ollama".to_string(),
            LlmProvider::OpenAiCompatible => "openai_compatible".to_string(),
        }
    }
}

pub fn summarize_recent_decisions(
    sample: &[crate::domain::decision::Decision],
) -> RecentDecisionSummary {
    let mut completed = 0;
    let mut failed = 0;
    let mut skipped_low_confidence = 0;

    for decision in sample {
        match decision.status {
            DecisionStatus::Completed => completed += 1,
            DecisionStatus::Failed => failed += 1,
            DecisionStatus::SkippedLowConfidence => skipped_low_confidence += 1,
            _ => {}
        }
    }

    RecentDecisionSummary {
        sample_size: sample.len(),
        latest_updated_at: sample.first().map(|decision| decision.updated_at.clone()),
        completed,
        failed,
        skipped_low_confidence,
    }
}
