use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context, Result, bail};
use tracing::{error, info};

use crate::{
    domain::{
        decision::{DecisionStatus, NewLlmRun},
        root_folder::{RootFolderChoice, RootFolderHint, join_series_path},
    },
    ports::{
        classifier::Classifier, decision_repository::DecisionRepository,
        metadata_provider::MetadataProvider, sonarr_gateway::SonarrGateway,
    },
};

#[derive(Clone)]
pub struct ProcessSeriesDecision {
    repository: Arc<dyn DecisionRepository>,
    sonarr: Arc<dyn SonarrGateway>,
    metadata: Arc<dyn MetadataProvider>,
    classifier: Arc<dyn Classifier>,
    policy: ClassificationPolicy,
}

#[derive(Clone, Debug)]
pub struct ClassificationPolicy {
    pub min_confidence: f64,
    pub root_folders: BTreeMap<String, RootFolderHint>,
}

impl ProcessSeriesDecision {
    pub fn new(
        repository: Arc<dyn DecisionRepository>,
        sonarr: Arc<dyn SonarrGateway>,
        metadata: Arc<dyn MetadataProvider>,
        classifier: Arc<dyn Classifier>,
        policy: ClassificationPolicy,
    ) -> Self {
        Self {
            repository,
            sonarr,
            metadata,
            classifier,
            policy,
        }
    }

    pub async fn run_recording_failure(&self, decision_id: i64, sonarr_series_id: i64) {
        if let Err(error) = self.run(decision_id, sonarr_series_id).await {
            let message = format!("{error:#}");
            error!(decision_id, sonarr_series_id, error = %message, "decision processing failed");
            if let Err(db_error) = self.repository.mark_failed(decision_id, &message).await {
                error!(decision_id, error = %db_error, "failed to persist decision failure");
            }
        }
    }

