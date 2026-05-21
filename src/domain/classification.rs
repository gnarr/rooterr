use std::collections::BTreeSet;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Classification {
    pub root_folder_path: String,
    pub confidence: f64,
    pub reason: String,
    #[serde(default)]
    pub signals: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ClassificationAttempt {
    pub classification: Option<Classification>,
    pub raw_response: Option<String>,
    pub parsed_response: Option<String>,
    pub prompt_hash: String,
    pub duration_ms: i64,
    pub error: Option<String>,
}

pub fn parse_classification_response(raw: &str, allowed_paths: &[&str]) -> Result<Classification> {
    let json_slice = extract_json_object(raw)?;
    let classification: Classification = serde_json::from_str(json_slice)
        .context("LLM response was not valid classification JSON")?;

    if !classification.confidence.is_finite()
        || classification.confidence < 0.0
        || classification.confidence > 1.0
    {
        bail!("LLM confidence must be between 0.0 and 1.0");
    }

    if classification.reason.trim().is_empty() {
        bail!("LLM response reason must not be empty");
    }

    let allowed = allowed_paths.iter().copied().collect::<BTreeSet<_>>();
    if !allowed.contains(classification.root_folder_path.as_str()) {
        bail!(
            "LLM selected unknown root folder path '{}'",
            classification.root_folder_path
        );
    }

    Ok(classification)
}

fn extract_json_object(raw: &str) -> Result<&str> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Ok(trimmed);
    }

    let start = trimmed
        .find('{')
        .ok_or_else(|| anyhow!("LLM response did not contain a JSON object"))?;
    let end = trimmed
        .rfind('}')
        .ok_or_else(|| anyhow!("LLM response did not contain a complete JSON object"))?;

    if end <= start {
        bail!("LLM response did not contain a complete JSON object");
    }

    Ok(&trimmed[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_classification_response() {
        let parsed = parse_classification_response(
            r#"{"root_folder_path":"/data/kids","confidence":0.91,"reason":"Animated children's series","signals":["Animation","Children"]}"#,
            &["/data/kids", "/data/scripted"],
        )
        .expect("classification");

        assert_eq!(parsed.root_folder_path, "/data/kids");
        assert_eq!(parsed.signals, vec!["Animation", "Children"]);
    }

    #[test]
    fn parses_json_wrapped_in_text() {
        let parsed = parse_classification_response(
            r#"```json
            {"root_folder_path":"/data/kids","confidence":0.91,"reason":"Animated children's series","signals":[]}
            ```"#,
            &["/data/kids"],
        )
        .expect("classification");

        assert_eq!(parsed.root_folder_path, "/data/kids");
    }

    #[test]
    fn rejects_unknown_root_folder() {
        let err = parse_classification_response(
            r#"{"root_folder_path":"/data/anime","confidence":0.91,"reason":"Anime","signals":[]}"#,
            &["/data/kids"],
        )
        .expect_err("unknown path");

        assert!(err.to_string().contains("unknown root folder"));
    }

    #[test]
    fn rejects_invalid_json() {
        let err =
            parse_classification_response("not json", &["/data/kids"]).expect_err("invalid json");

        assert!(err.to_string().contains("JSON object"));
    }

    #[test]
    fn rejects_out_of_range_confidence() {
        let err = parse_classification_response(
            r#"{"root_folder_path":"/data/kids","confidence":1.2,"reason":"x","signals":[]}"#,
            &["/data/kids"],
        )
        .expect_err("confidence");

        assert!(err.to_string().contains("confidence"));
    }
}
