use uuid::Uuid;

use crate::db;
use crate::db::pool::AppState;
use crate::models::connection::ConnectionConfig;
use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};
use crate::security::secrets::SecretString;

/// 測試連線。成功後驅動實例（含池）以 ConnectionId 快取於 AppState 供後續重用。
#[tauri::command]
pub async fn test_connection(
    state: tauri::State<'_, AppState>,
    config: ConnectionConfig,
    password: SecretString,
) -> Result<(), EluEtlError> {
    let driver = db::create_driver(&config, &password);
    driver.test_connection().await?;
    state.insert_driver(config.id, driver).await;
    tracing::info!(
        target: "audit",
        conn_id = %config.id,
        kind = ?config.kind,
        "連線測試成功"
    );
    Ok(())
}

#[tauri::command]
pub async fn get_tables(
    state: tauri::State<'_, AppState>,
    conn_id: Uuid,
) -> Result<Vec<TableInfo>, EluEtlError> {
    state.driver(&conn_id).await?.list_tables().await
}

#[tauri::command]
pub async fn get_columns(
    state: tauri::State<'_, AppState>,
    conn_id: Uuid,
    table: String,
) -> Result<Vec<ColumnInfo>, EluEtlError> {
    state.driver(&conn_id).await?.get_columns(&table).await
}
