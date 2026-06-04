use std::{collections::BTreeMap, sync::Arc};

use chrono::Utc;
use tokio::join;

use crate::{
    config::LlmProvider,
    domain::{
        root_folder::{RootFolder, RootFolderHint},
        status::{
            MetadataServiceType, StatusLevel, StatusOperationalSummary, StatusPageView,
            StatusRootFolderView, StatusSection, summarize_recent_decisions,
        },
    },
    ports::{
        llm_status_probe::LlmStatusProbe, metadata_status_probe::MetadataStatusProbe,
        sonarr_gateway::SonarrGateway,
    },
};

#[derive(Clone)]
pub struct ViewStatus {
    sonarr: Arc<dyn SonarrGateway>,
    llm: Arc<dyn LlmStatusProbe>,
    metadata: Arc<dyn MetadataStatusProbe>,
    config: StatusConfigSnapshot,
}

#[derive(Debug, Clone)]
pub struct StatusConfigSnapshot {
    pub webhook_auth_configured: bool,
    pub sonarr_base_url: String,
    pub llm_provider: LlmProvider,
    pub tmdb_configured: bool,
    pub tvdb_configured: bool,
    pub configured_root_folders: BTreeMap<String, RootFolderHint>,
}

impl ViewStatus {
    pub fn new(
        sonarr: Arc<dyn SonarrGateway>,
        llm: Arc<dyn LlmStatusProbe>,
        metadata: Arc<dyn MetadataStatusProbe>,
        config: StatusConfigSnapshot,
    ) -> Self {
        Self {
            sonarr,
            llm,
            metadata,
            config,
        }
    }

    pub async fn view(
        &self,
        recent_decisions: &[crate::domain::decision::Decision],
    ) -> StatusPageView {
        let (sonarr_result, llm_result, tmdb_result, tvdb_result) = join!(
            self.sonarr.root_folders(),
            self.llm.probe_status(),
            self.probe_tmdb(),
            self.probe_tvdb(),
        );

        let (sonarr, sonarr_root_folders) = match sonarr_result {
            Ok(root_folders) => {
                let count = root_folders.len();
                (
                    StatusSection {
                        level: StatusLevel::Ok,
                        summary: "Sonarr is reachable and the API key is valid".to_string(),
                        details: vec![format!("{} root folder(s) returned by Sonarr", count)],
                        error: None,
                    },
                    map_sonarr_root_folders(root_folders),
                )
            }
            Err(error) => (
                StatusSection {
                    level: StatusLevel::Error,
                    summary: "Sonarr probe failed".to_string(),
                    details: Vec::new(),
                    error: Some(error.to_string()),
                },
                Vec::new(),
            ),
        };

        let llm = match llm_result {
            Ok(result) => {
                let level = match result.model_available {
                    Some(true) | None => StatusLevel::Ok,
                    Some(false) => StatusLevel::Warn,
                };
                let summary = match self.config.llm_provider {
                    LlmProvider::Ollama if result.model_available == Some(true) => {
                        "Ollama is reachable and the configured model is available".to_string()
                    }
                    LlmProvider::Ollama => {
                        "Ollama is reachable but the configured model is missing".to_string()
                    }
                    LlmProvider::OpenAiCompatible => {
                        "OpenAI-compatible endpoint is reachable".to_string()
                    }
                };

                let mut details = vec![format!(
                    "provider: {}",
                    String::from(self.config.llm_provider.clone())
                )];
                if let Some(detail) = result.detail {
                    details.push(detail);
                }

                StatusSection {
                    level,
                    summary,
                    details,
                    error: None,
                }
            }
            Err(error) => StatusSection {
                level: StatusLevel::Error,
                summary: "LLM provider probe failed".to_string(),
                details: vec![format!(
                    "provider: {}",
                    String::from(self.config.llm_provider.clone())
                )],
                error: Some(error.to_string()),
            },
        };

        let tmdb = match tmdb_result {
            Some(result) => result,
            None => StatusSection {
                level: StatusLevel::NotConfigured,
                summary: "TMDB is not configured".to_string(),
                details: Vec::new(),
                error: None,
            },
        };

        let tvdb = match tvdb_result {
            Some(result) => result,
            None => StatusSection {
                level: StatusLevel::NotConfigured,
                summary: "TVDB is not configured".to_string(),
                details: Vec::new(),
                error: None,
            },
        };

        StatusPageView {
            checked_at: Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string(),
            sonarr_base_url: self.config.sonarr_base_url.clone(),
            sonarr,
            llm_provider: String::from(self.config.llm_provider.clone()),
            llm_base_url: self.llm.base_url().to_string(),
            llm_model: self.llm.model().to_string(),
            llm,
            tmdb,
            tvdb,
            configured_root_folders: map_configured_root_folders(
                &self.config.configured_root_folders,
            ),
            sonarr_root_folders,
            operational: StatusOperationalSummary {
                version: env!("CARGO_PKG_VERSION").to_string(),
                webhook_auth_configured: self.config.webhook_auth_configured,
                recent_decisions: summarize_recent_decisions(recent_decisions),
            },
        }
    }

