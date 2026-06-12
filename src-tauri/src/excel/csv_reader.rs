use encoding_rs::Encoding;

use crate::models::errors::EluEtlError;
use crate::models::value::{parse_date, parse_datetime, CellValue};

/// 讀取 CSV 為中介值。
/// 編碼處理：BOM 優先 → chardetng 自動偵測（Big5 / UTF-8 / UTF-16 等）→
/// 呼叫端可傳 `encoding_override`（IANA 名稱，如 "Big5"）強制指定。
pub fn read_rows(
    path: &str,
    encoding_override: Option<&str>,
) -> Result<Vec<Vec<CellValue>>, EluEtlError> {
    let bytes = std::fs::read(path)?;
    let encoding = resolve_encoding(&bytes, encoding_override)?;
    let (text, _, had_errors) = encoding.decode(&bytes);
    if had_errors {
        tracing::warn!(path, encoding = encoding.name(), "CSV 解碼出現替代字元");
    }

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());

    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record?;
        rows.push(record.iter().map(parse_cell).collect());
    }
    Ok(rows)
}

/// 偵測使用的編碼名稱（供前端顯示確認）。
pub fn detect_encoding_name(path: &str) -> Result<String, EluEtlError> {
    let bytes = std::fs::read(path)?;
    Ok(resolve_encoding(&bytes, None)?.name().to_string())
}

fn resolve_encoding(
    bytes: &[u8],
    encoding_override: Option<&str>,
) -> Result<&'static Encoding, EluEtlError> {
    if let Some(label) = encoding_override {
        return Encoding::for_label(label.as_bytes())
            .ok_or_else(|| EluEtlError::Config(format!("未知的編碼: {label}")));
    }
    if let Some((enc, _)) = Encoding::for_bom(bytes) {
        return Ok(enc);
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    let sample_len = bytes.len().min(64 * 1024);
    detector.feed(&bytes[..sample_len], bytes.len() <= sample_len);
    Ok(detector.guess(None, chardetng::Utf8Detection::Allow))
}

/// CSV 欄位型別嘗試解析：整數 → 浮點 → 布林 → 日期時間 → 日期 → 文字。
/// 前導零（如 "007"）保留為文字，避免靜默破壞工號 / 編號類資料。
fn parse_cell(s: &str) -> CellValue {
    let t = s.trim();
    if t.is_empty() {
        return CellValue::Null;
    }
    let leading_zero = t.len() > 1 && t.starts_with('0') && !t[1..].starts_with('.');
    if !leading_zero {
        if let Ok(v) = t.parse::<i64>() {
            return CellValue::Int(v);
        }
        if let Ok(v) = t.parse::<f64>() {
            if v.is_finite() {
                return CellValue::Float(v);
            }
        }
    }
    match t.to_ascii_lowercase().as_str() {
        "true" => return CellValue::Bool(true),
        "false" => return CellValue::Bool(false),
        _ => {}
    }
    if t.len() >= 8 {
        if let Some(d) = parse_date(t) {
            return CellValue::Date(d);
        }
        if let Some(dt) = parse_datetime(t) {
            return CellValue::DateTime(dt);
        }
    }
    CellValue::Text(t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_typed_cells() {
        assert_eq!(parse_cell("42"), CellValue::Int(42));
        assert_eq!(parse_cell("3.5"), CellValue::Float(3.5));
        assert_eq!(parse_cell("TRUE"), CellValue::Bool(true));
        assert_eq!(parse_cell(""), CellValue::Null);
        // 前導零保留為文字（工號 / 編號）
        assert_eq!(parse_cell("007"), CellValue::Text("007".into()));
        assert_eq!(parse_cell("0.5"), CellValue::Float(0.5));
        assert!(matches!(parse_cell("2026-06-12"), CellValue::Date(_)));
    }

    #[test]
    fn decodes_big5_csv() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big5.csv");
        let (encoded, _, _) = encoding_rs::BIG5.encode("姓名,部門\n王小明,財務部\n");
        std::fs::write(&path, &encoded).unwrap();

        let rows = read_rows(path.to_str().unwrap(), None).unwrap();
        assert_eq!(rows[0][0], CellValue::Text("姓名".into()));
        assert_eq!(rows[1][1], CellValue::Text("財務部".into()));
    }

    #[test]
    fn explicit_encoding_override() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("o.csv");
        let (encoded, _, _) = encoding_rs::BIG5.encode("中文\n");
        std::fs::write(&path, &encoded).unwrap();
        let rows = read_rows(path.to_str().unwrap(), Some("Big5")).unwrap();
        assert_eq!(rows[0][0], CellValue::Text("中文".into()));
    }
}
