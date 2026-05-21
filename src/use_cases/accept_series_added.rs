use std::sync::Arc;

use anyhow::{Result, bail};

use crate::{domain::series::SeriesAdded, ports::decision_repository::DecisionRepository};

#[derive(Clone)]
pub struct AcceptSeriesAdded {
    repository: Arc<dyn DecisionRepository>,
}

#[derive(Debug, Clone)]
pub struct AcceptSeriesAddedInput {
    pub event_type: String,
    pub instance_name: Option<String>,
    pub application_url: Option<String>,
    pub series: Option<IncomingSeries>,
}

#[derive(Debug, Clone)]
pub struct IncomingSeries {
    pub sonarr_series_id: i64,
    pub title: Option<String>,
    pub year: Option<i64>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptSeriesAddedOutcome {
    Accepted {
        decision_id: i64,
        sonarr_series_id: i64,
    },
    Duplicate {
        decision_id: i64,
        sonarr_series_id: i64,
    },
    Ignored,
}

impl AcceptSeriesAdded {
    pub fn new(repository: Arc<dyn DecisionRepository>) -> Self {
        Self { repository }
    }

    pub async fn accept(&self, input: AcceptSeriesAddedInput) -> Result<AcceptSeriesAddedOutcome> {
        if input.event_type != "SeriesAdd" {
            return Ok(AcceptSeriesAddedOutcome::Ignored);
        }

        let Some(series) = input.series else {
            bail!("SeriesAdd webhook was missing series");
        };

        let instance_name = input
            .instance_name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                input
                    .application_url
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or("sonarr")
            .to_string();

        let sonarr_series_id = series.sonarr_series_id;
        let insert = self
            .repository
            .insert_decision_if_absent(&SeriesAdded {
                instance_name,
                sonarr_series_id,
                title: series.title,
                year: series.year,
                path: series.path,
            })
            .await?;

        if insert.created {
            Ok(AcceptSeriesAddedOutcome::Accepted {
                decision_id: insert.decision_id,
                sonarr_series_id,
            })
        } else {
            Ok(AcceptSeriesAddedOutcome::Duplicate {
                decision_id: insert.decision_id,
                sonarr_series_id,
            })
        }
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
            decision::{Decision, DecisionStatus, InsertDecisionResult, LlmRun, NewLlmRun},
            metadata::MetadataBundle,
            series::SeriesDetails,
        },
        ports::decision_repository::DecisionRepository,
    };

    #[derive(Default)]
    struct FakeRepository {
        inserts: Mutex<Vec<SeriesAdded>>,
        created: Mutex<bool>,
    }

    #[async_trait]
    impl DecisionRepository for FakeRepository {
        async fn insert_decision_if_absent(
            &self,
            series: &SeriesAdded,
        ) -> Result<InsertDecisionResult> {
            self.inserts.lock().expect("inserts").push(series.clone());
            let created = *self.created.lock().expect("created");
            Ok(InsertDecisionResult {
                decision_id: 7,
                created,
            })
        }

        async fn decision(&self, _id: i64) -> Result<Option<Decision>> {
            unimplemented!()
        }

        async fn list_decisions(&self, _limit: i64) -> Result<Vec<Decision>> {
            unimplemented!()
        }

        async fn update_decision_basics(&self, _id: i64, _series: &SeriesDetails) -> Result<()> {
            unimplemented!()
        }

        async fn mark_status(&self, _id: i64, _status: DecisionStatus) -> Result<()> {
            unimplemented!()
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

    fn series_input() -> IncomingSeries {
        IncomingSeries {
            sonarr_series_id: 42,
            title: Some("Bluey".to_string()),
            year: Some(2018),
            path: Some("/data/tv/Bluey (2018)".to_string()),
        }
    }

    #[tokio::test]
    async fn series_add_starts_new_decision() {
        let repo = Arc::new(FakeRepository::default());
        *repo.created.lock().expect("created") = true;
        let use_case = AcceptSeriesAdded::new(repo.clone());

        let outcome = use_case
            .accept(AcceptSeriesAddedInput {
                event_type: "SeriesAdd".to_string(),
                instance_name: Some("sonarr".to_string()),
                application_url: None,
                series: Some(series_input()),
            })
            .await
            .expect("accept");

        assert_eq!(
            outcome,
            AcceptSeriesAddedOutcome::Accepted {
                decision_id: 7,
                sonarr_series_id: 42
            }
        );
        assert_eq!(repo.inserts.lock().expect("inserts").len(), 1);
    }

    #[tokio::test]
    async fn duplicate_series_add_is_ignored() {
        let repo = Arc::new(FakeRepository::default());
        *repo.created.lock().expect("created") = false;
        let use_case = AcceptSeriesAdded::new(repo);

        let outcome = use_case
            .accept(AcceptSeriesAddedInput {
                event_type: "SeriesAdd".to_string(),
                instance_name: Some("sonarr".to_string()),
                application_url: None,
                series: Some(series_input()),
            })
            .await
            .expect("accept");

        assert_eq!(
            outcome,
            AcceptSeriesAddedOutcome::Duplicate {
                decision_id: 7,
                sonarr_series_id: 42
            }
        );
    }

    #[tokio::test]
    async fn non_series_add_event_is_ignored_without_repository_write() {
        let repo = Arc::new(FakeRepository::default());
        let use_case = AcceptSeriesAdded::new(repo.clone());

        let outcome = use_case
            .accept(AcceptSeriesAddedInput {
                event_type: "Download".to_string(),
                instance_name: None,
                application_url: None,
                series: None,
            })
            .await
            .expect("accept");

        assert_eq!(outcome, AcceptSeriesAddedOutcome::Ignored);
        assert!(repo.inserts.lock().expect("inserts").is_empty());
    }
}
