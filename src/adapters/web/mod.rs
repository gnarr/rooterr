use std::{collections::HashMap, sync::Arc};

use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect},
    routing::{get, post},
};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use serde::Deserialize;

use crate::{
    bootstrap::AppServices,
    domain::decision::{Decision, DecisionStatus, LlmRun},
    use_cases::{
        accept_series_added::{AcceptSeriesAddedInput, AcceptSeriesAddedOutcome, IncomingSeries},
        retry_decision::RetryDecisionOutcome,
    },
};

pub fn router(services: Arc<AppServices>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/webhooks/sonarr", post(sonarr_webhook))
        .route("/series/{id}", get(series_detail))
        .route("/series/{id}/retry", post(retry_series))
        .with_state(services)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SonarrWebhookPayload {
    event_type: String,
    instance_name: Option<String>,
    application_url: Option<String>,
    series: Option<WebhookSeries>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebhookSeries {
    id: i64,
    title: Option<String>,
    path: Option<String>,
    year: Option<i64>,
}

impl From<SonarrWebhookPayload> for AcceptSeriesAddedInput {
    fn from(payload: SonarrWebhookPayload) -> Self {
        Self {
            event_type: payload.event_type,
            instance_name: payload.instance_name,
            application_url: payload.application_url,
            series: payload.series.map(|series| IncomingSeries {
                sonarr_series_id: series.id,
                title: series.title,
                year: series.year,
                path: series.path,
            }),
        }
    }
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn sonarr_webhook(
    State(services): State<Arc<AppServices>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    axum::Json(payload): axum::Json<SonarrWebhookPayload>,
) -> impl IntoResponse {
    if !webhook_token_valid(&services, &headers, &query) {
        return (StatusCode::UNAUTHORIZED, "invalid webhook token").into_response();
    }

    let outcome = services.accept_series_added.accept(payload.into()).await;
    match outcome {
        Ok(AcceptSeriesAddedOutcome::Ignored) => {
            (StatusCode::ACCEPTED, "ignored event").into_response()
        }
        Ok(AcceptSeriesAddedOutcome::Duplicate { .. }) => {
            (StatusCode::OK, "duplicate webhook ignored").into_response()
        }
        Ok(AcceptSeriesAddedOutcome::Accepted {
            decision_id,
            sonarr_series_id,
        }) => {
            let services_for_task = services.clone();
            tokio::spawn(async move {
                services_for_task
                    .process_series_decision
                    .run_recording_failure(decision_id, sonarr_series_id)
                    .await;
            });

            (StatusCode::ACCEPTED, "accepted").into_response()
        }
        Err(error) if error.to_string().contains("missing series") => (
            StatusCode::BAD_REQUEST,
            "SeriesAdd webhook was missing series",
        )
            .into_response(),
        Err(error) => {
            tracing::error!(error = %error, "failed to accept webhook decision");
            (StatusCode::INTERNAL_SERVER_ERROR, "failed to store webhook").into_response()
        }
    }
}

async fn index(State(services): State<Arc<AppServices>>) -> impl IntoResponse {
    match services.list_decisions.list(200).await {
        Ok(decisions) => page(
            "Rooterr",
            html! {
                h1 { "Rooterr" }
                section class="panel" {
                    h2 { "Decision History" }
                    @if decisions.is_empty() {
                        p class="muted" { "No Sonarr SeriesAdd webhooks have been processed yet." }
                    } @else {
                        table {
                            thead {
                                tr {
                                    th { "Series" }
                                    th { "Chosen Root" }
                                    th { "Confidence" }
                                    th { "Status" }
                                    th { "Updated" }
                                }
                            }
                            tbody {
                                @for decision in &decisions {
                                    tr {
                                        td {
                                            a href=(format!("/series/{}", decision.id)) {
                                                (series_title(decision))
                                            }
                                        }
                                        td { (decision.selected_root_folder_path.as_deref().unwrap_or("-")) }
                                        td { (format_confidence(decision.confidence)) }
                                        td { span class=(status_class(&decision.status)) { (decision.status.as_str()) } }
                                        td { (&decision.updated_at) }
                                    }
                                }
                            }
                        }
                    }
                }
            },
        )
        .into_response(),
        Err(error) => {
            tracing::error!(error = %error, "failed to load decisions");
            (StatusCode::INTERNAL_SERVER_ERROR, "failed to load decisions").into_response()
        }
    }
}

async fn series_detail(
    State(services): State<Arc<AppServices>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let decision_view = match services.view_decision.view(id).await {
        Ok(Some(decision_view)) => decision_view,
        Ok(None) => return (StatusCode::NOT_FOUND, "decision not found").into_response(),
        Err(error) => {
            tracing::error!(id, error = %error, "failed to load decision");
            return (StatusCode::INTERNAL_SERVER_ERROR, "failed to load decision").into_response();
        }
    };
    let decision = decision_view.decision;
    let metadata = decision_view.metadata_snapshot;
    let llm_runs = decision_view.llm_runs;

    page(
        &series_title(&decision),
        html! {
            nav { a href="/" { "Decision History" } }
            h1 { (series_title(&decision)) }
            section class="panel grid" {
                div { strong { "Status" } span class=(status_class(&decision.status)) { (decision.status.as_str()) } }
                div { strong { "Chosen root" } span { (decision.selected_root_folder_path.as_deref().unwrap_or("-")) } }
                div { strong { "Confidence" } span { (format_confidence(decision.confidence)) } }
                div { strong { "Sonarr ID" } span { (decision.sonarr_series_id) } }
                div { strong { "Old path" } span { (decision.old_path.as_deref().unwrap_or("-")) } }
                div { strong { "Applied at" } span { (decision.applied_at.as_deref().unwrap_or("-")) } }
            }
            section class="panel" {
                h2 { "Reason" }
                p { (decision.reason.as_deref().unwrap_or("-")) }
                @if let Some(error) = &decision.error {
                    h2 { "Error" }
                    pre class="error-block" { (error) }
                }
                form method="post" action=(format!("/series/{}/retry", decision.id)) {
                    button type="submit" { "Retry classification" }
                }
            }
            section class="panel" {
                h2 { "LLM Runs" }
                @for run in &llm_runs {
                    (llm_run(run))
                }
            }
            section class="panel" {
                h2 { "Metadata Snapshot" }
                @if let Some(metadata) = metadata {
                    pre { (metadata) }
                } @else {
                    p class="muted" { "No metadata snapshot has been recorded yet." }
                }
            }
        },
    )
    .into_response()
}

async fn retry_series(
    State(services): State<Arc<AppServices>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match services.retry_decision.retry(id).await {
        Ok(RetryDecisionOutcome::NotFound) => {
            return (StatusCode::NOT_FOUND, "decision not found").into_response();
        }
        Ok(RetryDecisionOutcome::RetryQueued { sonarr_series_id }) => {
            let services_for_task = services.clone();
            tokio::spawn(async move {
                services_for_task
                    .process_series_decision
                    .run_recording_failure(id, sonarr_series_id)
                    .await;
            });
        }
        Err(error) => {
            tracing::error!(id, error = %error, "failed to retry decision");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to retry decision",
            )
                .into_response();
        }
    }

    Redirect::to(&format!("/series/{id}")).into_response()
}

fn webhook_token_valid(
    services: &AppServices,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> bool {
    let Some(expected) = services.webhook_token.as_deref() else {
        return true;
    };

    query.get("token").map(String::as_str) == Some(expected)
        || headers
            .get("x-rooterr-token")
            .and_then(|value| value.to_str().ok())
            == Some(expected)
        || headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            == Some(expected)
}

fn page(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                style { (PreEscaped(STYLE)) }
            }
            body {
                main {
                    (body)
                }
            }
        }
    }
}

