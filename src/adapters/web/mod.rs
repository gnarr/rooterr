use std::{collections::HashMap, convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Redirect,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use serde::Deserialize;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use crate::{
    adapters::decision_events::DecisionEventPayload,
    bootstrap::AppServices,
    domain::{
        decision::{Decision, DecisionStatus, LlmRun},
        status::{RecentDecisionSummary, StatusPageView, StatusRootFolderView, StatusSection},
    },
    use_cases::{
        accept_series_added::{AcceptSeriesAddedInput, AcceptSeriesAddedOutcome, IncomingSeries},
        retry_decision::RetryDecisionOutcome,
        view_decision::DecisionView,
    },
};

pub fn router(services: Arc<AppServices>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/status", get(status_page))
        .route("/events/decisions", get(decision_events))
        .route("/healthz", get(healthz))
        .route("/decision-history/{id}/row", get(decision_history_row))
        .route("/webhooks/sonarr", post(sonarr_webhook))
        .route("/series/{id}", get(series_detail))
        .route("/series/{id}/content", get(series_detail_content))
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

async fn decision_events(
    State(services): State<Arc<AppServices>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(services.decision_events.subscribe()).filter_map(|event| {
        let event = event.ok()?;
        let payload = serde_json::to_string(&DecisionEventPayload::from(event)).ok()?;

        Some(Ok(Event::default()
            .event(event.kind.as_str())
            .data(payload)))
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
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
                (top_nav(None))
                (decision_history(&decisions))
                script { (PreEscaped(HISTORY_SCRIPT)) }
            },
        )
        .into_response(),
        Err(error) => {
            tracing::error!(error = %error, "failed to load decisions");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to load decisions",
            )
                .into_response()
        }
    }
}

async fn status_page(State(services): State<Arc<AppServices>>) -> impl IntoResponse {
    let recent_decisions = match services.list_decisions.list(25).await {
        Ok(decisions) => decisions,
        Err(error) => {
            tracing::error!(error = %error, "failed to load recent decisions for status page");
            Vec::new()
        }
    };

    let view = services.view_status.view(&recent_decisions).await;
    page(
        "Rooterr Status",
        html! {
            (top_nav(Some("status")))
            (status_content(&view))
        },
    )
    .into_response()
}

async fn decision_history_row(
    State(services): State<Arc<AppServices>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match services.view_decision.decision(id).await {
        Ok(Some(decision)) => history_row(&decision).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "decision not found").into_response(),
        Err(error) => {
            tracing::error!(id, error = %error, "failed to load decision history row");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to load decision history row",
            )
                .into_response()
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
    page(
        &series_title(&decision_view.decision),
        html! {
            (top_nav(Some("series_detail")))
            (series_content_region(&decision_view))
            script { (PreEscaped(DETAIL_SCRIPT)) }
        },
    )
    .into_response()
}

fn top_nav(active: Option<&str>) -> Markup {
    html! {
        nav class="top-nav" {
            a href="/" class=[active.is_none().then_some("active")] { "Decision History" }
            a href="/status" class=[(active == Some("status")).then_some("active")] { "Status" }
        }
    }
}

async fn series_detail_content(
    State(services): State<Arc<AppServices>>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match services.view_decision.view(id).await {
        Ok(Some(view)) => series_content(&view).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "decision not found").into_response(),
        Err(error) => {
            tracing::error!(id, error = %error, "failed to load series detail content");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to load series detail content",
            )
                .into_response()
        }
    }
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

const HISTORY_SCRIPT: &str = r#"
(() => {
  const table = document.getElementById("decision-history-table");
  const rows = document.getElementById("decision-history-rows");
  const empty = document.getElementById("decision-history-empty");
  if (!table || !rows || !window.EventSource) return;

  const parseId = (event) => {
    try {
      return JSON.parse(event.data).id;
    } catch (_) {
      return null;
    }
  };

  const fetchRow = async (id) => {
    const response = await fetch(`/decision-history/${id}/row`, {
      headers: { "X-Requested-With": "rooterr-live-update" }
    });
    if (!response.ok) return null;

    const template = document.createElement("template");
    template.innerHTML = await response.text();
    return template.content.querySelector("tr");
  };

  const source = new EventSource("/events/decisions");
  source.addEventListener("decision-created", async (event) => {
    const id = parseId(event);
    if (id == null) return;

    const current = document.getElementById(`decision-row-${id}`);
    const row = await fetchRow(id);
    if (!row) return;

    if (current) {
      current.replaceWith(row);
      return;
    }

    rows.prepend(row);
    table.hidden = false;
    if (empty) empty.hidden = true;

    const limit = Number.parseInt(table.dataset.limit || "200", 10);
    while (rows.children.length > limit) {
      rows.lastElementChild?.remove();
    }
  });

  source.addEventListener("decision-updated", async (event) => {
    const id = parseId(event);
    if (id == null) return;

    const current = document.getElementById(`decision-row-${id}`);
    if (!current) return;

    const row = await fetchRow(id);
    if (row) current.replaceWith(row);
  });
})();
"#;

