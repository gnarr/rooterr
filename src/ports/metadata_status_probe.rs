use anyhow::Result;
use async_trait::async_trait;

use crate::domain::status::{MetadataServiceProbeResult, MetadataServiceType};

#[async_trait]
pub trait MetadataStatusProbe: Send + Sync {
    async fn probe_service(
        &self,
        service: MetadataServiceType,
    ) -> Result<MetadataServiceProbeResult>;
}
