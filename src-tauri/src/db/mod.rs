pub mod driver;
pub mod pool;

mod mssql;
mod mysql;
mod postgres;
mod sqlite;

use std::sync::Arc;

use crate::models::connection::{ConnectionConfig, DbKind};
use crate::security::secrets::SecretString;
use driver::DbDriver;

/// 驅動工廠：依資料庫種類建立對應驅動（tiberius 或 sqlx）。
pub fn create_driver(config: &ConnectionConfig, password: &SecretString) -> Arc<dyn DbDriver> {
    match config.kind {
        DbKind::SqlServer => Arc::new(mssql::MssqlDriver::new(config.clone(), password.clone())),
        DbKind::Postgres => Arc::new(postgres::PostgresDriver::new(config, password)),
        DbKind::MySql => Arc::new(mysql::MySqlDriver::new(config, password)),
        DbKind::Sqlite => Arc::new(sqlite::SqliteDriver::new(config)),
    }
}
