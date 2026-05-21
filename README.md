# Rooterr

Rooterr is a small Rust companion service for Sonarr. It listens for Sonarr `SeriesAdd` webhooks, enriches the series metadata, asks a local LLM to choose the best existing Sonarr root folder, applies the new path with `moveFiles=true`, and stores the decision and reason in SQLite.

## Quick Start

1. Copy the example config:

   ```bash
   cp rooterr.toml.example rooterr.toml
   ```

2. Edit `rooterr.toml` with your Sonarr URL, Sonarr API key, local LLM endpoint, and optional TMDB/TVDB credentials.

3. Run locally:

   ```bash
   cargo run
   ```

4. Open the UI at `http://localhost:9898`.

## Sonarr Webhook

In Sonarr, add a Webhook connection for series-added events and point it to:

```text
http://rooterr-host:9898/webhooks/sonarr?token=your-rooterr-token
```

If `sonarr.webhook_token` is omitted, Rooterr accepts the webhook without a token. If it is set, Rooterr accepts the token as `?token=...`, `X-Rooterr-Token`, or `Authorization: Bearer ...`.

## Local LLMs

Rooterr supports:

- Ollama: `POST /api/chat`
- OpenAI-compatible servers: `POST /v1/chat/completions`

The model is required to return JSON with:

```json
{
  "root_folder_path": "/data/kids",
  "confidence": 0.91,
  "reason": "Animated children's series with kids genre metadata.",
  "signals": ["Animation", "Children"]
}
```

The selected `root_folder_path` must exactly match a root folder returned by Sonarr.

## Architecture

Rooterr is a one-crate Rust application using a lightweight hexagonal architecture with screaming names:

```text
src/
  domain/      Rooterr-owned concepts such as decisions, series, metadata, and root folders
  ports/       Traits for repository, Sonarr, metadata, and classifier boundaries
  use_cases/   Product workflows such as accepting a series add, processing a decision, and retrying
  adapters/    Axum web, SQLite, Sonarr HTTP, metadata APIs, and local LLM implementations
```

`bootstrap.rs` wires concrete adapters into `Arc<dyn Trait>` ports. The web adapter stays thin: it parses HTTP input, calls use cases, spawns background processing, and renders server-side HTML.

## Docker Compose

```bash
cp rooterr.toml.example rooterr.toml
docker compose -f docker-compose.example.yml up --build
```

Persist the SQLite database by setting `database.sqlite_path` to a path under a mounted volume.
