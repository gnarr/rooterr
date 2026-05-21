use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{OptionalExtension, params};

use crate::{
    domain::{
        classification::Classification,
        decision::{Decision, DecisionStatus, InsertDecisionResult, LlmRun, NewLlmRun},
        metadata::MetadataBundle,
        series::{SeriesAdded, SeriesDetails},
    },
    ports::decision_repository::DecisionRepository,
};

#[derive(Clone)]
pub struct SqliteDecisionRepository {
    pool: Pool<SqliteConnectionManager>,
}

impl SqliteDecisionRepository {
    pub fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create database directory {}", parent.display())
            })?;
        }

        let manager = SqliteConnectionManager::file(path);
        let pool = Pool::new(manager).context("failed to create sqlite connection pool")?;
        let repository = Self { pool };
        repository.migrate()?;
        Ok(repository)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn()?;
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS decisions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_name TEXT NOT NULL,
                sonarr_series_id INTEGER NOT NULL,
                title TEXT,
                year INTEGER,
                old_path TEXT,
                selected_root_folder_path TEXT,
                confidence REAL,
                reason TEXT,
                status TEXT NOT NULL,
                error TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                applied_at TEXT,
                UNIQUE(instance_name, sonarr_series_id)
            );

            CREATE TABLE IF NOT EXISTS metadata_snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                decision_id INTEGER NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
                metadata_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS llm_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                decision_id INTEGER NOT NULL REFERENCES decisions(id) ON DELETE CASCADE,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_hash TEXT NOT NULL,
                raw_response TEXT,
                parsed_response TEXT,
                duration_ms INTEGER,
                error TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS idx_decisions_updated_at ON decisions(updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_snapshots_decision ON metadata_snapshots(decision_id, id DESC);
            CREATE INDEX IF NOT EXISTS idx_llm_runs_decision ON llm_runs(decision_id, id DESC);
            "#,
        )
        .context("failed to run sqlite migrations")?;

        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version) VALUES (1)",
            [],
        )
        .context("failed to record migration version")?;
        Ok(())
    }

    fn conn(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>> {
        self.pool.get().context("failed to get sqlite connection")
    }
}

#[async_trait]
impl DecisionRepository for SqliteDecisionRepository {
    async fn insert_decision_if_absent(
        &self,
        series: &SeriesAdded,
    ) -> Result<InsertDecisionResult> {
        let conn = self.conn()?;
        conn.execute(
            r#"
            INSERT OR IGNORE INTO decisions
                (instance_name, sonarr_series_id, title, year, old_path, status)
            VALUES
                (?1, ?2, ?3, ?4, ?5, 'received')
            "#,
            params![
                series.instance_name,
                series.sonarr_series_id,
                series.title,
                series.year,
                series.path
            ],
        )
        .context("failed to insert decision")?;

        let created = conn.changes() == 1;
        let decision_id = if created {
            conn.last_insert_rowid()
        } else {
            conn.query_row(
                "SELECT id FROM decisions WHERE instance_name = ?1 AND sonarr_series_id = ?2",
                params![series.instance_name, series.sonarr_series_id],
                |row| row.get(0),
            )
            .context("failed to find existing decision")?
        };

        Ok(InsertDecisionResult {
            decision_id,
            created,
        })
    }

    async fn decision(&self, id: i64) -> Result<Option<Decision>> {
        let conn = self.conn()?;
        conn.query_row(
            r#"
            SELECT id, instance_name, sonarr_series_id, title, year, old_path,
                   selected_root_folder_path, confidence, reason, status, error,
                   created_at, updated_at, applied_at
            FROM decisions
            WHERE id = ?1
            "#,
            params![id],
            map_decision,
        )
        .optional()
        .context("failed to load decision")
    }