fn llm_run(run: &LlmRun) -> Markup {
    html! {
        article class="run" {
            h3 { (&run.provider) " / " (&run.model) }
            p class="muted" { "Prompt hash " code { (&run.prompt_hash) } " at " (&run.created_at) }
            @if let Some(duration) = run.duration_ms {
                p class="muted" { "Duration: " (duration) " ms" }
            }
            @if let Some(error) = &run.error {
                pre class="error-block" { (error) }
            }
            @if let Some(parsed) = &run.parsed_response {
                h4 { "Parsed response" }
                pre { (parsed) }
            }
            @if let Some(raw) = &run.raw_response {
                h4 { "Raw response" }
                pre { (raw) }
            }
        }
    }
}

fn series_title(decision: &Decision) -> String {
    match (&decision.title, decision.year) {
        (Some(title), Some(year)) => format!("{title} ({year})"),
        (Some(title), None) => title.clone(),
        (None, _) => format!("Sonarr series {}", decision.sonarr_series_id),
    }
}

fn format_confidence(confidence: Option<f64>) -> String {
    confidence
        .map(|value| format!("{:.0}%", value * 100.0))
        .unwrap_or_else(|| "-".to_string())
}

fn status_class(status: &DecisionStatus) -> &'static str {
    match status {
        DecisionStatus::Completed => "status status-ok",
        DecisionStatus::Failed => "status status-error",
        DecisionStatus::SkippedLowConfidence => "status status-warn",
        DecisionStatus::Processing | DecisionStatus::Applying | DecisionStatus::Received => {
            "status status-active"
        }
        DecisionStatus::Unknown(_) => "status",
    }
}

