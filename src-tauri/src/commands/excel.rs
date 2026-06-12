use serde::Serialize;

use crate::excel::{csv_reader, schema_infer, source};
use crate::models::errors::EluEtlError;
use crate::models::value::DataType;

/// 預覽中顯示 / 取樣的行數上限。
const PREVIEW_ROWS: usize = 100;
/// 型別推斷取樣行數（開發計畫 §2.2.2）。
const INFER_SAMPLE: usize = 100;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnPreview {
    pub index: usize,
    pub name: String,
    /// None = 取樣全為 NULL，型別未定，需使用者指定
    pub inferred_type: Option<DataType>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePreview {
    pub columns: Vec<ColumnPreview>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub total_rows: usize,
    /// CSV 偵測到的編碼（Excel 為 None）
    pub encoding: Option<String>,
}

#[tauri::command]
pub async fn list_sheets(path: String) -> Result<Vec<String>, EluEtlError> {
    tokio::task::spawn_blocking(move || source::list_sheets(&path)).await?
}

/// 讀取預覽：前 100 行資料 + 表頭 + 型別推斷。
/// 注意：calamine 會載入整個 sheet（記憶體預檢警告由前端依 totalRows 顯示）。
#[tauri::command]
pub async fn read_preview(
    path: String,
    sheet: String,
    has_header: bool,
    encoding: Option<String>,
) -> Result<SourcePreview, EluEtlError> {
    tokio::task::spawn_blocking(move || {
        let detected = match source::detect_kind(&path)? {
            source::SourceKind::Csv => Some(match &encoding {
                Some(e) => e.clone(),
                None => csv_reader::detect_encoding_name(&path)?,
            }),
            source::SourceKind::Excel => None,
        };
        let mut rows = source::read_rows(&path, &sheet, encoding.as_deref())?;

        let header: Vec<String> = if has_header && !rows.is_empty() {
            let first = rows.remove(0);
            first
                .iter()
                .enumerate()
                .map(|(i, c)| match c.to_json() {
                    serde_json::Value::String(s) if !s.is_empty() => s,
                    serde_json::Value::Null => format!("欄位{}", i + 1),
                    v => v.to_string(),
                })
                .collect()
        } else {
            let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
            (0..ncols).map(|i| format!("欄位{}", i + 1)).collect()
        };

        let inferred = schema_infer::infer_types(&rows, INFER_SAMPLE);
        let columns = header
            .into_iter()
            .enumerate()
            .map(|(i, name)| ColumnPreview {
                index: i,
                name,
                inferred_type: inferred.get(i).copied().flatten(),
            })
            .collect();

        Ok(SourcePreview {
            columns,
            total_rows: rows.len(),
            rows: rows
                .iter()
                .take(PREVIEW_ROWS)
                .map(|r| r.iter().map(|c| c.to_json()).collect())
                .collect(),
            encoding: detected,
        })
    })
    .await?
}