    async fn probe_tmdb(&self) -> Option<StatusSection> {
        if !self.config.tmdb_configured {
            return None;
        }

        Some(
            match self.metadata.probe_service(MetadataServiceType::Tmdb).await {
                Ok(result) => StatusSection {
                    level: StatusLevel::Ok,
                    summary: "TMDB is reachable and authenticated".to_string(),
                    details: result.detail.into_iter().collect(),
                    error: None,
                },
                Err(error) => StatusSection {
                    level: StatusLevel::Error,
                    summary: "TMDB probe failed".to_string(),
                    details: Vec::new(),
                    error: Some(error.to_string()),
                },
            },
        )
    }

    async fn probe_tvdb(&self) -> Option<StatusSection> {
        if !self.config.tvdb_configured {
            return None;
        }

        Some(
            match self.metadata.probe_service(MetadataServiceType::Tvdb).await {
                Ok(result) => StatusSection {
                    level: StatusLevel::Ok,
                    summary: "TVDB is reachable and authenticated".to_string(),
                    details: result.detail.into_iter().collect(),
                    error: None,
                },
                Err(error) => StatusSection {
                    level: StatusLevel::Error,
                    summary: "TVDB probe failed".to_string(),
                    details: Vec::new(),
                    error: Some(error.to_string()),
                },
            },
        )
    }
}

fn map_sonarr_root_folders(folders: Vec<RootFolder>) -> Vec<StatusRootFolderView> {
    folders
        .into_iter()
        .map(|folder| StatusRootFolderView {
            path: folder.path,
            label: None,
            description: None,
        })
        .collect()
}

