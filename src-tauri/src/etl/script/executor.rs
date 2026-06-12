//! ETL 腳本執行器：lookup join（hash 比對）+ 型別轉換 + 批次寫入。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::driver::DbDriver;
use crate::db::{quote_columns, quote_table, Dialect};
use crate::etl::executor::{EtlProgress, EtlSummary};
use crate::etl::script::ast::{Expr, Script};
use crate::etl::transform::RowError;
use crate::excel::source;
use crate::models::errors::EluEtlError;
use crate::models::value::{CellValue, DataType};

const ERROR_DETAIL_CAP: usize = 1_000;

/// 腳本任務參數。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptJobParams {
    pub job_id: Uuid,
    pub conn_id: Uuid,
    pub source_path: String,
    pub sheet: String,
    pub has_header: bool,
    #[serde(default)]
    pub encoding: Option<String>,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    pub script: String,
}

fn default_batch_size() -> usize {
    5_000
}

/// 指派值的繫結結果（執行前解析一次，行迴圈內零查找成本）。
enum Binding {
    Source(usize),
    Lookup(usize),
    Const(CellValue),
}

fn literal_to_cell(expr: &Expr) -> Option<CellValue> {
    match expr {
        Expr::Text(s) => Some(CellValue::Text(s.clone())),
        Expr::Int(v) => Some(CellValue::Int(*v)),
        Expr::Float(v) => Some(CellValue::Float(*v)),
        Expr::Bool(v) => Some(CellValue::Bool(*v)),
        Expr::Null => Some(CellValue::Null),
        Expr::Col(_) => None,
    }
}

/// lookup key 正規化：文字 trim + 不分大小寫；整數值的 Float 正規化為整數字串
/// （來源推斷為 Int、DB 端為 Float 時仍可比對）。
fn match_key(cell: &CellValue) -> Option<String> {
    match cell {
        CellValue::Null => None,
        CellValue::Text(s) => {
            let t = s.trim().to_lowercase();
            (!t.is_empty()).then_some(t)
        }
        CellValue::Int(v) => Some(v.to_string()),
        CellValue::Float(f) if f.fract() == 0.0 && f.abs() < i64::MAX as f64 => {
            Some((*f as i64).to_string())
        }
        CellValue::Float(f) => Some(f.to_string()),
        CellValue::Bool(b) => Some(b.to_string()),
        CellValue::DateTime(dt) => Some(dt.format("%Y-%m-%d %H:%M:%S").to_string()),
        CellValue::Date(d) => Some(d.format("%Y-%m-%d").to_string()),
    }
}

fn table_ref_to_string(parts: &[String], dialect: Dialect) -> Result<String, EluEtlError> {
    if parts.len() == 3 && !matches!(dialect, Dialect::Mssql) {
        return Err(EluEtlError::Config(format!(
            "{} 不支援 db 前綴的三段式資料表名稱（僅 SQL Server 支援）",
            parts.join(".")
        )));
    }
    Ok(parts.join("."))
}

pub fn run_blocking_parse(script_text: &str) -> Result<Script, EluEtlError> {
    crate::etl::script::parser::parse(script_text)
        .map_err(|e| EluEtlError::Etl(format!("第 {} 行：{}", e.line, e.message)))
}

