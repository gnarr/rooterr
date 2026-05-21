use anyhow::Result;
use async_trait::async_trait;

use crate::domain::{
    classification::ClassificationAttempt, metadata::MetadataBundle, root_folder::RootFolderChoice,
};

#[async_trait]
pub trait Classifier: Send + Sync {
    fn provider_name(&self) -> &'static str;
    fn model(&self) -> &str;
    async fn classify(
        &self,
        metadata: &MetadataBundle,
        root_folders: &[RootFolderChoice],
    ) -> Result<ClassificationAttempt>;
}
