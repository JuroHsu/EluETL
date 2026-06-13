//! 統一的 ETL 來源讀取：檔案（Excel / CSV）與資料庫查詢共用同一份
//! 「表頭 + 資料列」輸出，供 wizard / 腳本兩個 executor 使用。

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::db::driver::DbDriver;
use crate::etl::mapping::SourceSpec;
use crate::excel::source;
use crate::models::errors::EluEtlError;
use crate::models::value::CellValue;

/// 讀取完成的來源資料（rows 不含表頭列）。
pub struct SourceData {
    pub header: Vec<String>,
    pub rows: Vec<Vec<CellValue>>,
    /// 第一筆資料的來源行號（1-based；檔案表頭佔第 1 行 → 2）。
    /// 資料庫來源以結果集行號計，自 1 起。
    pub first_data_row: usize,
}

/// 自首列抽出表頭；空欄名以「欄位N」補齊（無表頭時全部以「欄位N」命名）。
pub fn header_from_rows(rows: &mut Vec<Vec<CellValue>>, has_header: bool) -> Vec<String> {
    if has_header && !rows.is_empty() {
        rows.remove(0)
            .iter()
            .enumerate()
            .map(|(i, c)| match c {
                CellValue::Text(s) if !s.is_empty() => s.clone(),
                _ => format!("欄位{}", i + 1),
            })
            .collect()
    } else {
        let n = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        (0..n).map(|i| format!("欄位{}", i + 1)).collect()
    }
}

/// 讀取檔案來源（同步 IO + 解析 → spawn_blocking）。
pub async fn read_file(
    path: &str,
    sheet: &str,
    encoding: Option<&str>,
    has_header: bool,
) -> Result<SourceData, EluEtlError> {
    let (path, sheet, encoding) = (
        path.to_string(),
        sheet.to_string(),
        encoding.map(str::to_string),
    );
    let mut rows =
        tokio::task::spawn_blocking(move || source::read_rows(&path, &sheet, encoding.as_deref()))
            .await??;
    let header = header_from_rows(&mut rows, has_header);
    Ok(SourceData {
        header,
        rows,
        first_data_row: if has_header { 2 } else { 1 },
    })
}

/// 讀取資料庫來源：一次性物化整個查詢結果（與匯出路徑相同的限制）。
pub async fn read_database(
    driver: Arc<dyn DbDriver>,
    query: &str,
) -> Result<SourceData, EluEtlError> {
    let result = driver
        .query_all(query, None)
        .await
        .map_err(|e| EluEtlError::Etl(format!("讀取來源查詢失敗：{e}")))?;
    Ok(SourceData {
        header: result.columns,
        rows: result.rows,
        first_data_row: 1,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 來源指紋（續跑安全檢查）：檔案 = 內容 SHA-256；
/// 資料庫 = 連線 + 查詢文字的 SHA-256（內容無法保證不變，續跑另行禁止）。
pub fn fingerprint(spec: &SourceSpec) -> Result<String, EluEtlError> {
    match spec {
        SourceSpec::File { path, .. } => Ok(sha256_hex(&std::fs::read(path)?)),
        SourceSpec::Database { conn_id, query } => {
            Ok(sha256_hex(format!("{conn_id}\n{query}").as_bytes()))
        }
    }
}
