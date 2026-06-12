use serde::Serialize;
use uuid::Uuid;

use crate::db;
use crate::db::pool::AppState;
use crate::excel::writer;
use crate::models::connection::{ConnectionConfig, DbKind};
use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};
use crate::security::{keychain, secrets::SecretString};

/// 測試連線（不儲存）。成功後驅動實例（含池）以 ConnectionId 快取供後續重用。
/// 檔案連線測試 = 檔案存在且可解析（列得出工作表）。
#[tauri::command]
pub async fn test_connection(
    state: tauri::State<'_, AppState>,
    config: ConnectionConfig,
    password: SecretString,
) -> Result<(), EluEtlError> {
    if config.kind == DbKind::File {
        let path = config.database.clone();
        tokio::task::spawn_blocking(move || crate::excel::source::list_sheets(&path)).await??;
    } else {
        let driver = db::create_driver(&config, &password)?;
        driver.test_connection().await?;
        state.insert_driver(config.id, driver).await;
    }
    tracing::info!(
        target: "audit",
        conn_id = %config.id,
        kind = ?config.kind,
        "連線測試成功"
    );
    Ok(())
}

/// 儲存連線：設定（不含密碼）進 state.db；密碼（若提供）進 OS keychain。
#[tauri::command]
pub async fn save_connection(
    state: tauri::State<'_, AppState>,
    config: ConnectionConfig,
    password: Option<SecretString>,
) -> Result<(), EluEtlError> {
    state.store()?.upsert_connection(&config).await?;
    if let Some(pw) = password {
        if matches!(
            config.kind,
            DbKind::SqlServer | DbKind::Postgres | DbKind::MySql
        ) {
            let id = config.id;
            tokio::task::spawn_blocking(move || keychain::save_password(&id, &pw)).await??;
        }
    }
    // 設定可能變更，作廢快取的驅動（下次使用時以新設定重建）
    state.evict_driver(&config.id).await;
    tracing::info!(target: "audit", conn_id = %config.id, name = %config.name, "連線已儲存");
    Ok(())
}

#[tauri::command]
pub async fn list_connections(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ConnectionConfig>, EluEtlError> {
    state.store()?.list_connections().await
}

#[tauri::command]
pub async fn delete_connection(
    state: tauri::State<'_, AppState>,
    conn_id: Uuid,
) -> Result<(), EluEtlError> {
    state.store()?.delete_connection(&conn_id).await?;
    tokio::task::spawn_blocking(move || keychain::delete_password(&conn_id)).await??;
    state.evict_driver(&conn_id).await;
    tracing::info!(target: "audit", conn_id = %conn_id, "連線已刪除");
    Ok(())
}

/// 驗證使用中連線是否可用（狀態列指示燈）。
#[tauri::command]
pub async fn ping_connection(
    state: tauri::State<'_, AppState>,
    conn_id: Uuid,
) -> Result<(), EluEtlError> {
    let config = state.store()?.get_connection(&conn_id).await?;
    if config.kind == DbKind::File {
        let path = config.database;
        tokio::task::spawn_blocking(move || crate::excel::source::list_sheets(&path)).await??;
        return Ok(());
    }
    state
        .get_or_create_driver(conn_id)
        .await?
        .test_connection()
        .await
}

#[tauri::command]
pub async fn get_tables(
    state: tauri::State<'_, AppState>,
    conn_id: Uuid,
) -> Result<Vec<TableInfo>, EluEtlError> {
    state
        .get_or_create_driver(conn_id)
        .await?
        .list_tables()
        .await
}

#[tauri::command]
pub async fn get_columns(
    state: tauri::State<'_, AppState>,
    conn_id: Uuid,
    table: String,
) -> Result<Vec<ColumnInfo>, EluEtlError> {
    state
        .get_or_create_driver(conn_id)
        .await?
        .get_columns(&table)
        .await
}

/// 查詢預覽（最多 100 行，JSON 序列化跨 IPC）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPreview {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
}

#[tauri::command]
pub async fn query_preview(
    state: tauri::State<'_, AppState>,
    conn_id: Uuid,
    sql: String,
) -> Result<QueryPreview, EluEtlError> {
    let result = state
        .get_or_create_driver(conn_id)
        .await?
        .query_all(&sql, Some(100))
        .await?;
    Ok(QueryPreview {
        columns: result.columns,
        rows: result
            .rows
            .iter()
            .map(|r| r.iter().map(|c| c.to_json()).collect())
            .collect(),
    })
}

/// 查詢結果匯出 xlsx：資料全程在 Rust 端流動，不跨 IPC。回傳列數。
#[tauri::command]
pub async fn export_query_to_excel(
    state: tauri::State<'_, AppState>,
    conn_id: Uuid,
    sql: String,
    output_path: String,
) -> Result<u64, EluEtlError> {
    let result = state
        .get_or_create_driver(conn_id)
        .await?
        .query_all(&sql, None)
        .await?;
    let count = tokio::task::spawn_blocking(move || {
        writer::write_xlsx(&output_path, &result.columns, &result.rows)
    })
    .await??;
    tracing::info!(target: "audit", conn_id = %conn_id, rows = count, "查詢結果已匯出 xlsx");
    Ok(count)
}
