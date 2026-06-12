use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::db::driver::DbDriver;
use crate::models::connection::ConnectionConfig;
use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};
use crate::security::secrets::SecretString;

/// PostgreSQL 驅動（sqlx）。
/// 以 ConnectOptions 組態而非 URL 字串，避免密碼特殊字元的轉義問題。
pub struct PostgresDriver {
    options: PgConnectOptions,
}

impl PostgresDriver {
    pub fn new(config: &ConnectionConfig, password: &SecretString) -> Self {
        let options = PgConnectOptions::new()
            .host(&config.host)
            .port(config.port_or_default())
            .username(&config.username)
            .password(password.expose())
            .database(&config.database);
        Self { options }
    }
}

#[async_trait::async_trait]
impl DbDriver for PostgresDriver {
    async fn test_connection(&self) -> Result<(), EluEtlError> {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(self.options.clone())
            .await?;
        sqlx::query("SELECT 1").execute(&pool).await?;
        pool.close().await;
        Ok(())
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError> {
        Err(EluEtlError::NotImplemented("postgres::list_tables"))
    }

    async fn get_columns(&self, _table: &str) -> Result<Vec<ColumnInfo>, EluEtlError> {
        Err(EluEtlError::NotImplemented("postgres::get_columns"))
    }
}
