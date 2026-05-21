use anyhow::Result;
use async_trait::async_trait;

use crate::domain::{
    root_folder::RootFolder,
    series::{SeriesDetails, SeriesFolder},
};

#[async_trait]
pub trait SonarrGateway: Send + Sync {
    async fn series(&self, series_id: i64) -> Result<SeriesDetails>;
    async fn root_folders(&self) -> Result<Vec<RootFolder>>;
    async fn series_folder(&self, series_id: i64) -> Result<SeriesFolder>;
    async fn move_series(
        &self,
        series_id: i64,
        series: &SeriesDetails,
        root_folder_path: &str,
        destination_path: &str,
    ) -> Result<()>;
}
