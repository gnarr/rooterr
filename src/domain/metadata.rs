use serde::Serialize;
use serde_json::Value;

use crate::domain::series::SeriesDetails;

#[derive(Debug, Clone, Serialize)]
pub struct MetadataBundle {
    pub sonarr: Value,
    pub tmdb: Option<Value>,
    pub tmdb_error: Option<String>,
    pub tvdb: Option<Value>,
    pub tvdb_error: Option<String>,
}

impl MetadataBundle {
    pub fn new(series: SeriesDetails) -> Self {
        Self {
            sonarr: series.raw,
            tmdb: None,
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        }
    }
}