/// 執行整份腳本。回傳彙總摘要（total = 來源行數；success = 寫入總行數）。
pub async fn run<F>(
    driver: Arc<dyn DbDriver>,
    dialect: Dialect,
    params: ScriptJobParams,
    script: Script,
    emit: F,
    cancel: CancellationToken,
) -> Result<EtlSummary, EluEtlError>
where
    F: Fn(EtlProgress) + Send + Sync,
{
    let started = Instant::now();

    // 讀取來源
    emit(progress(&params, "read", 0, 0, 0, 0));
    let mut rows = {
        let (path, sheet, encoding) = (
            params.source_path.clone(),
            params.sheet.clone(),
            params.encoding.clone(),
        );
        tokio::task::spawn_blocking(move || source::read_rows(&path, &sheet, encoding.as_deref()))
            .await??
    };

    // 表頭 → 欄名索引（不分大小寫）
    let header: Vec<String> = if params.has_header && !rows.is_empty() {
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
    };
    let col_index: HashMap<String, usize> = header
        .iter()
        .enumerate()
        .map(|(i, name)| (name.to_lowercase(), i))
        .collect();

    let total_rows = rows.len() as u64;
    let first_data_row = if params.has_header { 2 } else { 1 };
    let batch_size = params.batch_size.max(1);
    let total_batches = rows.len().div_ceil(batch_size).max(1) * script.statements.len();

    let mut success_rows: u64 = 0;
    let mut error_rows: u64 = 0;
    let mut errors: Vec<RowError> = Vec::new();
    let mut done_batches = 0usize;

    for stmt in &script.statements {
        if cancel.is_cancelled() {
            return Ok(summary(
                &params,
                "cancelled",
                total_rows,
                success_rows,
                error_rows,
                started,
                None,
                errors,
            ));
        }

        // ---- 解析目標表與欄位型別 ----
        let target = table_ref_to_string(&stmt.target_table, dialect)?;
        let target_cols = driver.get_columns(&target).await.map_err(|e| {
            EluEtlError::Etl(format!("第 {} 行：無法讀取目標表 {target}：{e}", stmt.line))
        })?;
        let target_types: HashMap<String, DataType> = target_cols
            .iter()
            .map(|c| (c.name.to_lowercase(), DataType::from_db_type(&c.db_type)))
            .collect();

        let columns: Vec<String> = stmt
            .assignments
            .iter()
            .map(|a| a.target_column.clone())
            .collect();
        let types: Vec<DataType> = stmt
            .assignments
            .iter()
            .map(|a| {
                target_types
                    .get(&a.target_column.to_lowercase())
                    .copied()
                    .ok_or_else(|| {
                        EluEtlError::Etl(format!(
                            "第 {} 行：目標表 {target} 沒有欄位 {}",
                            a.line, a.target_column
                        ))
                    })
            })
            .collect::<Result<_, _>>()?;

        // ---- 條件 lookup：載入比對表 ----
        let source_prefix = stmt
            .condition
            .as_ref()
            .map(|c| c.left.prefix_key())
            .unwrap_or_default();
        let lookup_prefix = stmt.condition.as_ref().map(|c| c.right.prefix_key());

        // 此陳述式引用到的 lookup 欄位（依出現順序，去重）
        let mut lookup_cols: Vec<String> = Vec::new();
        if let Some(lp) = &lookup_prefix {
            for a in &stmt.assignments {
                if let Expr::Col(r) = &a.value {
                    if &r.prefix_key() == lp
                        && !lookup_cols
                            .iter()
                            .any(|c| c.eq_ignore_ascii_case(&r.column))
                    {
                        lookup_cols.push(r.column.clone());
                    }
                }
            }
        }

        let lookup_map: Option<HashMap<String, Vec<CellValue>>> = match &stmt.condition {
            None => None,
            Some(cond) => {
                emit(progress(
                    &params,
                    "lookup",
                    done_batches,
                    total_batches,
                    success_rows,
                    error_rows,
                ));
                let lookup_table = table_ref_to_string(&cond.right.prefix, dialect)?;
                let mut select_cols = vec![cond.right.column.clone()];
                select_cols.extend(lookup_cols.iter().cloned());
                let sql = format!(
                    "SELECT {} FROM {}",
                    quote_columns(dialect, &select_cols)?,
                    quote_table(dialect, &lookup_table)?
                );
                let result = driver.query_all(&sql, None).await.map_err(|e| {
                    EluEtlError::Etl(format!(
                        "第 {} 行：讀取比對表 {lookup_table} 失敗：{e}",
                        cond.line
                    ))
                })?;
                let mut map: HashMap<String, Vec<CellValue>> =
                    HashMap::with_capacity(result.rows.len());
                let mut dup = 0usize;
                for mut row in result.rows {
                    let rest = row.split_off(1);
                    if let Some(key) = match_key(&row[0]) {
                        if map.insert(key, rest).is_some() {
                            dup += 1;
                        }
                    }
                }
                if dup > 0 {
                    tracing::warn!(
                        table = %lookup_table,
                        duplicates = dup,
                        "比對表鍵值重複，以最後一筆為準"
                    );
                }
                Some(map)
            }
        };

        // ---- 指派值繫結 ----
        let bindings: Vec<Binding> = stmt
            .assignments
            .iter()
            .map(|a| -> Result<Binding, EluEtlError> {
                if let Some(lit) = literal_to_cell(&a.value) {
                    return Ok(Binding::Const(lit));
                }
                let Expr::Col(r) = &a.value else {
                    unreachable!()
                };
                let key = r.prefix_key();
                if lookup_prefix.as_deref() == Some(key.as_str()) {
                    let pos = lookup_cols
                        .iter()
                        .position(|c| c.eq_ignore_ascii_case(&r.column))
                        .expect("lookup 欄位已預先收集");
                    return Ok(Binding::Lookup(pos));
                }
                if key.is_empty() || key == source_prefix || stmt.condition.is_none() {
                    let idx = col_index.get(&r.column.to_lowercase()).ok_or_else(|| {
                        EluEtlError::Etl(format!(
                            "第 {} 行：來源檔沒有欄位 {}（可用欄位：{}）",
                            r.line,
                            r.column,
                            header.join(", ")
                        ))
                    })?;
                    return Ok(Binding::Source(*idx));
                }
                Err(EluEtlError::Etl(format!(
                    "第 {} 行：未知的資料來源前綴 [{}]（來源為 [{}]，比對表為 [{}]）",
                    r.line,
                    key,
                    source_prefix,
                    lookup_prefix.as_deref().unwrap_or("-")
                )))
            })
            .collect::<Result<_, _>>()?;

        let match_src_idx: Option<usize> = match &stmt.condition {
            None => None,
            Some(cond) => Some(*col_index.get(&cond.left.column.to_lowercase()).ok_or_else(
                || {
                    EluEtlError::Etl(format!(
                        "第 {} 行：來源檔沒有比對欄位 {}",
                        cond.line, cond.left.column
                    ))
                },
            )?),
        };

        // ---- 行迴圈：比對 + 組裝 + 型別轉換 ----
        emit(progress(
            &params,
            "transform",
            done_batches,
            total_batches,
            success_rows,
            error_rows,
        ));
        let mut out_rows: Vec<Vec<CellValue>> = Vec::new();
        'row: for (i, row) in rows.iter().enumerate() {
            let row_no = first_data_row + i;
            let matched: Option<&Vec<CellValue>> = match (&lookup_map, match_src_idx) {
                (Some(map), Some(idx)) => {
                    let cell = row.get(idx).unwrap_or(&CellValue::Null);
                    let Some(key) = match_key(cell) else {
                        error_rows += 1;
                        push_error(&mut errors, row_no, &header[idx], "比對欄位為空".into());
                        continue 'row;
                    };
                    match map.get(&key) {
                        Some(m) => Some(m),
                        None => {
                            error_rows += 1;
                            push_error(
                                &mut errors,
                                row_no,
                                &header[idx],
                                format!("查無對應：{key}"),
                            );
                            continue 'row;
                        }
                    }
                }
                _ => None,
            };

            let mut out = Vec::with_capacity(bindings.len());
            for (b, (a, ty)) in bindings.iter().zip(stmt.assignments.iter().zip(&types)) {
                let raw = match b {
                    Binding::Const(v) => v.clone(),
                    Binding::Source(idx) => row.get(*idx).cloned().unwrap_or(CellValue::Null),
                    Binding::Lookup(pos) => matched
                        .and_then(|m| m.get(*pos))
                        .cloned()
                        .unwrap_or(CellValue::Null),
                };
                match raw.convert_to(*ty) {
                    Ok(v) => out.push(v),
                    Err(reason) => {
                        error_rows += 1;
                        push_error(&mut errors, row_no, &a.target_column, reason);
                        continue 'row;
                    }
                }
            }
            out_rows.push(out);
        }

        // ---- 批次寫入 ----
        for chunk in out_rows.chunks(batch_size) {
            if cancel.is_cancelled() {
                return Ok(summary(
                    &params,
                    "cancelled",
                    total_rows,
                    success_rows,
                    error_rows,
                    started,
                    None,
                    errors,
                ));
            }
            emit(progress(
                &params,
                "load",
                done_batches,
                total_batches,
                success_rows,
                error_rows,
            ));
            match driver.write_batch(&target, &columns, &types, chunk).await {
                Ok(n) => success_rows += n,
                Err(e) => {
                    return Ok(summary(
                        &params,
                        "failed",
                        total_rows,
                        success_rows,
                        error_rows,
                        started,
                        Some(format!("寫入 {target} 失敗：{e}")),
                        errors,
                    ));
                }
            }
            done_batches += 1;
            emit(progress(
                &params,
                "load",
                done_batches,
                total_batches,
                success_rows,
                error_rows,
            ));
        }
        // 空批次（全部行都被過濾）也要推進進度
        if out_rows.is_empty() {
            done_batches += rows.len().div_ceil(batch_size).max(1);
            emit(progress(
                &params,
                "load",
                done_batches.min(total_batches),
                total_batches,
                success_rows,
                error_rows,
            ));
        }

        tracing::info!(
            target: "audit",
            job_id = %params.job_id,
            statement_line = stmt.line,
            table = %target,
            inserted = out_rows.len(),
            "腳本陳述式完成"
        );
    }

    Ok(summary(
        &params,
        "completed",
        total_rows,
        success_rows,
        error_rows,
        started,
        None,
        errors,
    ))
}

fn push_error(errors: &mut Vec<RowError>, row: usize, column: &str, reason: String) {
    if errors.len() < ERROR_DETAIL_CAP {
        errors.push(RowError {
            row,
            column: column.to_string(),
            reason,
        });
    }
}

fn progress(
    params: &ScriptJobParams,
    phase: &str,
    batch: usize,
    total_batches: usize,
    success_rows: u64,
    error_rows: u64,
) -> EtlProgress {
    EtlProgress {
        job_id: params.job_id,
        phase: phase.to_string(),
        batch,
        total_batches,
        success_rows,
        error_rows,
    }
}

#[allow(clippy::too_many_arguments)]
fn summary(
    params: &ScriptJobParams,
    status: &str,
    total_rows: u64,
    success_rows: u64,
    error_rows: u64,
    started: Instant,
    failure: Option<String>,
    errors: Vec<RowError>,
) -> EtlSummary {
    EtlSummary {
        job_id: params.job_id,
        status: status.to_string(),
        total_rows,
        success_rows,
        error_rows,
        elapsed_ms: started.elapsed().as_millis() as u64,
        failure,
        errors,
    }
}
