use std::sync::Arc;

use anyhow::{Context, Result};
use reqwest::Client;

use crate::{
    adapters::{
        decision_events::{DecisionEventHub, NotifyingDecisionRepository},
        llm::LocalLlmClassifier,
        metadata::ExternalMetadataProvider,
        sonarr_http::SonarrHttpGateway,
        sqlite::SqliteDecisionRepository,
    },
    config::Config,
    ports::{
        classifier::Classifier, decision_repository::DecisionRepository,
        llm_model_provisioner::LlmModelProvisioner, metadata_provider::MetadataProvider,
        sonarr_gateway::SonarrGateway,
    },
    use_cases::{
        accept_series_added::AcceptSeriesAdded,
        ensure_llm_model_ready::EnsureLlmModelReady,
        list_decisions::ListDecisions,
        process_series_decision::{ClassificationPolicy, ProcessSeriesDecision},
        retry_decision::RetryDecision,
        view_decision::ViewDecision,
    },
};

#[derive(Clone)]
pub struct AppServices {
    pub webhook_token: Option<String>,
    pub decision_events: DecisionEventHub,
    pub accept_series_added: AcceptSeriesAdded,
    pub process_series_decision: ProcessSeriesDecision,
    pub retry_decision: RetryDecision,
    pub list_decisions: ListDecisions,
    pub view_decision: ViewDecision,
}

impl AppServices {
    pub async fn new(config: Config) -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("rooterr/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("failed to build HTTP client")?;

        let decision_events = DecisionEventHub::new();
        let sqlite_repository: Arc<dyn DecisionRepository> =
            Arc::new(SqliteDecisionRepository::new(&config.database.sqlite_path)?);
        let repository: Arc<dyn DecisionRepository> = Arc::new(NotifyingDecisionRepository::new(
            sqlite_repository,
            decision_events.clone(),
        ));
        let sonarr: Arc<dyn SonarrGateway> =
            Arc::new(SonarrHttpGateway::new(http.clone(), &config.sonarr));
        let metadata: Arc<dyn MetadataProvider> = Arc::new(ExternalMetadataProvider::new(
            http.clone(),
            &config.metadata,
        ));
        let llm = Arc::new(LocalLlmClassifier::new(http, &config.llm));
        if config.llm.auto_pull {
            let provisioner: Arc<dyn LlmModelProvisioner> = llm.clone();
            EnsureLlmModelReady::new(provisioner).execute().await?;
        }
        let classifier: Arc<dyn Classifier> = llm;
        let policy = ClassificationPolicy {
            min_confidence: config.classification.min_confidence,
            root_folders: config.classification.root_folders.clone(),
        };

        Ok(Self {
            webhook_token: config.sonarr.webhook_token,
            decision_events,
            accept_series_added: AcceptSeriesAdded::new(repository.clone()),
            process_series_decision: ProcessSeriesDecision::new(
                repository.clone(),
                sonarr,
                metadata,
                classifier,
                policy,
            ),
            retry_decision: RetryDecision::new(repository.clone()),
            list_decisions: ListDecisions::new(repository.clone()),
            view_decision: ViewDecision::new(repository),
        })
    }
}