    async fn list_decisions(&self, limit: i64) -> Result<Vec<Decision>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, instance_name, sonarr_series_id, title, year, old_path,
                       selected_root_folder_path, confidence, reason, status, error,
                       created_at, updated_at, applied_at
                FROM decisions
                ORDER BY updated_at DESC, id DESC
                LIMIT ?1
                "#,
            )
            .context("failed to prepare decision list")?;

        let rows = stmt
            .query_map(params![limit], map_decision)
            .context("failed to list decisions")?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to map decisions")
    }

    async fn update_decision_basics(&self, id: i64, series: &SeriesDetails) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            r#"
            UPDATE decisions
            SET title = COALESCE(?2, title),
                year = COALESCE(?3, year),
                old_path = COALESCE(?4, old_path),
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?1
            "#,
            params![id, series.title(), series.year(), series.path()],
        )
        .context("failed to update decision basics")?;
        Ok(())
    }

    async fn mark_status(&self, id: i64, status: DecisionStatus) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            r#"
            UPDATE decisions
            SET status = ?2, error = NULL, updated_at = CURRENT_TIMESTAMP
            WHERE id = ?1
            "#,
            params![id, status.as_str()],
        )
        .context("failed to update decision status")?;
        Ok(())
    }

    async fn mark_failed(&self, id: i64, error: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            r#"
            UPDATE decisions
            SET status = 'failed', error = ?2, updated_at = CURRENT_TIMESTAMP
            WHERE id = ?1
            "#,
            params![id, error],
        )
        .context("failed to mark decision failed")?;
        Ok(())
    }

    async fn mark_applying(&self, id: i64, classification: &Classification) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            r#"
            UPDATE decisions
            SET status = 'applying',
                selected_root_folder_path = ?2,
                confidence = ?3,
                reason = ?4,
                error = NULL,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?1
            "#,
            params![
                id,
                classification.root_folder_path,
                classification.confidence,
                classification.reason
            ],
        )
        .context("failed to mark decision applying")?;
        Ok(())
    }

    async fn mark_completed(&self, id: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            r#"
            UPDATE decisions
            SET status = 'completed',
                error = NULL,
                applied_at = CURRENT_TIMESTAMP,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?1
            "#,
            params![id],
        )
        .context("failed to mark decision completed")?;
        Ok(())
    }

    async fn mark_skipped_low_confidence(
        &self,
        id: i64,
        classification: &Classification,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            r#"
            UPDATE decisions
            SET status = 'skipped_low_confidence',
                selected_root_folder_path = ?2,
                confidence = ?3,
                reason = ?4,
                error = NULL,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?1
            "#,
            params![
                id,
                classification.root_folder_path,
                classification.confidence,
                classification.reason
            ],
        )
        .context("failed to mark decision skipped")?;
        Ok(())
    }

    async fn insert_metadata_snapshot(
        &self,
        decision_id: i64,
        metadata: &MetadataBundle,
    ) -> Result<()> {
        let metadata_json =
            serde_json::to_string_pretty(metadata).context("failed to encode metadata snapshot")?;
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO metadata_snapshots(decision_id, metadata_json) VALUES (?1, ?2)",
            params![decision_id, metadata_json],
        )
        .context("failed to insert metadata snapshot")?;
        Ok(())
    }

    async fn latest_metadata_snapshot(&self, decision_id: i64) -> Result<Option<String>> {
        let conn = self.conn()?;
        conn.query_row(
            r#"
            SELECT metadata_json
            FROM metadata_snapshots
            WHERE decision_id = ?1
            ORDER BY id DESC
            LIMIT 1
            "#,
            params![decision_id],
            |row| row.get(0),
        )
        .optional()
        .context("failed to load latest metadata snapshot")
    }

    async fn insert_llm_run(&self, run: NewLlmRun) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            r#"
            INSERT INTO llm_runs
                (decision_id, provider, model, prompt_hash, raw_response, parsed_response, duration_ms, error)
            VALUES
                (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                run.decision_id,
                run.provider,
                run.model,
                run.prompt_hash,
                run.raw_response,
                run.parsed_response,
                run.duration_ms,
                run.error
            ],
        )
        .context("failed to insert llm run")?;
        Ok(())
    }

    async fn llm_runs(&self, decision_id: i64) -> Result<Vec<LlmRun>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT id, provider, model, prompt_hash, raw_response, parsed_response,
                       duration_ms, error, created_at
                FROM llm_runs
                WHERE decision_id = ?1
                ORDER BY id DESC
                "#,
            )
            .context("failed to prepare llm run list")?;

        let rows = stmt
            .query_map(params![decision_id], |row| {
                Ok(LlmRun {
                    id: row.get(0)?,
                    provider: row.get(1)?,
                    model: row.get(2)?,
                    prompt_hash: row.get(3)?,
                    raw_response: row.get(4)?,
                    parsed_response: row.get(5)?,
                    duration_ms: row.get(6)?,
                    error: row.get(7)?,
                    created_at: row.get(8)?,
                })
            })
            .context("failed to list llm runs")?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to map llm runs")
    }
}