const DETAIL_SCRIPT: &str = r#"
(() => {
  const content = document.getElementById("series-detail-content");
  if (!content || !window.EventSource) return;

  const id = Number.parseInt(content.dataset.decisionId || "", 10);
  if (!Number.isFinite(id)) return;

  const refresh = async (event) => {
    try {
      if (JSON.parse(event.data).id !== id) return;
    } catch (_) {
      return;
    }

    const response = await fetch(`/series/${id}/content`, {
      headers: { "X-Requested-With": "rooterr-live-update" }
    });
    if (response.ok) content.innerHTML = await response.text();
  };

  const source = new EventSource("/events/decisions");
  source.addEventListener("decision-created", refresh);
  source.addEventListener("decision-updated", refresh);
})();
"#;

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

fn history_row(decision: &Decision) -> Markup {
    html! {
        tr id=(format!("decision-row-{}", decision.id)) data-decision-id=(decision.id) {
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

fn decision_history(decisions: &[Decision]) -> Markup {
    html! {
        h1 { "Rooterr" }
        section class="panel" {
            h2 { "Decision History" }
            p id="decision-history-empty" class="muted" hidden[!decisions.is_empty()] {
                "No Sonarr SeriesAdd webhooks have been processed yet."
            }
            table id="decision-history-table" data-limit="200" hidden[decisions.is_empty()] {
                thead {
                    tr {
                        th { "Series" }
                        th { "Chosen Root" }
                        th { "Confidence" }
                        th { "Status" }
                        th { "Updated" }
                    }
                }
                tbody id="decision-history-rows" {
                    @for decision in decisions {
                        (history_row(decision))
                    }
                }
            }
        }
    }
}

fn status_content(view: &StatusPageView) -> Markup {
    html! {
        h1 { "System Status" }
        p class="muted" { "Checked at " (&view.checked_at) }
        section class="panel grid" {
            div { strong { "Version" } span { (&view.operational.version) } }
            div {
                strong { "Webhook auth" }
                span { (if view.operational.webhook_auth_configured { "configured" } else { "disabled" }) }
            }
        }
        section class="panel" {
            h2 { "Dependencies" }
            (status_section(
                "Sonarr",
                Some(&view.sonarr_base_url),
                None,
                &view.sonarr,
            ))
            (status_section(
                "LLM",
                Some(&format!("{} / {}", view.llm_provider, view.llm_base_url)),
                Some(html! { p class="muted" { "Configured model: " code { (&view.llm_model) } } }),
                &view.llm,
            ))
            (status_section("TMDB", None, None, &view.tmdb))
            (status_section("TVDB", None, None, &view.tvdb))
        }
        section class="panel" {
            h2 { "Root Folders" }
            h3 { "Configured classification root folders" }
            (root_folder_list(&view.configured_root_folders, "No classification root folders are configured."))
            h3 { "Sonarr root folders" }
            (root_folder_list(&view.sonarr_root_folders, "Sonarr did not return any root folders."))
        }
        section class="panel" {
            h2 { "Recent Decisions" }
            (recent_decision_summary(&view.operational.recent_decisions))
        }
    }
}

fn status_section(
    title: &str,
    subtitle: Option<&str>,
    footer: Option<Markup>,
    section: &StatusSection,
) -> Markup {
    html! {
        article class="status-block" {
            div class="status-block-header" {
                h3 { (title) }
                span class=(section.level.badge_class()) { (section.level.as_str()) }
            }
            @if let Some(subtitle) = subtitle {
                p class="muted" { (subtitle) }
            }
            p { (&section.summary) }
            @if !section.details.is_empty() {
                ul class="flat-list" {
                    @for detail in &section.details {
                        li { (detail) }
                    }
                }
            }
            @if let Some(error) = &section.error {
                pre class="error-block" { (error) }
            }
            @if let Some(footer) = footer {
                (footer)
            }
        }
    }
}

fn root_folder_list(folders: &[StatusRootFolderView], empty_message: &str) -> Markup {
    if folders.is_empty() {
        return html! { p class="muted" { (empty_message) } };
    }

    html! {
        table {
            thead {
                tr {
                    th { "Path" }
                    th { "Label" }
                    th { "Description" }
                }
            }
            tbody {
                @for folder in folders {
                    tr {
                        td { code { (&folder.path) } }
                        td { (folder.label.as_deref().unwrap_or("-")) }
                        td { (folder.description.as_deref().unwrap_or("-")) }
                    }
                }
            }
        }
    }
}

fn recent_decision_summary(summary: &RecentDecisionSummary) -> Markup {
    html! {
        @if summary.sample_size == 0 {
            p class="muted" { "No decisions have been recorded yet." }
        } @else {
            div class="grid" {
                div { strong { "Sample size" } span { (summary.sample_size) } }
                div { strong { "Latest update" } span { (summary.latest_updated_at.as_deref().unwrap_or("-")) } }
                div { strong { "Completed" } span { (summary.completed) } }
                div { strong { "Failed" } span { (summary.failed) } }
                div { strong { "Skipped low confidence" } span { (summary.skipped_low_confidence) } }
            }
        }
    }
}

fn series_content_region(view: &DecisionView) -> Markup {
    html! {
        div id="series-detail-content" data-decision-id=(view.decision.id) {
            (series_content(view))
        }
    }
}

fn series_content(view: &DecisionView) -> Markup {
    let decision = &view.decision;

    html! {
        h1 { (series_title(decision)) }
        section class="panel grid" {
            div { strong { "Status" } span class=(status_class(&decision.status)) { (decision.status.as_str()) } }
            div { strong { "Chosen root" } span { (decision.selected_root_folder_path.as_deref().unwrap_or("-")) } }
            div { strong { "Confidence" } span { (format_confidence(decision.confidence)) } }
            div { strong { "Sonarr ID" } span { (decision.sonarr_series_id) } }
            div { strong { "Old path" } span { (decision.old_path.as_deref().unwrap_or("-")) } }
            div { strong { "Updated" } span { (&decision.updated_at) } }
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
            @for run in &view.llm_runs {
                (llm_run(run))
            }
        }
        section class="panel" {
            h2 { "Metadata Snapshot" }
            @if let Some(metadata) = &view.metadata_snapshot {
                pre { (metadata) }
            } @else {
                p class="muted" { "No metadata snapshot has been recorded yet." }
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
.top-nav {
  display: flex;
  gap: 12px;
  margin: 0 0 18px;
}
.top-nav a.active {
  font-weight: 600;
}
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
.status-block + .status-block {
  border-top: 1px solid var(--border);
  margin-top: 14px;
  padding-top: 14px;
}
.status-block-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}
.flat-list {
  margin: 8px 0 0;
  padding-left: 18px;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::status::{StatusLevel, StatusOperationalSummary};

    fn decision() -> Decision {
        Decision {
            id: 42,
            instance_name: "sonarr".to_string(),
            sonarr_series_id: 73,
            title: Some("Bluey".to_string()),
            year: Some(2018),
            old_path: Some("/data/tv/Bluey".to_string()),
            selected_root_folder_path: Some("/data/kids".to_string()),
            confidence: Some(0.94),
            reason: Some("Family series".to_string()),
            status: DecisionStatus::Applying,
            error: None,
            created_at: "2026-05-22 12:00:00".to_string(),
            updated_at: "2026-05-22 12:01:00".to_string(),
            applied_at: None,
        }
    }

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

    #[test]
    fn decision_history_row_contains_live_fields() {
        let rendered = history_row(&decision()).into_string();

        assert!(rendered.contains(r#"id="decision-row-42""#));
        assert!(rendered.contains(r#"data-decision-id="42""#));
        assert!(rendered.contains("Bluey (2018)"));
        assert!(rendered.contains("/data/kids"));
        assert!(rendered.contains("94%"));
        assert!(rendered.contains("status status-active"));
        assert!(rendered.contains("2026-05-22 12:01:00"));
    }

    #[test]
    fn decision_history_contains_empty_state_and_live_hooks() {
        let rendered = decision_history(&[]).into_string();

        assert!(rendered.contains(r#"id="decision-history-empty""#));
        assert!(rendered.contains(r#"id="decision-history-table""#));
        assert!(rendered.contains(r#"id="decision-history-rows""#));
        assert!(rendered.contains("hidden"));
        assert!(HISTORY_SCRIPT.contains("new EventSource(\"/events/decisions\")"));
        assert!(HISTORY_SCRIPT.contains("decision-created"));
        assert!(HISTORY_SCRIPT.contains("decision-updated"));
    }

    #[test]
    fn detail_content_contains_current_state_and_live_hooks() {
        let view = DecisionView {
            decision: decision(),
            metadata_snapshot: Some("{ \"title\": \"Bluey\" }".to_string()),
            llm_runs: vec![LlmRun {
                id: 9,
                provider: "test".to_string(),
                model: "model".to_string(),
                prompt_hash: "hash".to_string(),
                raw_response: None,
                parsed_response: None,
                duration_ms: Some(3),
                error: None,
                created_at: "2026-05-22 12:01:00".to_string(),
            }],
        };

        let rendered = series_content_region(&view).into_string();

        assert!(rendered.contains(r#"id="series-detail-content""#));
        assert!(rendered.contains(r#"data-decision-id="42""#));
        assert!(rendered.contains("Chosen root"));
        assert!(rendered.contains("Updated"));
        assert!(rendered.contains("LLM Runs"));
        assert!(rendered.contains("Metadata Snapshot"));
        assert!(DETAIL_SCRIPT.contains("new EventSource(\"/events/decisions\")"));
        assert!(DETAIL_SCRIPT.contains("/content"));
    }

    #[test]
    fn top_nav_links_to_status_page() {
        let rendered = top_nav(Some("status")).into_string();

        assert!(rendered.contains(r#"href="/status""#));
        assert!(rendered.contains("Decision History"));
        assert!(rendered.contains("Status"));
    }

    #[test]
    fn top_nav_does_not_highlight_history_on_series_detail() {
        let rendered = top_nav(Some("series_detail")).into_string();

        assert!(!rendered.contains(r#"href="/" class="active""#));
        assert!(!rendered.contains(r#"href="/status" class="active""#));
    }

    #[test]
    fn status_content_renders_sections_and_empty_states() {
        let view = StatusPageView {
            checked_at: "2026-06-04 12:00:00 UTC".to_string(),
            sonarr_base_url: "http://sonarr:8989".to_string(),
            sonarr: StatusSection {
                level: StatusLevel::Ok,
                summary: "Sonarr ok".to_string(),
                details: vec!["1 root folder returned".to_string()],
                error: None,
            },
            llm_provider: "ollama".to_string(),
            llm_base_url: "http://ollama:11434".to_string(),
            llm_model: "qwen3:0.6b".to_string(),
            llm: StatusSection {
                level: StatusLevel::Warn,
                summary: "Model missing".to_string(),
                details: vec!["provider: ollama".to_string()],
                error: None,
            },
            tmdb: StatusSection {
                level: StatusLevel::NotConfigured,
                summary: "TMDB is not configured".to_string(),
                details: Vec::new(),
                error: None,
            },
            tvdb: StatusSection {
                level: StatusLevel::Error,
                summary: "TVDB probe failed".to_string(),
                details: Vec::new(),
                error: Some("auth failed".to_string()),
            },
            configured_root_folders: vec![StatusRootFolderView {
                path: "/tv/scripted".to_string(),
                label: Some("Scripted".to_string()),
                description: Some("General TV".to_string()),
            }],
            sonarr_root_folders: Vec::new(),
            operational: StatusOperationalSummary {
                version: "0.1.4".to_string(),
                webhook_auth_configured: true,
                recent_decisions: RecentDecisionSummary {
                    sample_size: 0,
                    latest_updated_at: None,
                    completed: 0,
                    failed: 0,
                    skipped_low_confidence: 0,
                },
            },
        };

        let rendered = status_content(&view).into_string();
        let llm_start = rendered.find("<h3>LLM</h3>").expect("LLM section missing");
        let configured_model = rendered
            .find("Configured model")
            .expect("configured model note missing");
        let tmdb_start = rendered
            .find("<h3>TMDB</h3>")
            .expect("TMDB section missing");

        assert!(rendered.contains("System Status"));
        assert!(rendered.contains("Sonarr ok"));
        assert!(rendered.contains("Configured model"));
        assert!(rendered.contains("TMDB is not configured"));
        assert!(rendered.contains("TVDB probe failed"));
        assert!(rendered.contains("No decisions have been recorded yet."));
        assert!(rendered.contains("Configured classification root folders"));
        assert!(rendered.contains("Sonarr did not return any root folders."));
        assert!(llm_start < configured_model);
        assert!(configured_model < tmdb_start);
    }
}
