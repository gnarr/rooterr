use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait LlmModelProvisioner: Send + Sync {
    async fn ensure_model_ready(&self) -> Result<()>;
}
