use std::path::Path;

use crate::excel::{csv_reader, reader};
use crate::models::errors::EluEtlError;
use crate::models::value::CellValue;

/// 來源檔種類（依副檔名判斷）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Excel,
    Csv,
}

pub fn detect_kind(path: &str) -> Result<SourceKind, EluEtlError> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "xlsx" | "xlsm" | "xls" | "xlsb" | "ods" => Ok(SourceKind::Excel),
        "csv" | "txt" | "tsv" => Ok(SourceKind::Csv),
        _ => Err(EluEtlError::Config(format!("不支援的檔案格式: .{ext}"))),
    }
}

/// 列出工作表（CSV 視為單一工作表 "CSV"）。
pub fn list_sheets(path: &str) -> Result<Vec<String>, EluEtlError> {
    match detect_kind(path)? {
        SourceKind::Excel => reader::list_sheets(path),
        SourceKind::Csv => Ok(vec!["CSV".to_string()]),
    }
}

/// 讀取資料列（含表頭列；表頭處理由呼叫端負責）。
pub fn read_rows(
    path: &str,
    sheet: &str,
    encoding_override: Option<&str>,
) -> Result<Vec<Vec<CellValue>>, EluEtlError> {
    match detect_kind(path)? {
        SourceKind::Excel => reader::read_rows(path, sheet),
        SourceKind::Csv => csv_reader::read_rows(path, encoding_override),
    }
}
