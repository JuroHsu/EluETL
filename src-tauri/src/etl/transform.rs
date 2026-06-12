use rayon::prelude::*;
use serde::Serialize;

use crate::etl::mapping::{MappingRule, NullPolicy};
use crate::models::value::CellValue;

/// 單行轉換錯誤（行號為來源檔行號，1-based，含表頭偏移由呼叫端計算）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowError {
    pub row: usize,
    pub column: String,
    pub reason: String,
}

/// 批次轉換（rayon 平行）：回傳（成功列, 錯誤清單）。
/// 一行內首個錯誤即記錄該行並跳過整行（不寫入半行資料）。
pub fn transform_rows(
    rows: &[Vec<CellValue>],
    rules: &[MappingRule],
    row_offset: usize,
) -> (Vec<Vec<CellValue>>, Vec<RowError>) {
    let results: Vec<Result<Vec<CellValue>, RowError>> = rows
        .par_iter()
        .enumerate()
        .map(|(i, row)| transform_row(row, rules, row_offset + i))
        .collect();

    let mut ok_rows = Vec::with_capacity(results.len());
    let mut errors = Vec::new();
    for r in results {
        match r {
            Ok(row) => ok_rows.push(row),
            Err(e) => errors.push(e),
        }
    }
    (ok_rows, errors)
}

fn transform_row(
    row: &[CellValue],
    rules: &[MappingRule],
    row_number: usize,
) -> Result<Vec<CellValue>, RowError> {
    let mut out = Vec::with_capacity(rules.len());
    for rule in rules {
        let cell = row.get(rule.source_index).unwrap_or(&CellValue::Null);
        if cell.is_null() {
            match rule.null_policy {
                NullPolicy::Allow => out.push(CellValue::Null),
                NullPolicy::Error => {
                    return Err(RowError {
                        row: row_number,
                        column: rule.source_name.clone(),
                        reason: "欄位為空，但 NULL 政策為不允許".to_string(),
                    })
                }
            }
            continue;
        }
        match cell.convert_to(rule.target_type) {
            Ok(v) => out.push(v),
            Err(reason) => {
                return Err(RowError {
                    row: row_number,
                    column: rule.source_name.clone(),
                    reason,
                })
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::value::DataType;

    fn rule(idx: usize, ty: DataType, null_policy: NullPolicy) -> MappingRule {
        MappingRule {
            source_index: idx,
            source_name: format!("col{idx}"),
            target_column: format!("t{idx}"),
            target_type: ty,
            null_policy,
        }
    }

    #[test]
    fn transforms_and_collects_errors() {
        let rows = vec![
            vec![CellValue::Text("42".into()), CellValue::Text("ok".into())],
            vec![CellValue::Text("abc".into()), CellValue::Text("x".into())],
            vec![CellValue::Null, CellValue::Text("y".into())],
        ];
        let rules = vec![
            rule(0, DataType::Integer, NullPolicy::Error),
            rule(1, DataType::Text, NullPolicy::Allow),
        ];
        let (ok, errs) = transform_rows(&rows, &rules, 2); // 資料自第 2 行起（表頭第 1 行）
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0][0], CellValue::Int(42));
        assert_eq!(errs.len(), 2);
        assert_eq!(errs[0].row, 3); // "abc" 在來源檔第 3 行
        assert_eq!(errs[1].row, 4); // NULL 在來源檔第 4 行
    }
}
