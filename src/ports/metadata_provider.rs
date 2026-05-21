use async_trait::async_trait;

use crate::domain::{metadata::MetadataBundle, series::SeriesDetails};

#[async_trait]
pub trait MetadataProvider: Send + Sync {
    async fn enrich(&self, series: SeriesDetails) -> MetadataBundle;
}
