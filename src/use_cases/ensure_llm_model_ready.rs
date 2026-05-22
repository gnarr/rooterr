use std::sync::Arc;

use anyhow::Result;

use crate::ports::llm_model_provisioner::LlmModelProvisioner;

#[derive(Clone)]
pub struct EnsureLlmModelReady {
    provisioner: Arc<dyn LlmModelProvisioner>,
}

impl EnsureLlmModelReady {
    pub fn new(provisioner: Arc<dyn LlmModelProvisioner>) -> Self {
        Self { provisioner }
    }

    pub async fn execute(&self) -> Result<()> {
        self.provisioner.ensure_model_ready().await
    }
}
