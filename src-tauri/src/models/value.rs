use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

/// 統一資料型別（型別轉換矩陣，見開發計畫 §4.3）。
/// Decimal（精確小數）列入 backlog，金額暫以 Text 或 Float 由使用者選擇。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataType {
    Integer,
    Float,
    Text,
    Bool,
    DateTime,
    Date,
}

impl DataType {
    /// DB 原生型別名稱 → 中介型別（INFORMATION_SCHEMA 的 data_type 字串）。
    pub fn from_db_type(db_type: &str) -> DataType {
        let t = db_type.to_lowercase();
        if t.contains("bool") || t == "bit" {
            DataType::Bool
        } else if t.contains("int") || t.contains("serial") || t == "year" {
            DataType::Integer
        } else if ["decimal", "numeric", "real", "double", "float", "money"]
            .iter()
            .any(|k| t.contains(k))
        {
            DataType::Float
        } else if t.contains("datetime") || t.contains("timestamp") {
            DataType::DateTime
        } else if t == "date" {
            DataType::Date
        } else {
            DataType::Text
        }
    }
}

/// Excel / CSV / DB 之間的中介值。
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
    DateTime(NaiveDateTime),
    Date(NaiveDate),
}

const DATETIME_FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%dT%H:%M:%S",
    "%Y/%m/%d %H:%M:%S",
    "%Y-%m-%d %H:%M",
    "%Y/%m/%d %H:%M",
];

const DATE_FORMATS: &[&str] = &["%Y-%m-%d", "%Y/%m/%d"];

impl CellValue {
    pub fn is_null(&self) -> bool {
        matches!(self, CellValue::Null)
    }

    pub fn data_type(&self) -> Option<DataType> {
        match self {
            CellValue::Null => None,
            CellValue::Int(_) => Some(DataType::Integer),
            CellValue::Float(_) => Some(DataType::Float),
            CellValue::Text(_) => Some(DataType::Text),
            CellValue::Bool(_) => Some(DataType::Bool),
            CellValue::DateTime(_) => Some(DataType::DateTime),
            CellValue::Date(_) => Some(DataType::Date),
        }
    }

    /// 序列化為 JSON（預覽 / 查詢結果跨 IPC 用；日期時間輸出 ISO 8601）。
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            CellValue::Null => serde_json::Value::Null,
            CellValue::Int(v) => serde_json::json!(v),
            CellValue::Float(v) => serde_json::json!(v),
            CellValue::Text(v) => serde_json::json!(v),
            CellValue::Bool(v) => serde_json::json!(v),
            CellValue::DateTime(v) => serde_json::json!(v.format("%Y-%m-%d %H:%M:%S").to_string()),
            CellValue::Date(v) => serde_json::json!(v.format("%Y-%m-%d").to_string()),
        }
    }

    /// 依型別矩陣轉換（開發計畫 §4.3）。
    /// 失敗回傳人類可讀原因；絕不靜默截斷（溢位、精度損失、無效日期一律報錯）。
    pub fn convert_to(&self, target: DataType) -> Result<CellValue, String> {
        match (self, target) {
            (CellValue::Null, _) => Ok(CellValue::Null),

            // ---- Integer ----
            (CellValue::Int(v), DataType::Integer) => Ok(CellValue::Int(*v)),
            (CellValue::Float(f), DataType::Integer) => {
                if f.fract() == 0.0 && *f >= i64::MIN as f64 && *f <= i64::MAX as f64 {
                    Ok(CellValue::Int(*f as i64))
                } else {
                    Err(format!("無法將 {f} 轉為整數（非整數或溢位）"))
                }
            }
            (CellValue::Text(s), DataType::Integer) => s
                .trim()
                .parse::<i64>()
                .map(CellValue::Int)
                .map_err(|_| format!("無法將「{s}」解析為整數")),
            (CellValue::Bool(b), DataType::Integer) => Ok(CellValue::Int(i64::from(*b))),

            // ---- Float ----
            (CellValue::Float(v), DataType::Float) => Ok(CellValue::Float(*v)),
            (CellValue::Int(v), DataType::Float) => Ok(CellValue::Float(*v as f64)),
            (CellValue::Text(s), DataType::Float) => s
                .trim()
                .parse::<f64>()
                .map(CellValue::Float)
                .map_err(|_| format!("無法將「{s}」解析為浮點數")),

            // ---- Text ----
            (v, DataType::Text) => Ok(CellValue::Text(v.to_display_string())),

            // ---- Bool ----
            (CellValue::Bool(v), DataType::Bool) => Ok(CellValue::Bool(*v)),
            (CellValue::Int(v), DataType::Bool) => match v {
                0 => Ok(CellValue::Bool(false)),
                1 => Ok(CellValue::Bool(true)),
                _ => Err(format!("無法將 {v} 轉為布林（僅接受 0/1）")),
            },
            (CellValue::Text(s), DataType::Bool) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "1" => Ok(CellValue::Bool(true)),
                "false" | "0" => Ok(CellValue::Bool(false)),
                _ => Err(format!("無法將「{s}」解析為布林")),
            },

            // ---- DateTime ----
            (CellValue::DateTime(v), DataType::DateTime) => Ok(CellValue::DateTime(*v)),
            (CellValue::Date(d), DataType::DateTime) => {
                Ok(CellValue::DateTime(d.and_hms_opt(0, 0, 0).unwrap()))
            }
            (CellValue::Text(s), DataType::DateTime) => parse_datetime(s.trim())
                .map(CellValue::DateTime)
                .ok_or_else(|| format!("無法將「{s}」解析為日期時間")),

            // ---- Date ----
            (CellValue::Date(v), DataType::Date) => Ok(CellValue::Date(*v)),
            (CellValue::DateTime(dt), DataType::Date) => Ok(CellValue::Date(dt.date())),
            (CellValue::Text(s), DataType::Date) => parse_date(s.trim())
                .map(CellValue::Date)
                .ok_or_else(|| format!("無法將「{s}」解析為日期")),

            (v, t) => Err(format!("不支援 {:?} → {:?} 的轉換", v.data_type(), t)),
        }
    }

    fn to_display_string(&self) -> String {
        match self {
            CellValue::Null => String::new(),
            CellValue::Int(v) => v.to_string(),
            CellValue::Float(v) => v.to_string(),
            CellValue::Text(v) => v.clone(),
            CellValue::Bool(v) => v.to_string(),
            CellValue::DateTime(v) => v.format("%Y-%m-%d %H:%M:%S").to_string(),
            CellValue::Date(v) => v.format("%Y-%m-%d").to_string(),
        }
    }
}

