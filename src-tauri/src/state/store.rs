use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Row;
use uuid::Uuid;

use crate::etl::mapping::EtlJobConfig;
use crate::models::connection::ConnectionConfig;
use crate::models::errors::EluEtlError;

/// 本地狀態庫（`{appDataDir}/state.db`）：連線設定（不含密碼）、
/// 任務歷史與 checkpoint。密碼一律在 OS keychain（security::keychain）。
pub struct StateStore {
    pool: SqlitePool,
}

/// 任務紀錄（續跑用）。
pub struct JobRow {
    pub config: EtlJobConfig,
    pub source_sha256: String,
    pub status: String,
    pub last_batch: i64,
}

impl StateStore {
    pub async fn init(dir: &Path) -> Result<Self, EluEtlError> {
        std::fs::create_dir_all(dir)?;
        let options = SqliteConnectOptions::new()
            .filename(dir.join("state.db"))
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS connections (
                id TEXT PRIMARY KEY,
                config_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY,
                config_json TEXT NOT NULL,
                source_sha256 TEXT NOT NULL,
                status TEXT NOT NULL,
                last_batch INTEGER NOT NULL DEFAULT 0,
                success_rows INTEGER NOT NULL DEFAULT 0,
                error_rows INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await?;

        Ok(Self { pool })
    }

    // ---- 連線設定 ----

    pub async fn upsert_connection(&self, config: &ConnectionConfig) -> Result<(), EluEtlError> {
        sqlx::query(
            "INSERT INTO connections (id, config_json) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET config_json = excluded.config_json",
        )
        .bind(config.id.to_string())
        .bind(serde_json::to_string(config)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_connections(&self) -> Result<Vec<ConnectionConfig>, EluEtlError> {
        let rows = sqlx::query("SELECT config_json FROM connections ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| Ok(serde_json::from_str(&r.get::<String, _>(0))?))
            .collect()
    }

    /// 以名稱查找連線（.etl 腳本的 CONNECTION('名稱') 引用；不分大小寫、忽略前後空白）。
    pub async fn get_connection_by_name(
        &self,
        name: &str,
    ) -> Result<ConnectionConfig, EluEtlError> {
        let target = name.trim();
        self.list_connections()
            .await?
            .into_iter()
            .find(|c| c.name.trim().eq_ignore_ascii_case(target) || c.name.trim() == target)
            .ok_or_else(|| {
                EluEtlError::NotFound(format!(
                    "找不到名為「{name}」的已儲存連線（請先在「資料庫連線」建立並儲存）"
                ))
            })
    }

    pub async fn get_connection(&self, id: &Uuid) -> Result<ConnectionConfig, EluEtlError> {
        let row = sqlx::query("SELECT config_json FROM connections WHERE id = ?1")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| EluEtlError::NotFound(format!("連線 {id} 不存在")))?;
        Ok(serde_json::from_str(&row.get::<String, _>(0))?)
    }

    pub async fn delete_connection(&self, id: &Uuid) -> Result<(), EluEtlError> {
        sqlx::query("DELETE FROM connections WHERE id = ?1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- 任務 / checkpoint ----

    pub async fn upsert_job(
        &self,
        config: &EtlJobConfig,
        source_sha256: &str,
        status: &str,
    ) -> Result<(), EluEtlError> {
        sqlx::query(
            "INSERT INTO jobs (id, config_json, source_sha256, status)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
               config_json = excluded.config_json,
               source_sha256 = excluded.source_sha256,
               status = excluded.status",
        )
        .bind(config.job_id.to_string())
        .bind(serde_json::to_string(config)?)
        .bind(source_sha256)
        .bind(status)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// 每批 commit 後更新 checkpoint（先 DB commit、後記 checkpoint，
    /// 極端崩潰下最多重複一批 — 見開發計畫 §4.4）。
    pub async fn update_job_progress(
        &self,
        job_id: &Uuid,
        last_batch: i64,
        success_rows: i64,
        error_rows: i64,
    ) -> Result<(), EluEtlError> {
        sqlx::query(
            "UPDATE jobs SET last_batch = ?2, success_rows = ?3, error_rows = ?4 WHERE id = ?1",
        )
        .bind(job_id.to_string())
        .bind(last_batch)
        .bind(success_rows)
        .bind(error_rows)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_job_status(&self, job_id: &Uuid, status: &str) -> Result<(), EluEtlError> {
        sqlx::query("UPDATE jobs SET status = ?2 WHERE id = ?1")
            .bind(job_id.to_string())
            .bind(status)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_job(&self, job_id: &Uuid) -> Result<JobRow, EluEtlError> {
        let row = sqlx::query(
            "SELECT config_json, source_sha256, status, last_batch FROM jobs WHERE id = ?1",
        )
        .bind(job_id.to_string())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| EluEtlError::NotFound(format!("任務 {job_id} 不存在")))?;
        Ok(JobRow {
            config: serde_json::from_str(&row.get::<String, _>(0))?,
            source_sha256: row.get::<String, _>(1),
            status: row.get::<String, _>(2),
            last_batch: row.get::<i64, _>(3),
        })
    }
}
