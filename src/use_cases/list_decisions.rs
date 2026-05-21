use std::sync::Arc;

use anyhow::Result;

use crate::{domain::decision::Decision, ports::decision_repository::DecisionRepository};

#[derive(Clone)]
pub struct ListDecisions {
    repository: Arc<dyn DecisionRepository>,
}

impl ListDecisions {
    pub fn new(repository: Arc<dyn DecisionRepository>) -> Self {
        Self { repository }
    }

    pub async fn list(&self, limit: i64) -> Result<Vec<Decision>> {
        self.repository.list_decisions(limit).await
    }
}
