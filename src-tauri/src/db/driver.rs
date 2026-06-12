use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};
use crate::models::value::{CellValue, DataType};

/// 查詢結果（欄名 + 中介值資料列）。
#[derive(Debug, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<CellValue>>,
}

/// 統一資料庫抽象：上層（ETL 引擎、IPC commands）僅依賴此 trait，
/// 不感知底層是 sqlx（PG / MySQL / SQLite）或 tiberius（SQL Server）。
#[async_trait::async_trait]
pub trait DbDriver: Send + Sync {
    /// 建立連線並執行最小查詢驗證可用性。
    async fn test_connection(&self) -> Result<(), EluEtlError>;

    /// 列出使用者可見的資料表。
    async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError>;

    /// 取得指定資料表的欄位定義（支援 `schema.table`）。
    async fn get_columns(&self, table: &str) -> Result<Vec<ColumnInfo>, EluEtlError>;

    /// 執行查詢並取回資料列；`max_rows` 限制筆數（預覽用 100，匯出傳 None）。
    ///
    /// 注意：目前為一次性物化（匯出超大結果集的串流化列於 backlog）。
    async fn query_all(
        &self,
        sql: &str,
        max_rows: Option<usize>,
    ) -> Result<QueryResult, EluEtlError>;

    /// 以單一交易寫入一批資料列（批次提交模式由 executor 以多次呼叫實現；
    /// 全有全無模式以單次呼叫整批實現）。回傳寫入行數。
    ///
    /// `types[i]` 為 `columns[i]` 的目標型別，用於 NULL 的型別化綁定。
    async fn write_batch(
        &self,
        table: &str,
        columns: &[String],
        types: &[DataType],
        rows: &[Vec<CellValue>],
    ) -> Result<u64, EluEtlError>;
}
