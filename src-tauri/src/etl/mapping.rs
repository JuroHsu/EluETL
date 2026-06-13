use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::value::DataType;

/// NULL 值處理政策。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NullPolicy {
    /// 寫入 NULL
    Allow,
    /// 視為錯誤（行進錯誤報告）
    Error,
}

/// 單一欄位對應規則：來源欄 → 目標欄 + 型別轉換 + NULL 政策。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingRule {
    pub source_index: usize,
    pub source_name: String,
    pub target_column: String,
    pub target_type: DataType,
    pub null_policy: NullPolicy,
}

/// 寫入模式（開發計畫 §4.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum WriteMode {
    /// 每批一個交易，commit 後記 checkpoint（預設；可續跑）
    BatchCommit,
    /// 整個任務單一交易，任何錯誤全部回滾
    AllOrNothing,
}

/// 錯誤政策（開發計畫 §4.4）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "policy")]
pub enum ErrorPolicy {
    /// 錯誤行進報告，繼續執行（預設）
    SkipAndReport,
    /// 首錯即停
    AbortOnFirst,
    /// 錯誤率超過 maxPercent% 自動中止
    AbortOnErrorRate { max_percent: f32 },
}

fn default_batch_size() -> usize {
    5_000
}

/// ETL 來源：檔案（Excel / CSV）或資料庫查詢。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum SourceSpec {
    /// 檔案來源（`sheet` 已解析為具體工作表名稱）
    #[serde(rename_all = "camelCase")]
    File {
        path: String,
        sheet: String,
        has_header: bool,
        #[serde(default)]
        encoding: Option<String>,
    },
    /// 資料庫來源：以已儲存連線執行查詢（密碼留在 OS keychain）
    #[serde(rename_all = "camelCase")]
    Database { conn_id: Uuid, query: String },
}

impl SourceSpec {
    /// 顯示用標籤（日誌 / 進度訊息）。
    pub fn label(&self) -> String {
        match self {
            SourceSpec::File { path, .. } => std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone()),
            SourceSpec::Database { query, .. } => format!("DB 查詢（{}）", query.trim()),
        }
    }
}

/// ETL 任務設定（前端組裝後跨 IPC 傳入；序列化存於 state.db 供續跑）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EtlJobConfig {
    pub job_id: Uuid,
    pub conn_id: Uuid,
    pub source: SourceSpec,
    pub target_table: String,
    pub rules: Vec<MappingRule>,
    pub write_mode: WriteMode,
    pub error_policy: ErrorPolicy,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}
