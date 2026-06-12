use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};

/// 統一資料庫抽象：上層（ETL 引擎、IPC commands）僅依賴此 trait，
/// 不感知底層是 sqlx（PG / MySQL / SQLite）或 tiberius（SQL Server）。
///
/// Week 4 將擴充 `query_stream`（串流查詢，供匯出）與
/// `bulk_insert`（各 DB 最佳化批次寫入），見開發計畫 §2.2.1。
#[async_trait::async_trait]
pub trait DbDriver: Send + Sync {
    /// 建立連線並執行最小查詢驗證可用性。
    async fn test_connection(&self) -> Result<(), EluEtlError>;

    /// 列出使用者可見的資料表。
    async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError>;

    /// 取得指定資料表的欄位定義。
    async fn get_columns(&self, table: &str) -> Result<Vec<ColumnInfo>, EluEtlError>;
}
