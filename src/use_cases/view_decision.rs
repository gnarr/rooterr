use std::sync::Arc;

use anyhow::Result;

use crate::{
    domain::decision::{Decision, LlmRun},
    ports::decision_repository::DecisionRepository,
};

#[derive(Clone)]
pub struct ViewDecision {
    repository: Arc<dyn DecisionRepository>,
}

#[derive(Debug, Clone)]
pub struct DecisionView {
    pub decision: Decision,
    pub metadata_snapshot: Option<String>,
    pub llm_runs: Vec<LlmRun>,
}

impl ViewDecision {
    pub fn new(repository: Arc<dyn DecisionRepository>) -> Self {
        Self { repository }
    }

    pub async fn decision(&self, decision_id: i64) -> Result<Option<Decision>> {
        self.repository.decision(decision_id).await
    }

    pub async fn view(&self, decision_id: i64) -> Result<Option<DecisionView>> {
        let Some(decision) = self.decision(decision_id).await? else {
            return Ok(None);
        };
        let metadata_snapshot = self
            .repository
            .latest_metadata_snapshot(decision_id)
            .await?;
        let llm_runs = self.repository.llm_runs(decision_id).await?;

        Ok(Some(DecisionView {
            decision,
            metadata_snapshot,
            llm_runs,
        }))
    }
}
