use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::{
    domain::{
        classification::Classification,
        decision::{Decision, DecisionStatus, InsertDecisionResult, LlmRun, NewLlmRun},
        metadata::MetadataBundle,
        series::{SeriesAdded, SeriesDetails},
    },
    ports::decision_repository::DecisionRepository,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionEventKind {
    Created,
    Updated,
}

impl DecisionEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "decision-created",
            Self::Updated => "decision-updated",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionEvent {
    pub kind: DecisionEventKind,
    pub decision_id: i64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DecisionEventPayload {
    pub id: i64,
}

impl From<DecisionEvent> for DecisionEventPayload {
    fn from(event: DecisionEvent) -> Self {
        Self {
            id: event.decision_id,
        }
    }
}

#[derive(Clone)]
pub struct DecisionEventHub {
    sender: broadcast::Sender<DecisionEvent>,
}

impl DecisionEventHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DecisionEvent> {
        self.sender.subscribe()
    }

    fn publish(&self, event: DecisionEvent) {
        let _ = self.sender.send(event);
    }
}

#[derive(Clone)]
pub struct NotifyingDecisionRepository {
    repository: Arc<dyn DecisionRepository>,
    events: DecisionEventHub,
}

impl NotifyingDecisionRepository {
    pub fn new(repository: Arc<dyn DecisionRepository>, events: DecisionEventHub) -> Self {
        Self { repository, events }
    }

    fn publish_created(&self, decision_id: i64) {
        self.events.publish(DecisionEvent {
            kind: DecisionEventKind::Created,
            decision_id,
        });
    }

    fn publish_updated(&self, decision_id: i64) {
        self.events.publish(DecisionEvent {
            kind: DecisionEventKind::Updated,
            decision_id,
        });
    }
}

#[async_trait]
impl DecisionRepository for NotifyingDecisionRepository {
    async fn insert_decision_if_absent(
        &self,
        series: &SeriesAdded,
    ) -> Result<InsertDecisionResult> {
        let inserted = self.repository.insert_decision_if_absent(series).await?;
        if inserted.created {
            self.publish_created(inserted.decision_id);
        }
        Ok(inserted)
    }

    async fn decision(&self, id: i64) -> Result<Option<Decision>> {
        self.repository.decision(id).await
    }

    async fn list_decisions(&self, limit: i64) -> Result<Vec<Decision>> {
        self.repository.list_decisions(limit).await
    }

    async fn update_decision_basics(&self, id: i64, series: &SeriesDetails) -> Result<()> {
        self.repository.update_decision_basics(id, series).await?;
        self.publish_updated(id);
        Ok(())
    }

    async fn mark_status(&self, id: i64, status: DecisionStatus) -> Result<()> {
        self.repository.mark_status(id, status).await?;
        self.publish_updated(id);
        Ok(())
    }

    async fn mark_failed(&self, id: i64, error: &str) -> Result<()> {
        self.repository.mark_failed(id, error).await?;
        self.publish_updated(id);
        Ok(())
    }

    async fn mark_applying(&self, id: i64, classification: &Classification) -> Result<()> {
        self.repository.mark_applying(id, classification).await?;
        self.publish_updated(id);
        Ok(())
    }

    async fn mark_completed(&self, id: i64) -> Result<()> {
        self.repository.mark_completed(id).await?;
        self.publish_updated(id);
        Ok(())
    }

    async fn mark_skipped_low_confidence(
        &self,
        id: i64,
        classification: &Classification,
    ) -> Result<()> {
        self.repository
            .mark_skipped_low_confidence(id, classification)
            .await?;
        self.publish_updated(id);
        Ok(())
    }

    async fn insert_metadata_snapshot(
        &self,
        decision_id: i64,
        metadata: &MetadataBundle,
    ) -> Result<()> {
        self.repository
            .insert_metadata_snapshot(decision_id, metadata)
            .await?;
        self.publish_updated(decision_id);
        Ok(())
    }

    async fn latest_metadata_snapshot(&self, decision_id: i64) -> Result<Option<String>> {
        self.repository.latest_metadata_snapshot(decision_id).await
    }

    async fn insert_llm_run(&self, run: NewLlmRun) -> Result<()> {
        let decision_id = run.decision_id;
        self.repository.insert_llm_run(run).await?;
        self.publish_updated(decision_id);
        Ok(())
    }

    async fn llm_runs(&self, decision_id: i64) -> Result<Vec<LlmRun>> {
        self.repository.llm_runs(decision_id).await
    }
}

#[cfg(test)]
mod tests {
    use anyhow::bail;
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::time::{Duration, timeout};

    use super::*;
    use crate::adapters::sqlite::SqliteDecisionRepository;

    fn repository() -> (TempDir, NotifyingDecisionRepository, DecisionEventHub) {
        let temp = TempDir::new().expect("temp dir");
        let sqlite: Arc<dyn DecisionRepository> = Arc::new(
            SqliteDecisionRepository::new(&temp.path().join("rooterr.sqlite3")).expect("sqlite"),
        );
        let events = DecisionEventHub::new();
        (
            temp,
            NotifyingDecisionRepository::new(sqlite, events.clone()),
            events,
        )
    }

    fn series_added() -> SeriesAdded {
        SeriesAdded {
            instance_name: "sonarr".to_string(),
            sonarr_series_id: 42,
            title: Some("Bluey".to_string()),
            year: Some(2018),
            path: Some("/data/tv/Bluey (2018)".to_string()),
        }
    }

    fn classification() -> Classification {
        Classification {
            root_folder_path: "/data/kids".to_string(),
            confidence: 0.94,
            reason: "Family series".to_string(),
            signals: vec!["Children".to_string()],
        }
    }

    async fn next_event(receiver: &mut broadcast::Receiver<DecisionEvent>) -> DecisionEvent {
        timeout(Duration::from_millis(100), receiver.recv())
            .await
            .expect("event timeout")
            .expect("event")
    }

    #[tokio::test]
    async fn emits_created_only_for_new_decisions() {
        let (_temp, repo, events) = repository();
        let mut receiver = events.subscribe();

        let inserted = repo
            .insert_decision_if_absent(&series_added())
            .await
            .expect("insert");
        assert_eq!(
            next_event(&mut receiver).await,
            DecisionEvent {
                kind: DecisionEventKind::Created,
                decision_id: inserted.decision_id,
            }
        );

        repo.insert_decision_if_absent(&series_added())
            .await
            .expect("duplicate");
        assert!(
            timeout(Duration::from_millis(20), receiver.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn emits_updated_after_display_relevant_writes() {
        let (_temp, repo, events) = repository();
        let inserted = repo
            .insert_decision_if_absent(&series_added())
            .await
            .expect("insert");
        let mut receiver = events.subscribe();

        repo.update_decision_basics(
            inserted.decision_id,
            &SeriesDetails::new(json!({
                "title": "Bluey",
                "year": 2018,
                "path": "/data/tv/Bluey (2018)"
            })),
        )
        .await
        .expect("basics");
        repo.mark_status(inserted.decision_id, DecisionStatus::Processing)
            .await
            .expect("status");
        repo.mark_applying(inserted.decision_id, &classification())
            .await
            .expect("applying");
        repo.mark_completed(inserted.decision_id)
            .await
            .expect("completed");
        repo.mark_skipped_low_confidence(inserted.decision_id, &classification())
            .await
            .expect("skipped");
        repo.mark_failed(inserted.decision_id, "failed")
            .await
            .expect("failed");
        repo.insert_metadata_snapshot(
            inserted.decision_id,
            &MetadataBundle {
                sonarr: json!({ "title": "Bluey" }),
                tmdb: None,
                tmdb_error: None,
                tvdb: None,
                tvdb_error: None,
            },
        )
        .await
        .expect("metadata");
        repo.insert_llm_run(NewLlmRun {
            decision_id: inserted.decision_id,
            provider: "test".to_string(),
            model: "model".to_string(),
            prompt_hash: "hash".to_string(),
            raw_response: None,
            parsed_response: None,
            duration_ms: Some(1),
            error: None,
        })
        .await
        .expect("llm run");

        for _ in 0..8 {
            assert_eq!(
                next_event(&mut receiver).await,
                DecisionEvent {
                    kind: DecisionEventKind::Updated,
                    decision_id: inserted.decision_id,
                }
            );
        }
    }

    struct FailingRepository;

    #[async_trait]
    impl DecisionRepository for FailingRepository {
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
            unimplemented!()
        }

        async fn mark_status(&self, _id: i64, _status: DecisionStatus) -> Result<()> {
            bail!("write failed")
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

    #[tokio::test]
    async fn does_not_emit_when_write_fails() {
        let events = DecisionEventHub::new();
        let repo = NotifyingDecisionRepository::new(Arc::new(FailingRepository), events.clone());
        let mut receiver = events.subscribe();

        assert!(
            repo.mark_status(5, DecisionStatus::Processing)
                .await
                .is_err()
        );
        assert!(
            timeout(Duration::from_millis(20), receiver.recv())
                .await
                .is_err()
        );
    }
}