    pub async fn run(&self, decision_id: i64, sonarr_series_id: i64) -> Result<()> {
        self.repository
            .mark_status(decision_id, DecisionStatus::Processing)
            .await?;

        let series = self
            .sonarr
            .series(sonarr_series_id)
            .await
            .context("failed to fetch Sonarr series")?;
        self.repository
            .update_decision_basics(decision_id, &series)
            .await?;

        let root_folders = self
            .sonarr
            .root_folders()
            .await
            .context("failed to fetch Sonarr root folders")?;
        if root_folders.is_empty() {
            bail!("Sonarr returned no root folders");
        }

        let root_folder_choices = root_folders
            .into_iter()
            .map(|folder| {
                let hint = self.policy.root_folders.get(&folder.path);
                RootFolderChoice {
                    path: folder.path,
                    label: hint.and_then(|hint| hint.label.clone()),
                    description: hint.and_then(|hint| hint.description.clone()),
                }
            })
            .collect::<Vec<_>>();

        let metadata = self.metadata.enrich(series.clone()).await;
        self.repository
            .insert_metadata_snapshot(decision_id, &metadata)
            .await?;

        let attempt = self
            .classifier
            .classify(&metadata, &root_folder_choices)
            .await
            .context("failed to run LLM classification")?;
        self.repository
            .insert_llm_run(NewLlmRun {
                decision_id,
                provider: self.classifier.provider_name().to_string(),
                model: self.classifier.model().to_string(),
                prompt_hash: attempt.prompt_hash.clone(),
                raw_response: attempt.raw_response.clone(),
                parsed_response: attempt.parsed_response.clone(),
                duration_ms: Some(attempt.duration_ms),
                error: attempt.error.clone(),
            })
            .await?;

        if let Some(error) = attempt.error {
            bail!("LLM classification failed: {error}");
        }
        let classification = attempt
            .classification
            .context("LLM classification did not return a result")?;

        if classification.confidence < self.policy.min_confidence {
            self.repository
                .mark_skipped_low_confidence(decision_id, &classification)
                .await?;
            info!(
                decision_id,
                confidence = classification.confidence,
                "decision skipped because confidence is below threshold"
            );
            return Ok(());
        }

        self.repository
            .mark_applying(decision_id, &classification)
            .await?;

        let folder = self
            .sonarr
            .series_folder(sonarr_series_id)
            .await
            .context("failed to fetch Sonarr generated series folder")?;
        let destination_path = join_series_path(&classification.root_folder_path, &folder.folder);
        self.sonarr
            .move_series(
                sonarr_series_id,
                &series,
                &classification.root_folder_path,
                &destination_path,
            )
            .await
            .context("failed to update Sonarr series root folder")?;

        self.repository.mark_completed(decision_id).await?;
        info!(decision_id, sonarr_series_id, "decision completed");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use anyhow::{Result, bail};
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::{
        domain::{
            classification::{Classification, ClassificationAttempt},
            decision::{Decision, InsertDecisionResult, LlmRun},
            metadata::MetadataBundle,
            root_folder::RootFolder,
            series::{SeriesAdded, SeriesDetails, SeriesFolder},
        },
        ports::{
            classifier::Classifier, decision_repository::DecisionRepository,
            metadata_provider::MetadataProvider, sonarr_gateway::SonarrGateway,
        },
    };

    #[derive(Default)]
    struct RepoState {
        statuses: Vec<DecisionStatus>,
        failed: Vec<String>,
        applying: Option<Classification>,
        completed: bool,
        skipped: Option<Classification>,
        snapshots: usize,
        llm_runs: Vec<NewLlmRun>,
    }

    #[derive(Default)]
    struct FakeRepository {
        state: Mutex<RepoState>,
    }

    #[async_trait]
    impl DecisionRepository for FakeRepository {
        async fn insert_decision_if_absent(
            &self,
            _series: &SeriesAdded,
        ) -> Result<InsertDecisionResult> {
            unimplemented!()
        }

        async fn decision(&self, _id: i64) -> Result<Option<Decision>> {
            unimplemented!()
        }

        async fn list_decisions(&self, _limit: i64) -> Result<Vec<Decision>> {
            unimplemented!()
        }

        async fn update_decision_basics(&self, _id: i64, _series: &SeriesDetails) -> Result<()> {
            Ok(())
        }

        async fn mark_status(&self, _id: i64, status: DecisionStatus) -> Result<()> {
            self.state.lock().expect("state").statuses.push(status);
            Ok(())
        }

        async fn mark_failed(&self, _id: i64, error: &str) -> Result<()> {
            self.state
                .lock()
                .expect("state")
                .failed
                .push(error.to_string());
            Ok(())
        }

        async fn mark_applying(&self, _id: i64, classification: &Classification) -> Result<()> {
            self.state.lock().expect("state").applying = Some(classification.clone());
            Ok(())
        }

        async fn mark_completed(&self, _id: i64) -> Result<()> {
            self.state.lock().expect("state").completed = true;
            Ok(())
        }

        async fn mark_skipped_low_confidence(
            &self,
            _id: i64,
            classification: &Classification,
        ) -> Result<()> {
            self.state.lock().expect("state").skipped = Some(classification.clone());
            Ok(())
        }

        async fn insert_metadata_snapshot(
            &self,
            _decision_id: i64,
            _metadata: &MetadataBundle,
        ) -> Result<()> {
            self.state.lock().expect("state").snapshots += 1;
            Ok(())
        }

        async fn latest_metadata_snapshot(&self, _decision_id: i64) -> Result<Option<String>> {
            unimplemented!()
        }

        async fn insert_llm_run(&self, run: NewLlmRun) -> Result<()> {
            self.state.lock().expect("state").llm_runs.push(run);
            Ok(())
        }

        async fn llm_runs(&self, _decision_id: i64) -> Result<Vec<LlmRun>> {
            unimplemented!()
        }
    }

    struct FakeSonarr {
        fail_series: bool,
        fail_move: bool,
        moves: Mutex<Vec<(i64, String, String)>>,
    }

    impl FakeSonarr {
        fn new() -> Self {
            Self {
                fail_series: false,
                fail_move: false,
                moves: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl SonarrGateway for FakeSonarr {
        async fn series(&self, _series_id: i64) -> Result<SeriesDetails> {
            if self.fail_series {
                bail!("sonarr fetch failed");
            }

            Ok(SeriesDetails::new(json!({
                "id": 42,
                "title": "Bluey",
                "year": 2018,
                "path": "/data/tv/Bluey (2018)",
                "rootFolderPath": "/data/tv",
                "tvdbId": 353546,
                "tmdbId": 82728,
                "imdbId": "tt7678620",
                "seriesType": "standard",
                "overview": "Bluey follows a Blue Heeler puppy and her family.",
                "genres": ["Animation", "Children"]
            })))
        }

        async fn root_folders(&self) -> Result<Vec<RootFolder>> {
            Ok(vec![
                RootFolder {
                    path: "/data/scripted".to_string(),
                },
                RootFolder {
                    path: "/data/kids".to_string(),
                },
            ])
        }

        async fn series_folder(&self, _series_id: i64) -> Result<SeriesFolder> {
            Ok(SeriesFolder {
                folder: "Bluey (2018)".to_string(),
            })
        }

        async fn move_series(
            &self,
            series_id: i64,
            _series: &SeriesDetails,
            root_folder_path: &str,
            destination_path: &str,
        ) -> Result<()> {
            if self.fail_move {
                bail!("sonarr move failed");
            }

            self.moves.lock().expect("moves").push((
                series_id,
                root_folder_path.to_string(),
                destination_path.to_string(),
            ));
            Ok(())
        }
    }

    struct FakeMetadata;

    #[async_trait]
    impl MetadataProvider for FakeMetadata {
        async fn enrich(&self, series: SeriesDetails) -> MetadataBundle {
            MetadataBundle::new(series)
        }
    }

    struct FakeClassifier {
        attempt: ClassificationAttempt,
    }

    #[async_trait]
    impl Classifier for FakeClassifier {
        fn provider_name(&self) -> &'static str {
            "fake"
        }

        fn model(&self) -> &str {
            "fake-model"
        }

        async fn classify(
            &self,
            _metadata: &MetadataBundle,
            _root_folders: &[RootFolderChoice],
        ) -> Result<ClassificationAttempt> {
            Ok(self.attempt.clone())
        }
    }

    fn classification(confidence: f64) -> Classification {
        Classification {
            root_folder_path: "/data/kids".to_string(),
            confidence,
            reason: "Animated children's series with kids genre metadata.".to_string(),
            signals: vec!["Animation".to_string(), "Children".to_string()],
        }
    }

    fn attempt(
        classification: Option<Classification>,
        error: Option<&str>,
    ) -> ClassificationAttempt {
        ClassificationAttempt {
            classification,
            raw_response: Some("raw".to_string()),
            parsed_response: Some("parsed".to_string()),
            prompt_hash: "hash".to_string(),
            duration_ms: 10,
            error: error.map(ToOwned::to_owned),
        }
    }

    fn use_case(
        repository: Arc<FakeRepository>,
        sonarr: Arc<FakeSonarr>,
        classifier: Arc<FakeClassifier>,
    ) -> ProcessSeriesDecision {
        ProcessSeriesDecision::new(
            repository,
            sonarr,
            Arc::new(FakeMetadata),
            classifier,
            ClassificationPolicy {
                min_confidence: 0.55,
                root_folders: BTreeMap::new(),
            },
        )
    }

    #[tokio::test]
    async fn successful_classification_applies_sonarr_move() {
        let repository = Arc::new(FakeRepository::default());
        let sonarr = Arc::new(FakeSonarr::new());
        let classifier = Arc::new(FakeClassifier {
            attempt: attempt(Some(classification(0.92)), None),
        });
        let use_case = use_case(repository.clone(), sonarr.clone(), classifier);

        use_case.run(7, 42).await.expect("run");

        let state = repository.state.lock().expect("state");
        assert!(state.completed);
        assert!(state.applying.is_some());
        assert_eq!(state.snapshots, 1);
        assert_eq!(state.llm_runs.len(), 1);
        drop(state);
        assert_eq!(
            sonarr.moves.lock().expect("moves").as_slice(),
            &[(
                42,
                "/data/kids".to_string(),
                "/data/kids/Bluey (2018)".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn low_confidence_classification_skips_move() {
        let repository = Arc::new(FakeRepository::default());
        let sonarr = Arc::new(FakeSonarr::new());
        let classifier = Arc::new(FakeClassifier {
            attempt: attempt(Some(classification(0.25)), None),
        });
        let use_case = use_case(repository.clone(), sonarr.clone(), classifier);

        use_case.run(7, 42).await.expect("run");

        let state = repository.state.lock().expect("state");
        assert!(state.skipped.is_some());
        assert!(!state.completed);
        drop(state);
        assert!(sonarr.moves.lock().expect("moves").is_empty());
    }

    #[tokio::test]
    async fn llm_error_records_failed_status() {
        let repository = Arc::new(FakeRepository::default());
        let sonarr = Arc::new(FakeSonarr::new());
        let classifier = Arc::new(FakeClassifier {
            attempt: attempt(None, Some("LLM selected unknown root folder path")),
        });
        let use_case = use_case(repository.clone(), sonarr, classifier);

        use_case.run_recording_failure(7, 42).await;

        let state = repository.state.lock().expect("state");
        assert!(state.failed[0].contains("LLM classification failed"));
    }

    #[tokio::test]
    async fn sonarr_fetch_failure_records_failed_status() {
        let repository = Arc::new(FakeRepository::default());
        let mut sonarr = FakeSonarr::new();
        sonarr.fail_series = true;
        let classifier = Arc::new(FakeClassifier {
            attempt: attempt(Some(classification(0.92)), None),
        });
        let use_case = use_case(repository.clone(), Arc::new(sonarr), classifier);

        use_case.run_recording_failure(7, 42).await;

        let state = repository.state.lock().expect("state");
        assert!(state.failed[0].contains("failed to fetch Sonarr series"));
    }

    #[tokio::test]
    async fn sonarr_apply_failure_records_failed_status() {
        let repository = Arc::new(FakeRepository::default());
        let mut sonarr = FakeSonarr::new();
        sonarr.fail_move = true;
        let classifier = Arc::new(FakeClassifier {
            attempt: attempt(Some(classification(0.92)), None),
        });
        let use_case = use_case(repository.clone(), Arc::new(sonarr), classifier);

        use_case.run_recording_failure(7, 42).await;

        let state = repository.state.lock().expect("state");
        assert!(state.failed[0].contains("failed to update Sonarr series root folder"));
    }
}
