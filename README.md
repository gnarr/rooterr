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

Rooterr creates the SQLite database automatically on startup when it is missing. By default it stores the database at `./data/rooterr.sqlite3`; the parent `data/` directory is created automatically.

## Sonarr Webhook

In Sonarr, create a dedicated Webhook connection for Rooterr:

1. Go to `Settings -> Connect -> + -> Webhook`.
2. Set `Name` to `Rooterr`.
3. Under `Notification Triggers`, enable only `On Series Add` / `Series Add`.
4. Leave unrelated triggers disabled, including grab, download/import, upgrade, rename, delete, health, application update, and manual interaction.
5. Set `Webhook URL`:

   ```text
   http://rooterr:9898/webhooks/sonarr
   ```

   Use that URL when Sonarr and Rooterr run in the same Docker Compose network. For a host or LAN install, use:

   ```text
   http://<rooterr-host>:9898/webhooks/sonarr
   ```

6. Set `Method` to `POST`.
7. Leave `Username` blank.
8. Leave `Password` blank.
9. Add this header when `sonarr.webhook_token` is set in `rooterr.toml`:

   ```text
   X-Rooterr-Token: <sonarr.webhook_token>
   ```

`X-Rooterr-Token` is the recommended authentication method because it keeps the secret out of URLs, logs, browser history, and proxy request lines. Rooterr also accepts this alternative header:

```text
Authorization: Bearer <sonarr.webhook_token>
```

Use a query-string token only if Sonarr cannot send custom headers:

```text
http://<rooterr-host>:9898/webhooks/sonarr?token=<sonarr.webhook_token>
```

Do not use Sonarr's `Username` and `Password` fields for Rooterr; those configure HTTP Basic authentication, which Rooterr does not use for webhooks. Sonarr already sends JSON, so no custom `Content-Type` header is needed.

If `sonarr.webhook_token` is omitted, Rooterr accepts the webhook without authentication. Only do this on a trusted private network. Sonarr's test button may send a `Test` event; Rooterr accepts authenticated requests but ignores anything that is not `SeriesAdd`.

## Local LLMs

Rooterr supports:

- Ollama: `POST /api/chat`
- OpenAI-compatible servers: `POST /v1/chat/completions`

For Ollama, Rooterr can automatically pull the configured model before the web server starts:

```toml
[llm]
provider = "ollama"
base_url = "http://ollama:11434"
model = "gemma3:270m-it-qat"
auto_pull = true
startup_wait_seconds = 60
pull_timeout_seconds = 900
auto_num_ctx = true
min_num_ctx = 4096
max_num_ctx = 0
reserved_output_tokens = 512
```

The first startup can take several minutes and requires the Ollama container to have internet access. `startup_wait_seconds` lets Rooterr wait for the Ollama service before checking local models, and `pull_timeout_seconds` controls the download timeout. The model is stored in the Ollama volume, so later restarts should skip the download. If you prefer to manage models manually, leave `auto_pull = false` and pull the model yourself:

```bash
docker exec ollama ollama pull gemma3:270m-it-qat
```

When `auto_num_ctx = true`, Rooterr estimates the final classification prompt size and sends an Ollama-only `options.num_ctx` value rounded up to a stable context bucket. `max_num_ctx = 0` lets Rooterr detect the model limit from Ollama; set a positive value to override it.

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

The example Compose file mounts `./data` to `/app/data`, matching the default `database.sqlite_path = "./data/rooterr.sqlite3"` from `rooterr.toml.example`.