pub fn parse_datetime(s: &str) -> Option<NaiveDateTime> {
    for fmt in DATETIME_FORMATS {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt);
        }
    }
    parse_date(s).and_then(|d| d.and_hms_opt(0, 0, 0))
}

pub fn parse_date(s: &str) -> Option<NaiveDate> {
    for fmt in DATE_FORMATS {
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            return Some(d);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_conversions() {
        assert_eq!(
            CellValue::Float(3.0).convert_to(DataType::Integer),
            Ok(CellValue::Int(3))
        );
        assert!(CellValue::Float(3.5).convert_to(DataType::Integer).is_err());
        assert_eq!(
            CellValue::Text(" 42 ".into()).convert_to(DataType::Integer),
            Ok(CellValue::Int(42))
        );
        assert!(CellValue::Text("abc".into())
            .convert_to(DataType::Integer)
            .is_err());
    }

    #[test]
    fn bool_conversions() {
        assert_eq!(
            CellValue::Int(1).convert_to(DataType::Bool),
            Ok(CellValue::Bool(true))
        );
        assert!(CellValue::Int(2).convert_to(DataType::Bool).is_err());
        assert_eq!(
            CellValue::Text("TRUE".into()).convert_to(DataType::Bool),
            Ok(CellValue::Bool(true))
        );
    }

    #[test]
    fn datetime_conversions() {
        let d = NaiveDate::from_ymd_opt(2026, 6, 12).unwrap();
        assert_eq!(
            CellValue::Text("2026/06/12".into()).convert_to(DataType::Date),
            Ok(CellValue::Date(d))
        );
        assert_eq!(
            CellValue::Date(d).convert_to(DataType::DateTime),
            Ok(CellValue::DateTime(d.and_hms_opt(0, 0, 0).unwrap()))
        );
        // DateTime → Date 為明確截斷（使用者選擇），允許
        let dt = d.and_hms_opt(13, 30, 0).unwrap();
        assert_eq!(
            CellValue::DateTime(dt).convert_to(DataType::Date),
            Ok(CellValue::Date(d))
        );
        assert!(CellValue::Text("not a date".into())
            .convert_to(DataType::Date)
            .is_err());
    }

    #[test]
    fn null_passes_through() {
        assert_eq!(
            CellValue::Null.convert_to(DataType::Integer),
            Ok(CellValue::Null)
        );
    }

    #[test]
    fn anything_to_text() {
        assert_eq!(
            CellValue::Int(7).convert_to(DataType::Text),
            Ok(CellValue::Text("7".into()))
        );
        let dt = NaiveDate::from_ymd_opt(2026, 1, 2)
            .unwrap()
            .and_hms_opt(3, 4, 5)
            .unwrap();
        assert_eq!(
            CellValue::DateTime(dt).convert_to(DataType::Text),
            Ok(CellValue::Text("2026-01-02 03:04:05".into()))
        );
    }
}
