use std::sync::Arc;

use anyhow::Result;

use crate::{domain::decision::DecisionStatus, ports::decision_repository::DecisionRepository};

#[derive(Clone)]
pub struct RetryDecision {
    repository: Arc<dyn DecisionRepository>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecisionOutcome {
    RetryQueued { sonarr_series_id: i64 },
    NotFound,
}

impl RetryDecision {
    pub fn new(repository: Arc<dyn DecisionRepository>) -> Self {
        Self { repository }
    }

    pub async fn retry(&self, decision_id: i64) -> Result<RetryDecisionOutcome> {
        let Some(decision) = self.repository.decision(decision_id).await? else {
            return Ok(RetryDecisionOutcome::NotFound);
        };

        self.repository
            .mark_status(decision_id, DecisionStatus::Received)
            .await?;

        Ok(RetryDecisionOutcome::RetryQueued {
            sonarr_series_id: decision.sonarr_series_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use anyhow::Result;
    use async_trait::async_trait;

    use super::*;
    use crate::{
        domain::{
            classification::Classification,
            decision::{Decision, InsertDecisionResult, LlmRun, NewLlmRun},
            metadata::MetadataBundle,
            series::{SeriesAdded, SeriesDetails},
        },
        ports::decision_repository::DecisionRepository,
    };

    struct FakeRepository {
        decision: Option<Decision>,
        statuses: Mutex<Vec<DecisionStatus>>,
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
            Ok(self.decision.clone())
        }

        async fn list_decisions(&self, _limit: i64) -> Result<Vec<Decision>> {
            unimplemented!()
        }

        async fn update_decision_basics(&self, _id: i64, _series: &SeriesDetails) -> Result<()> {
            unimplemented!()
        }

        async fn mark_status(&self, _id: i64, status: DecisionStatus) -> Result<()> {
            self.statuses.lock().expect("statuses").push(status);
            Ok(())
        }

        async fn mark_failed(&self, _id: i64, _error: &str) -> Result<()> {
            unimplemented!()
        }

        async fn mark_applying(&self, _id: i64, _classification: &Classification) -> Result<()> {
            unimplemented!()
        }

        async fn mark_completed(&self, _id: i64) -> Result<()> {
            unimplemented!()
        }

        async fn mark_skipped_low_confidence(
            &self,
            _id: i64,
            _classification: &Classification,
        ) -> Result<()> {
            unimplemented!()
        }

        async fn insert_metadata_snapshot(
            &self,
            _decision_id: i64,
            _metadata: &MetadataBundle,
        ) -> Result<()> {
            unimplemented!()
        }

        async fn latest_metadata_snapshot(&self, _decision_id: i64) -> Result<Option<String>> {
            unimplemented!()
        }

        async fn insert_llm_run(&self, _run: NewLlmRun) -> Result<()> {
            unimplemented!()
        }

        async fn llm_runs(&self, _decision_id: i64) -> Result<Vec<LlmRun>> {
            unimplemented!()
        }
    }

    fn decision() -> Decision {
        Decision {
            id: 7,
            instance_name: "sonarr".to_string(),
            sonarr_series_id: 42,
            title: Some("Bluey".to_string()),
            year: Some(2018),
            old_path: None,
            selected_root_folder_path: None,
            confidence: None,
            reason: None,
            status: DecisionStatus::Failed,
            error: Some("failed".to_string()),
            created_at: "now".to_string(),
            updated_at: "now".to_string(),
            applied_at: None,
        }
    }

    #[tokio::test]
    async fn retry_resets_decision_and_returns_sonarr_series_id() {
        let repo = Arc::new(FakeRepository {
            decision: Some(decision()),
            statuses: Mutex::new(Vec::new()),
        });
        let use_case = RetryDecision::new(repo.clone());

        let outcome = use_case.retry(7).await.expect("retry");

        assert_eq!(
            outcome,
            RetryDecisionOutcome::RetryQueued {
                sonarr_series_id: 42
            }
        );
        assert_eq!(
            repo.statuses.lock().expect("statuses").as_slice(),
            &[DecisionStatus::Received]
        );
    }

    #[tokio::test]
    async fn retry_reports_missing_decision() {
        let repo = Arc::new(FakeRepository {
            decision: None,
            statuses: Mutex::new(Vec::new()),
        });
        let use_case = RetryDecision::new(repo);

        assert_eq!(
            use_case.retry(7).await.expect("retry"),
            RetryDecisionOutcome::NotFound
        );
    }
}
