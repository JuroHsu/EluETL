use serde::{Deserialize, Serialize};

/// 資料表資訊（list_tables 回傳）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableInfo {
    /// schema 名稱（如 MSSQL 的 dbo、PG 的 public）；SQLite 無 schema 為 None。
    pub schema: Option<String>,
    pub name: String,
}

/// 欄位資訊（get_columns 回傳）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    /// 資料庫原生型別名稱（NVARCHAR、TIMESTAMP …），僅供顯示與 mapping 參考。
    pub db_type: String,
    pub nullable: bool,
    pub ordinal: u32,
}
