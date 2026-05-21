use anyhow::Result;
use async_trait::async_trait;

use crate::domain::{
    classification::Classification,
    decision::{Decision, DecisionStatus, InsertDecisionResult, LlmRun, NewLlmRun},
    metadata::MetadataBundle,
    series::{SeriesAdded, SeriesDetails},
};

#[async_trait]
pub trait DecisionRepository: Send + Sync {
    async fn insert_decision_if_absent(&self, series: &SeriesAdded)
    -> Result<InsertDecisionResult>;
    async fn decision(&self, id: i64) -> Result<Option<Decision>>;
    async fn list_decisions(&self, limit: i64) -> Result<Vec<Decision>>;
    async fn update_decision_basics(&self, id: i64, series: &SeriesDetails) -> Result<()>;
    async fn mark_status(&self, id: i64, status: DecisionStatus) -> Result<()>;
    async fn mark_failed(&self, id: i64, error: &str) -> Result<()>;
    async fn mark_applying(&self, id: i64, classification: &Classification) -> Result<()>;
    async fn mark_completed(&self, id: i64) -> Result<()>;
    async fn mark_skipped_low_confidence(
        &self,
        id: i64,
        classification: &Classification,
    ) -> Result<()>;
    async fn insert_metadata_snapshot(
        &self,
        decision_id: i64,
        metadata: &MetadataBundle,
    ) -> Result<()>;
    async fn latest_metadata_snapshot(&self, decision_id: i64) -> Result<Option<String>>;
    async fn insert_llm_run(&self, run: NewLlmRun) -> Result<()>;
    async fn llm_runs(&self, decision_id: i64) -> Result<Vec<LlmRun>>;
}
