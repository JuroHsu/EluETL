use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::db::driver::DbDriver;
use crate::models::connection::ConnectionConfig;
use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};

/// SQLite 驅動（sqlx）。`ConnectionConfig.database` 為檔案路徑。
pub struct SqliteDriver {
    options: SqliteConnectOptions,
}

impl SqliteDriver {
    pub fn new(config: &ConnectionConfig) -> Self {
        let options = SqliteConnectOptions::new()
            .filename(&config.database)
            // 測試連線不應憑空建檔；建檔行為留給匯入流程明確處理
            .create_if_missing(false);
        Self { options }
    }
}

#[async_trait::async_trait]
impl DbDriver for SqliteDriver {
    async fn test_connection(&self) -> Result<(), EluEtlError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(self.options.clone())
            .await?;
        sqlx::query("SELECT 1").execute(&pool).await?;
        pool.close().await;
        Ok(())
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError> {
        Err(EluEtlError::NotImplemented("sqlite::list_tables"))
    }

    async fn get_columns(&self, _table: &str) -> Result<Vec<ColumnInfo>, EluEtlError> {
        Err(EluEtlError::NotImplemented("sqlite::get_columns"))
    }
}
