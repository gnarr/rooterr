use anyhow::Result;
use async_trait::async_trait;

use crate::{config::LlmProvider, domain::status::LlmStatusProbeResult};

#[async_trait]
pub trait LlmStatusProbe: Send + Sync {
    fn provider(&self) -> LlmProvider;
    fn base_url(&self) -> &str;
    fn model(&self) -> &str;
    async fn probe_status(&self) -> Result<LlmStatusProbeResult>;
}
