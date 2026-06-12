use calamine::{open_workbook_auto, Data, Reader};

use crate::models::errors::EluEtlError;
use crate::models::value::{parse_datetime, CellValue};

/// 列出活頁簿中的工作表名稱。
pub fn list_sheets(path: &str) -> Result<Vec<String>, EluEtlError> {
    let workbook = open_workbook_auto(path)?;
    Ok(workbook.sheet_names().to_vec())
}

/// 讀取整個工作表為中介值（calamine 將 sheet 載入記憶體，
/// 大檔記憶體預檢由 command 層負責，見開發計畫 §2.2.2）。
pub fn read_rows(path: &str, sheet: &str) -> Result<Vec<Vec<CellValue>>, EluEtlError> {
    let mut workbook = open_workbook_auto(path)?;
    let range = workbook.worksheet_range(sheet)?;
    Ok(range
        .rows()
        .map(|row| row.iter().map(data_to_cell).collect())
        .collect())
}

/// calamine Data → CellValue。
/// Excel 日期（serial f64，1900/1904 紀元）由 calamine `dates` feature 處理。
fn data_to_cell(d: &Data) -> CellValue {
    match d {
        Data::Empty => CellValue::Null,
        Data::String(s) => {
            if s.is_empty() {
                CellValue::Null
            } else {
                CellValue::Text(s.clone())
            }
        }
        Data::Float(f) => CellValue::Float(*f),
        Data::Int(i) => CellValue::Int(*i),
        Data::Bool(b) => CellValue::Bool(*b),
        Data::DateTime(dt) => dt
            .as_datetime()
            .map(CellValue::DateTime)
            .unwrap_or(CellValue::Null),
        Data::DateTimeIso(s) => parse_datetime(s)
            .map(CellValue::DateTime)
            .unwrap_or_else(|| CellValue::Text(s.clone())),
        Data::DurationIso(s) => CellValue::Text(s.clone()),
        // 儲存格錯誤（#DIV/0! 等）視為 NULL，由 null 政策處理
        Data::Error(_) => CellValue::Null,
    }
}
