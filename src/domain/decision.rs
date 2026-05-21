use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum DecisionStatus {
    Received,
    Processing,
    Applying,
    Completed,
    Failed,
    SkippedLowConfidence,
    Unknown(String),
}

impl DecisionStatus {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Received => "received",
            Self::Processing => "processing",
            Self::Applying => "applying",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::SkippedLowConfidence => "skipped_low_confidence",
            Self::Unknown(value) => value,
        }
    }
}

impl From<&str> for DecisionStatus {
    fn from(value: &str) -> Self {
        match value {
            "received" => Self::Received,
            "processing" => Self::Processing,
            "applying" => Self::Applying,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "skipped_low_confidence" => Self::SkippedLowConfidence,
            other => Self::Unknown(other.to_string()),
        }
    }
}

impl From<String> for DecisionStatus {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub id: i64,
    pub instance_name: String,
    pub sonarr_series_id: i64,
    pub title: Option<String>,
    pub year: Option<i64>,
    pub old_path: Option<String>,
    pub selected_root_folder_path: Option<String>,
    pub confidence: Option<f64>,
    pub reason: Option<String>,
    pub status: DecisionStatus,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub applied_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmRun {
    pub id: i64,
    pub provider: String,
    pub model: String,
    pub prompt_hash: String,
    pub raw_response: Option<String>,
    pub parsed_response: Option<String>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct NewLlmRun {
    pub decision_id: i64,
    pub provider: String,
    pub model: String,
    pub prompt_hash: String,
    pub raw_response: Option<String>,
    pub parsed_response: Option<String>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsertDecisionResult {
    pub decision_id: i64,
    pub created: bool,
}