fn map_decision(row: &rusqlite::Row<'_>) -> rusqlite::Result<Decision> {
    let status: String = row.get(9)?;
    Ok(Decision {
        id: row.get(0)?,
        instance_name: row.get(1)?,
        sonarr_series_id: row.get(2)?,
        title: row.get(3)?,
        year: row.get(4)?,
        old_path: row.get(5)?,
        selected_root_folder_path: row.get(6)?,
        confidence: row.get(7)?,
        reason: row.get(8)?,
        status: status.into(),
        error: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        applied_at: row.get(13)?,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn repository() -> (TempDir, SqliteDecisionRepository) {
        let temp = TempDir::new().expect("temp dir");
        let repo =
            SqliteDecisionRepository::new(&temp.path().join("rooterr.sqlite3")).expect("repo");
        (temp, repo)
    }

    fn series_added() -> SeriesAdded {
        SeriesAdded {
            instance_name: "sonarr".to_string(),
            sonarr_series_id: 42,
            title: Some("Bluey".to_string()),
            year: Some(2018),
            path: Some("/data/tv/Bluey (2018)".to_string()),
        }
    }

    #[tokio::test]
    async fn duplicate_insert_is_idempotent() {
        let (_temp, repo) = repository();

        let first = repo
            .insert_decision_if_absent(&series_added())
            .await
            .expect("first insert");
        let second = repo
            .insert_decision_if_absent(&series_added())
            .await
            .expect("second insert");

        assert!(first.created);
        assert!(!second.created);
        assert_eq!(first.decision_id, second.decision_id);
    }

    #[tokio::test]
    async fn status_transitions_snapshots_and_llm_runs_are_persisted() {
        let (_temp, repo) = repository();
        let inserted = repo
            .insert_decision_if_absent(&series_added())
            .await
            .expect("insert");
        repo.mark_status(inserted.decision_id, DecisionStatus::Processing)
            .await
            .expect("processing");

        let metadata = MetadataBundle {
            sonarr: serde_json::json!({ "title": "Bluey" }),
            tmdb: None,
            tmdb_error: None,
            tvdb: None,
            tvdb_error: None,
        };
        repo.insert_metadata_snapshot(inserted.decision_id, &metadata)
            .await
            .expect("snapshot");
        repo.insert_llm_run(NewLlmRun {
            decision_id: inserted.decision_id,
            provider: "test".to_string(),
            model: "model".to_string(),
            prompt_hash: "hash".to_string(),
            raw_response: Some("raw".to_string()),
            parsed_response: Some("parsed".to_string()),
            duration_ms: Some(12),
            error: None,
        })
        .await
        .expect("llm run");

        let decision = repo
            .decision(inserted.decision_id)
            .await
            .expect("load")
            .expect("row");
        assert_eq!(decision.status, DecisionStatus::Processing);
        assert!(
            repo.latest_metadata_snapshot(inserted.decision_id)
                .await
                .expect("snapshot")
                .is_some()
        );
        assert_eq!(
            repo.llm_runs(inserted.decision_id)
                .await
                .expect("runs")
                .len(),
            1
        );
    }
}
