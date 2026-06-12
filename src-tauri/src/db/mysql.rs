use std::time::Duration;

use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};

use crate::db::driver::DbDriver;
use crate::models::connection::ConnectionConfig;
use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};
use crate::security::secrets::SecretString;

/// MySQL 驅動（sqlx）。
pub struct MySqlDriver {
    options: MySqlConnectOptions,
}

impl MySqlDriver {
    pub fn new(config: &ConnectionConfig, password: &SecretString) -> Self {
        let options = MySqlConnectOptions::new()
            .host(&config.host)
            .port(config.port_or_default())
            .username(&config.username)
            .password(password.expose())
            .database(&config.database);
        Self { options }
    }
}

#[async_trait::async_trait]
impl DbDriver for MySqlDriver {
    async fn test_connection(&self) -> Result<(), EluEtlError> {
        let pool = MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(self.options.clone())
            .await?;
        sqlx::query("SELECT 1").execute(&pool).await?;
        pool.close().await;
        Ok(())
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError> {
        Err(EluEtlError::NotImplemented("mysql::list_tables"))
    }

    async fn get_columns(&self, _table: &str) -> Result<Vec<ColumnInfo>, EluEtlError> {
        Err(EluEtlError::NotImplemented("mysql::get_columns"))
    }
}
