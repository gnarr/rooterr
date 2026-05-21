use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};

use crate::{
    config::MetadataConfig,
    domain::{metadata::MetadataBundle, series::SeriesDetails},
    ports::metadata_provider::MetadataProvider,
};

const TMDB_BASE_URL: &str = "https://api.themoviedb.org";
const TVDB_BASE_URL: &str = "https://api4.thetvdb.com/v4";

#[derive(Clone)]
pub struct ExternalMetadataProvider {
    http: Client,
    config: MetadataConfig,
}

impl ExternalMetadataProvider {
    pub fn new(http: Client, config: &MetadataConfig) -> Self {
        Self {
            http,
            config: config.clone(),
        }
    }

    async fn fetch_tmdb(&self, series: &SeriesDetails) -> Result<Option<Value>> {
        let Some(token) = self.config.tmdb_bearer_token.as_deref() else {
            return Ok(None);
        };

        let ids = series.ids();
        let tmdb_id = match ids.tmdb_id {
            Some(id) => Some(id),
            None => {
                let Some(tvdb_id) = ids.tvdb_id else {
                    return Ok(None);
                };
                self.find_tmdb_id_by_tvdb(token, tvdb_id).await?
            }
        };

        let Some(tmdb_id) = tmdb_id else {
            return Ok(None);
        };

        let details_url = format!(
            "{TMDB_BASE_URL}/3/tv/{tmdb_id}?append_to_response=external_ids,keywords,content_ratings,aggregate_credits"
        );
        self.tmdb_get(token, &details_url).await.map(Some)
    }

    async fn find_tmdb_id_by_tvdb(&self, token: &str, tvdb_id: i64) -> Result<Option<i64>> {
        let url = format!("{TMDB_BASE_URL}/3/find/{tvdb_id}?external_source=tvdb_id");
        let value = self.tmdb_get(token, &url).await?;
        Ok(value
            .get("tv_results")
            .and_then(Value::as_array)
            .and_then(|results| results.first())
            .and_then(|item| item.get("id"))
            .and_then(Value::as_i64))
    }

    async fn tmdb_get(&self, token: &str, url: &str) -> Result<Value> {
        let response = self
            .http
            .request(Method::GET, url)
            .bearer_auth(token)
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("failed to send TMDB request to {url}"))?;

        read_json_response("TMDB", response.status(), response.text().await, url).await
    }

    async fn fetch_tvdb(&self, series: &SeriesDetails) -> Result<Value> {
        let Some(api_key) = self.config.tvdb_api_key.as_deref() else {
            bail!("TVDB API key is not configured");
        };

        let Some(tvdb_id) = series.ids().tvdb_id else {
            bail!("Sonarr series has no TVDB ID");
        };

        let token = self.tvdb_login(api_key).await?;
        let extended = self
            .tvdb_get(
                &token,
                &format!("{TVDB_BASE_URL}/series/{tvdb_id}/extended"),
            )
            .await?;

        let translation = match self
            .tvdb_get(
                &token,
                &format!("{TVDB_BASE_URL}/series/{tvdb_id}/translations/eng"),
            )
            .await
        {
            Ok(value) => Some(value),
            Err(error) => Some(json!({ "error": error.to_string() })),
        };

        Ok(json!({
            "extended": extended,
            "translation": translation,
        }))
    }

    async fn tvdb_login(&self, api_key: &str) -> Result<String> {
        let mut body = json!({ "apikey": api_key });
        if let Some(pin) = self.config.tvdb_pin.as_deref() {
            body.as_object_mut()
                .expect("login body object")
                .insert("pin".to_string(), Value::String(pin.to_string()));
        }

        let response = self
            .http
            .request(Method::POST, format!("{TVDB_BASE_URL}/login"))
            .json(&body)
            .send()
            .await
            .context("failed to send TVDB login request")?;

        let value =
            read_json_response("TVDB", response.status(), response.text().await, "/login").await?;
        value
            .get("data")
            .and_then(|data| data.get("token"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow!("TVDB login response did not include data.token"))
    }

    async fn tvdb_get(&self, token: &str, url: &str) -> Result<Value> {
        let response = self
            .http
            .request(Method::GET, url)
            .bearer_auth(token)
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("failed to send TVDB request to {url}"))?;

        read_json_response("TVDB", response.status(), response.text().await, url).await
    }
}

#[async_trait]
impl MetadataProvider for ExternalMetadataProvider {
    async fn enrich(&self, series: SeriesDetails) -> MetadataBundle {
        let mut bundle = MetadataBundle::new(series.clone());

        if self.config.tmdb_bearer_token.is_some() {
            match self.fetch_tmdb(&series).await {
                Ok(value) => bundle.tmdb = value,
                Err(error) => bundle.tmdb_error = Some(error.to_string()),
            }
        }

        if self.config.tvdb_api_key.is_some() {
            match self.fetch_tvdb(&series).await {
                Ok(value) => bundle.tvdb = Some(value),
                Err(error) => bundle.tvdb_error = Some(error.to_string()),
            }
        }

        bundle
    }
}

async fn read_json_response(
    provider: &str,
    status: StatusCode,
    body: reqwest::Result<String>,
    url: &str,
) -> Result<Value> {
    let body = body.with_context(|| format!("failed to read {provider} response from {url}"))?;

    if !status.is_success() {
        let body = body.trim();
        if body.is_empty() {
            bail!("{provider} returned HTTP {status}");
        }
        bail!("{provider} returned HTTP {status}: {body}");
    }

    serde_json::from_str(&body)
        .with_context(|| format!("failed to parse {provider} JSON from {url}"))
}
