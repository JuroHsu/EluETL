use crate::models::value::{CellValue, DataType};

/// 取樣前 `sample` 列投票推斷各欄型別（開發計畫 §2.2.2）。
/// 全 NULL 的欄位回傳 None（前端標示「未定」，要求使用者指定）。
pub fn infer_types(rows: &[Vec<CellValue>], sample: usize) -> Vec<Option<DataType>> {
    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    (0..ncols)
        .map(|col| {
            rows.iter()
                .take(sample)
                .filter_map(|r| r.get(col).and_then(|c| c.data_type()))
                .fold(None, |acc, t| Some(merge(acc, t)))
        })
        .collect()
}

/// 型別合併規則：Int+Float → Float；Date+DateTime → DateTime；其餘混合 → Text。
fn merge(acc: Option<DataType>, t: DataType) -> DataType {
    match acc {
        None => t,
        Some(a) if a == t => a,
        Some(DataType::Integer) | Some(DataType::Float)
            if matches!(t, DataType::Integer | DataType::Float) =>
        {
            DataType::Float
        }
        Some(DataType::Date) | Some(DataType::DateTime)
            if matches!(t, DataType::Date | DataType::DateTime) =>
        {
            DataType::DateTime
        }
        _ => DataType::Text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn votes_types() {
        let rows = vec![
            vec![
                CellValue::Int(1),
                CellValue::Text("a".into()),
                CellValue::Null,
            ],
            vec![
                CellValue::Float(2.5),
                CellValue::Text("b".into()),
                CellValue::Null,
            ],
        ];
        let t = infer_types(&rows, 100);
        assert_eq!(t[0], Some(DataType::Float)); // Int + Float → Float
        assert_eq!(t[1], Some(DataType::Text));
        assert_eq!(t[2], None); // 全 NULL → 未定
    }

    #[test]
    fn mixed_becomes_text() {
        let rows = vec![vec![CellValue::Int(1)], vec![CellValue::Text("x".into())]];
        assert_eq!(infer_types(&rows, 100)[0], Some(DataType::Text));
    }
}
