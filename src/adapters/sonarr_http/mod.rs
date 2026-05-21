use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{
    config::SonarrConfig,
    domain::{
        root_folder::RootFolder,
        series::{SeriesDetails, SeriesFolder},
    },
    ports::sonarr_gateway::SonarrGateway,
};

#[derive(Clone)]
pub struct SonarrHttpGateway {
    http: Client,
    base_url: String,
    api_key: String,
}

#[derive(Debug, Deserialize)]
struct SonarrSeriesFolderResponse {
    folder: String,
}

impl SonarrHttpGateway {
    pub fn new(http: Client, config: &SonarrConfig) -> Self {
        Self {
            http,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            api_key: config.api_key.clone(),
        }
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .http
            .request(Method::GET, self.endpoint(path))
            .header("X-Api-Key", &self.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("failed to send Sonarr request to {path}"))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .with_context(|| format!("failed to read Sonarr response from {path}"))?;
        ensure_success(status, Some(&body))?;

        serde_json::from_str(&body)
            .with_context(|| format!("failed to parse Sonarr JSON from {path}"))
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

#[async_trait]
impl SonarrGateway for SonarrHttpGateway {
    async fn series(&self, series_id: i64) -> Result<SeriesDetails> {
        let raw = self
            .get_json(&format!("/api/v3/series/{series_id}"))
            .await?;
        Ok(SeriesDetails::new(raw))
    }

    async fn root_folders(&self) -> Result<Vec<RootFolder>> {
        self.get_json("/api/v3/rootfolder").await
    }

    async fn series_folder(&self, series_id: i64) -> Result<SeriesFolder> {
        let response: SonarrSeriesFolderResponse = self
            .get_json(&format!("/api/v3/series/{series_id}/folder"))
            .await?;
        Ok(SeriesFolder {
            folder: response.folder,
        })
    }

    async fn move_series(
        &self,
        series_id: i64,
        series: &SeriesDetails,
        root_folder_path: &str,
        destination_path: &str,
    ) -> Result<()> {
        let mut series = series.raw.clone();
        let object = series
            .as_object_mut()
            .ok_or_else(|| anyhow!("Sonarr series response was not a JSON object"))?;
        object.insert(
            "rootFolderPath".to_string(),
            Value::String(root_folder_path.to_string()),
        );
        object.insert(
            "path".to_string(),
            Value::String(destination_path.to_string()),
        );

        let url = self.endpoint(&format!("/api/v3/series/{series_id}"));
        let response = self
            .http
            .request(Method::PUT, url)
            .header("X-Api-Key", &self.api_key)
            .query(&[("moveFiles", "true")])
            .json(&series)
            .send()
            .await
            .context("failed to send Sonarr move request")?;

        ensure_success(response.status(), response.text().await.ok().as_deref())
    }
}

fn ensure_success(status: StatusCode, body: Option<&str>) -> Result<()> {
    if status.is_success() {
        Ok(())
    } else {
        let body = body.unwrap_or("").trim();
        if body.is_empty() {
            bail!("Sonarr returned HTTP {status}");
        } else {
            bail!("Sonarr returned HTTP {status}: {body}");
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_partial_json, method, path, query_param},
    };

    use super::*;

    #[tokio::test]
    async fn move_series_sets_root_folder_path_and_move_files_query() {
        let server = MockServer::start().await;
        let config = SonarrConfig {
            base_url: server.uri(),
            api_key: "key".to_string(),
            webhook_token: None,
        };
        let gateway = SonarrHttpGateway::new(Client::new(), &config);
        let series = SeriesDetails::new(json!({
            "id": 42,
            "title": "Bluey",
            "path": "/data/tv/Bluey (2018)",
            "rootFolderPath": "/data/tv"
        }));

        Mock::given(method("PUT"))
            .and(path("/api/v3/series/42"))
            .and(query_param("moveFiles", "true"))
            .and(body_partial_json(json!({
                "rootFolderPath": "/data/kids",
                "path": "/data/kids/Bluey (2018)"
            })))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;

        gateway
            .move_series(42, &series, "/data/kids", "/data/kids/Bluey (2018)")
            .await
            .expect("move series");
    }
}