fn map_configured_root_folders(
    folders: &BTreeMap<String, RootFolderHint>,
) -> Vec<StatusRootFolderView> {
    folders
        .iter()
        .map(|(path, hint)| StatusRootFolderView {
            path: path.clone(),
            label: hint.label.clone(),
            description: hint.description.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use anyhow::{Result, bail};
    use async_trait::async_trait;

    use crate::{
        domain::{
            decision::{Decision, DecisionStatus},
            root_folder::RootFolder,
            status::{LlmStatusProbeResult, MetadataServiceProbeResult},
        },
        ports::{
            llm_status_probe::LlmStatusProbe, metadata_status_probe::MetadataStatusProbe,
            sonarr_gateway::SonarrGateway,
        },
    };

    use super::*;

    #[derive(Clone)]
    struct FakeSonarr {
        root_folders: Result<Vec<RootFolder>, String>,
    }

    #[async_trait]
    impl SonarrGateway for FakeSonarr {
        async fn series(&self, _series_id: i64) -> Result<crate::domain::series::SeriesDetails> {
            bail!("unused")
        }
        async fn root_folders(&self) -> Result<Vec<RootFolder>> {
            self.root_folders.clone().map_err(anyhow::Error::msg)
        }
        async fn series_folder(
            &self,
            _series_id: i64,
        ) -> Result<crate::domain::series::SeriesFolder> {
            bail!("unused")
        }
        async fn move_series(
            &self,
            _series_id: i64,
            _series: &crate::domain::series::SeriesDetails,
            _root_folder_path: &str,
            _destination_path: &str,
        ) -> Result<()> {
            bail!("unused")
        }
    }

    #[derive(Clone)]
    struct FakeLlm {
        base_url: String,
        model: String,
        probe: Result<LlmStatusProbeResult, String>,
    }

    #[async_trait]
    impl LlmStatusProbe for FakeLlm {
        fn base_url(&self) -> &str {
            &self.base_url
        }
        fn model(&self) -> &str {
            &self.model
        }
        async fn probe_status(&self) -> Result<LlmStatusProbeResult> {
            self.probe.clone().map_err(anyhow::Error::msg)
        }
    }

    #[derive(Clone)]
    struct FakeMetadata {
        tmdb: Result<MetadataServiceProbeResult, String>,
        tvdb: Result<MetadataServiceProbeResult, String>,
    }

    #[async_trait]
    impl MetadataStatusProbe for FakeMetadata {
        async fn probe_service(
            &self,
            service: MetadataServiceType,
        ) -> Result<MetadataServiceProbeResult> {
            match service {
                MetadataServiceType::Tmdb => self.tmdb.clone().map_err(anyhow::Error::msg),
                MetadataServiceType::Tvdb => self.tvdb.clone().map_err(anyhow::Error::msg),
            }
        }
    }

    fn config() -> StatusConfigSnapshot {
        StatusConfigSnapshot {
            webhook_auth_configured: true,
            sonarr_base_url: "http://sonarr:8989".to_string(),
            llm_provider: LlmProvider::Ollama,
            tmdb_configured: true,
            tvdb_configured: true,
            configured_root_folders: BTreeMap::from([(
                "/tv/scripted".to_string(),
                RootFolderHint {
                    label: Some("Scripted".to_string()),
                    description: Some("General scripted TV.".to_string()),
                },
            )]),
        }
    }

    fn decision(status: DecisionStatus, updated_at: &str) -> Decision {
        Decision {
            id: 1,
            instance_name: "sonarr".to_string(),
            sonarr_series_id: 7,
            title: Some("Bluey".to_string()),
            year: Some(2018),
            old_path: None,
            selected_root_folder_path: None,
            confidence: None,
            reason: None,
            status,
            error: None,
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            applied_at: None,
        }
    }

    #[tokio::test]
    async fn view_reports_mixed_status_sections() {
        let use_case = ViewStatus::new(
            Arc::new(FakeSonarr {
                root_folders: Ok(vec![RootFolder {
                    path: "/tv/scripted".to_string(),
                }]),
            }),
            Arc::new(FakeLlm {
                base_url: "http://ollama:11434".to_string(),
                model: "qwen3:0.6b".to_string(),
                probe: Ok(LlmStatusProbeResult {
                    model_available: Some(false),
                    detail: Some("configured model missing".to_string()),
                }),
            }),
            Arc::new(FakeMetadata {
                tmdb: Ok(MetadataServiceProbeResult {
                    detail: Some("TMDB ok".to_string()),
                }),
                tvdb: Err("TVDB auth failed".to_string()),
            }),
            config(),
        );

        let view = use_case
            .view(&[
                decision(DecisionStatus::Completed, "2026-06-04 10:00:00"),
                decision(DecisionStatus::Failed, "2026-06-04 09:00:00"),
                decision(DecisionStatus::SkippedLowConfidence, "2026-06-04 08:00:00"),
            ])
            .await;

        assert_eq!(view.sonarr.level, StatusLevel::Ok);
        assert_eq!(view.llm.level, StatusLevel::Warn);
        assert_eq!(view.tmdb.level, StatusLevel::Ok);
        assert_eq!(view.tvdb.level, StatusLevel::Error);
        assert_eq!(view.configured_root_folders.len(), 1);
        assert_eq!(view.sonarr_root_folders.len(), 1);
        assert_eq!(view.operational.recent_decisions.completed, 1);
        assert_eq!(view.operational.recent_decisions.failed, 1);
        assert_eq!(view.operational.recent_decisions.skipped_low_confidence, 1);
    }
}