const STYLE: &str = r#"
:root {
  color-scheme: light dark;
  --bg: #f7f7f4;
  --panel: #ffffff;
  --text: #1f2428;
  --muted: #667076;
  --border: #d7d9d7;
  --accent: #2f6f73;
  --ok: #1c7c54;
  --warn: #946200;
  --error: #a83232;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #171918;
    --panel: #202322;
    --text: #edf0ee;
    --muted: #aab2ae;
    --border: #3a403d;
  }
}
* { box-sizing: border-box; }
body {
  margin: 0;
  background: var(--bg);
  color: var(--text);
  font: 15px/1.45 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}
main {
  width: min(1160px, calc(100vw - 32px));
  margin: 32px auto;
}
h1 { margin: 0 0 18px; font-size: 30px; }
h2 { margin: 0 0 14px; font-size: 18px; }
h3 { margin: 0 0 8px; font-size: 15px; }
h4 { margin: 14px 0 6px; font-size: 13px; color: var(--muted); }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
.panel {
  background: var(--panel);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 18px;
  margin: 14px 0;
}
.grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(230px, 1fr));
  gap: 12px;
}
.grid div {
  display: grid;
  gap: 4px;
}
.grid strong {
  color: var(--muted);
  font-size: 12px;
  text-transform: uppercase;
}
table {
  width: 100%;
  border-collapse: collapse;
}
th, td {
  padding: 10px 8px;
  border-bottom: 1px solid var(--border);
  text-align: left;
  vertical-align: top;
}
th { color: var(--muted); font-size: 12px; text-transform: uppercase; }
.status {
  display: inline-block;
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 2px 8px;
  font-size: 12px;
}
.status-ok { color: var(--ok); border-color: var(--ok); }
.status-warn { color: var(--warn); border-color: var(--warn); }
.status-error { color: var(--error); border-color: var(--error); }
.status-active { color: var(--accent); border-color: var(--accent); }
.muted { color: var(--muted); }
pre {
  overflow: auto;
  max-height: 520px;
  padding: 12px;
  border-radius: 6px;
  background: color-mix(in srgb, var(--bg), var(--panel) 35%);
  border: 1px solid var(--border);
}
.error-block {
  color: var(--error);
}
button {
  appearance: none;
  border: 1px solid var(--accent);
  border-radius: 6px;
  background: var(--accent);
  color: white;
  padding: 8px 12px;
  font: inherit;
  cursor: pointer;
}
.run {
  border-top: 1px solid var(--border);
  padding-top: 12px;
  margin-top: 12px;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_series_add_webhook() {
        let payload: SonarrWebhookPayload = serde_json::from_str(
            r#"{
                "eventType": "SeriesAdd",
                "instanceName": "sonarr",
                "series": {
                    "id": 42,
                    "title": "Bluey",
                    "path": "/data/tv/Bluey (2018)",
                    "tvdbId": 353546,
                    "tmdbId": 82728,
                    "imdbId": "tt7678620",
                    "type": "standard",
                    "year": 2018,
                    "genres": ["Animation", "Children"]
                }
            }"#,
        )
        .expect("payload");

        assert_eq!(payload.event_type, "SeriesAdd");
        assert_eq!(payload.instance_name.as_deref(), Some("sonarr"));
        assert_eq!(payload.series.expect("series").id, 42);
    }

    #[test]
    fn deserializes_non_series_add_events() {
        let payload: SonarrWebhookPayload =
            serde_json::from_str(r#"{"eventType":"Download"}"#).expect("payload");

        assert_eq!(payload.event_type, "Download");
        assert!(payload.series.is_none());
    }
}
