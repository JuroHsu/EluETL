//! ETL 腳本執行器（v0.2）：多 JOIN 查表（hash 比對）+ WHERE 過濾 +
//! merge upsert（預載目標鍵集合 probe）+ 型別轉換 + 批次寫入。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::driver::DbDriver;
use crate::db::{equality_clause, quote_columns, quote_table, Dialect};
use crate::etl::executor::{EtlProgress, EtlSummary};
use crate::etl::script::ast::{
    Action, Assignment, CmpOp, Condition, ConnRef, Expr, GenKind, Join, JoinPolicy, Script, Work,
};
use crate::etl::script::gen;
use crate::etl::source_input;
use crate::etl::transform::RowError;
use crate::models::errors::EluEtlError;
use crate::models::value::{CellValue, DataType};

const ERROR_DETAIL_CAP: usize = 1_000;
/// 複合鍵分隔符（不可能出現在正規化值內的控制字元）。
const KEY_SEP: char = '\u{1f}';

/// 腳本任務參數（IPC 傳入）。來源與目標皆為選擇性：
/// 腳本標頭（SOURCE/TARGET）優先，否則回退到此處的工作區選擇。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptJobParams {
    pub job_id: Uuid,
    #[serde(default)]
    pub conn_id: Option<Uuid>,
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub sheet: Option<String>,
    #[serde(default)]
    pub has_header: Option<bool>,
    #[serde(default)]
    pub encoding: Option<String>,
    #[serde(default)]
    pub source_conn_id: Option<Uuid>,
    #[serde(default)]
    pub source_query: Option<String>,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    pub script: String,
}

fn default_batch_size() -> usize {
    5_000
}

/// 解析完成的腳本來源：檔案，或已建好驅動的資料庫查詢。
#[derive(Clone)]
pub enum ScriptSource {
    File {
        path: String,
        sheet: String,
        has_header: bool,
        encoding: Option<String>,
    },
    Database {
        driver: Arc<dyn DbDriver>,
        query: String,
    },
}

impl ScriptSource {
    /// audit 日誌用標籤（檔案路徑或查詢文字）。
    pub fn label(&self) -> &str {
        match self {
            ScriptSource::File { path, .. } => path,
            ScriptSource::Database { query, .. } => query,
        }
    }
}

/// 標頭 / 工作區解析完成後的具體任務（executor 的輸入）。
#[derive(Clone)]
pub struct ResolvedScriptJob {
    pub job_id: Uuid,
    pub source: ScriptSource,
    pub batch_size: usize,
}

// ---------- 指派 / 條件運算元的執行期繫結 ----------

/// 運算式繫結為執行期取值（行迴圈內零查找成本）。
enum ValBind {
    /// 主來源欄位 → row[idx]
    Source(usize),
    /// 查表欄位 → joins[join_idx] 的第 pos 個值（未命中時取 NULL）
    Join(usize, usize),
    Const(CellValue),
    Gen(GenKind),
    /// 合成欄位：各項求值後轉文字串接（NULL 視為空字串）
    Concat(Vec<ValBind>),
}

/// 別名 → 取值來源的繫結環境（執行前建立一次）。
struct BindCtx<'a> {
    from_alias: &'a str,
    col_index: &'a HashMap<String, usize>,
    header: &'a [String],
    /// (alias_lower, 欄位名小寫 → 值位置)
    joins: &'a [JoinMeta],
}

struct JoinMeta {
    alias_lower: String,
    col_pos: HashMap<String, usize>,
}

fn build_valbind(expr: &Expr, ctx: &BindCtx<'_>) -> Result<ValBind, EluEtlError> {
    match expr {
        Expr::Text(s) => Ok(ValBind::Const(CellValue::Text(s.clone()))),
        Expr::Int(v) => Ok(ValBind::Const(CellValue::Int(*v))),
        Expr::Float(v) => Ok(ValBind::Const(CellValue::Float(*v))),
        Expr::Bool(v) => Ok(ValBind::Const(CellValue::Bool(*v))),
        Expr::Null => Ok(ValBind::Const(CellValue::Null)),
        Expr::Gen(k) => Ok(ValBind::Gen(*k)),
        Expr::Concat(parts) => Ok(ValBind::Concat(
            parts
                .iter()
                .map(|p| build_valbind(p, ctx))
                .collect::<Result<_, _>>()?,
        )),
        Expr::Col(r) => {
            let alias = r.alias.to_lowercase();
            if alias == ctx.from_alias {
                let idx = ctx.col_index.get(&r.column.to_lowercase()).ok_or_else(|| {
                    EluEtlError::Etl(format!(
                        "第 {} 行：來源沒有欄位 {}（可用欄位：{}）",
                        r.line,
                        r.column,
                        ctx.header.join(", ")
                    ))
                })?;
                return Ok(ValBind::Source(*idx));
            }
            if let Some((jidx, meta)) = ctx
                .joins
                .iter()
                .enumerate()
                .find(|(_, m)| m.alias_lower == alias)
            {
                let pos = meta.col_pos.get(&r.column.to_lowercase()).ok_or_else(|| {
                    EluEtlError::Etl(format!(
                        "第 {} 行：查表 [{}] 沒有欄位 {}",
                        r.line, r.alias, r.column
                    ))
                })?;
                return Ok(ValBind::Join(jidx, *pos));
            }
            Err(EluEtlError::Etl(format!(
                "第 {} 行：別名 [{}] 未對應到主來源或任何查表",
                r.line, r.alias
            )))
        }
    }
}

/// 行迴圈內的取值（合成欄位遞迴求值）。`joins` 為本列各查表的命中值（None = 未命中）。
fn eval_valbind(
    b: &ValBind,
    row: &[CellValue],
    joins: &[Option<&Vec<CellValue>>],
    gen_ctx: &gen::GenContext,
) -> CellValue {
    match b {
        ValBind::Const(v) => v.clone(),
        ValBind::Source(idx) => row.get(*idx).cloned().unwrap_or(CellValue::Null),
        ValBind::Join(j, pos) => joins
            .get(*j)
            .copied()
            .flatten()
            .and_then(|vals| vals.get(*pos))
            .cloned()
            .unwrap_or(CellValue::Null),
        ValBind::Gen(k) => gen::generate(*k, row, gen_ctx),
        ValBind::Concat(parts) => CellValue::Text(
            parts
                .iter()
                .map(|p| eval_valbind(p, row, joins, gen_ctx).to_display_string())
                .collect::<String>(),
        ),
    }
}

// ---------- 條件（布林樹 + 比較 / IS [NOT] EMPTY / IN / LIKE / BETWEEN） ----------

enum CondPlan {
    And(Vec<CondPlan>),
    Or(Vec<CondPlan>),
    Not(Box<CondPlan>),
    Compare(ValBind, CmpOp, ValBind),
    IsEmpty(ValBind, bool),                 // (運算元, negated)
    In(ValBind, Vec<ValBind>, bool),        // (運算元, 清單, negated)
    Like(ValBind, ValBind, bool),           // (運算元, 樣式, negated)
    Between(ValBind, ValBind, ValBind, bool), // (運算元, 下界, 上界, negated)
}

fn build_cond_plan(cond: &Condition, ctx: &BindCtx<'_>) -> Result<CondPlan, EluEtlError> {
    Ok(match cond {
        Condition::And(v) => CondPlan::And(
            v.iter().map(|c| build_cond_plan(c, ctx)).collect::<Result<_, _>>()?,
        ),
        Condition::Or(v) => CondPlan::Or(
            v.iter().map(|c| build_cond_plan(c, ctx)).collect::<Result<_, _>>()?,
        ),
        Condition::Not(c) => CondPlan::Not(Box::new(build_cond_plan(c, ctx)?)),
        Condition::Compare(c) => CondPlan::Compare(
            build_valbind(&c.left, ctx)?,
            c.op,
            build_valbind(&c.right, ctx)?,
        ),
        Condition::IsEmpty(e) => CondPlan::IsEmpty(build_valbind(&e.expr, ctx)?, e.negated),
        Condition::In {
            expr,
            list,
            negated,
            ..
        } => CondPlan::In(
            build_valbind(expr, ctx)?,
            list.iter().map(|e| build_valbind(e, ctx)).collect::<Result<_, _>>()?,
            *negated,
        ),
        Condition::Like {
            expr,
            pattern,
            negated,
            ..
        } => CondPlan::Like(
            build_valbind(expr, ctx)?,
            build_valbind(pattern, ctx)?,
            *negated,
        ),
        Condition::Between {
            expr,
            low,
            high,
            negated,
            ..
        } => CondPlan::Between(
            build_valbind(expr, ctx)?,
            build_valbind(low, ctx)?,
            build_valbind(high, ctx)?,
            *negated,
        ),
    })
}

fn eval_cond_plan(
    plan: &CondPlan,
    row: &[CellValue],
    joins: &[Option<&Vec<CellValue>>],
    gen_ctx: &gen::GenContext,
) -> bool {
    let ev = |b: &ValBind| eval_valbind(b, row, joins, gen_ctx);
    match plan {
        CondPlan::And(v) => v.iter().all(|c| eval_cond_plan(c, row, joins, gen_ctx)),
        CondPlan::Or(v) => v.iter().any(|c| eval_cond_plan(c, row, joins, gen_ctx)),
        CondPlan::Not(c) => !eval_cond_plan(c, row, joins, gen_ctx),
        CondPlan::Compare(l, op, r) => eval_compare(&ev(l), *op, &ev(r)),
        CondPlan::IsEmpty(e, negated) => cell_is_empty(&ev(e)) != *negated,
        CondPlan::In(e, list, negated) => {
            let v = match_key(&ev(e));
            let hit = list.iter().any(|item| v.is_some() && match_key(&ev(item)) == v);
            hit != *negated
        }
        CondPlan::Like(e, pat, negated) => {
            let v = ev(e).to_display_string();
            let p = ev(pat).to_display_string();
            like_match(&v, &p) != *negated
        }
        CondPlan::Between(e, lo, hi, negated) => {
            let v = ev(e);
            let within = eval_compare(&v, CmpOp::Ge, &ev(lo))
                && eval_compare(&v, CmpOp::Le, &ev(hi));
            within != *negated
        }
    }
}

/// SQL LIKE 比對：`%` = 任意序列、`_` = 任意單一字元；不分大小寫（貪婪 + 回溯）。
fn like_match(value: &str, pattern: &str) -> bool {
    let v: Vec<char> = value.to_lowercase().chars().collect();
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let (mut vi, mut pi) = (0usize, 0usize);
    let (mut star_p, mut star_v): (Option<usize>, Option<usize>) = (None, None);
    while vi < v.len() {
        if pi < p.len() && (p[pi] == '_' || p[pi] == v[vi]) {
            vi += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '%' {
            star_p = Some(pi);
            star_v = Some(vi);
            pi += 1;
        } else if let (Some(sp), Some(sv)) = (star_p, star_v) {
            pi = sp + 1;
            vi = sv + 1;
            star_v = Some(sv + 1);
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '%' {
        pi += 1;
    }
    pi == p.len()
}

fn cell_is_empty(cell: &CellValue) -> bool {
    match cell {
        CellValue::Null => true,
        CellValue::Text(s) => s.trim().is_empty(),
        _ => false,
    }
}

fn eval_compare(left: &CellValue, op: CmpOp, right: &CellValue) -> bool {
    match op {
        CmpOp::Eq => match_key(left) == match_key(right),
        CmpOp::Ne => match_key(left) != match_key(right),
        CmpOp::Gt | CmpOp::Lt | CmpOp::Ge | CmpOp::Le => {
            let ord = match (as_number(left), as_number(right)) {
                (Some(a), Some(b)) => a.partial_cmp(&b),
                _ => match (match_key(left), match_key(right)) {
                    (Some(a), Some(b)) => Some(a.cmp(&b)),
                    _ => None,
                },
            };
            match ord {
                None => false,
                Some(o) => match op {
                    CmpOp::Gt => o.is_gt(),
                    CmpOp::Lt => o.is_lt(),
                    CmpOp::Ge => o.is_ge(),
                    CmpOp::Le => o.is_le(),
                    _ => unreachable!(),
                },
            }
        }
    }
}

fn as_number(cell: &CellValue) -> Option<f64> {
    match cell {
        CellValue::Int(v) => Some(*v as f64),
        CellValue::Float(v) => Some(*v),
        CellValue::Text(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

// ---------- 查表 ----------

/// 載入完成的查表計畫（per work）。
struct JoinRuntime {
    alias: String,
    policy: JoinPolicy,
    line: usize,
    /// 每筆來源列的 probe 鍵（對應 lookup 表的鍵欄順序）
    probes: Vec<ValBind>,
    /// 命中值：複合鍵 → 值欄位列
    map: HashMap<String, Vec<CellValue>>,
}

/// lookup key 正規化：文字 trim + 不分大小寫；整數值的 Float 正規化為整數字串。
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

/// 組合複合鍵：任一段為空（None）→ 整個鍵視為缺值（None）。
fn compose_key(cells: &[CellValue]) -> Option<String> {
    let mut parts = Vec::with_capacity(cells.len());
    for c in cells {
        parts.push(match_key(c)?);
    }
    Some(parts.join(&KEY_SEP.to_string()))
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

/// 取得某 JOIN 的 driver：TARGET → 目標 driver；SOURCE → 來源 driver（限資料庫來源）。
fn join_driver(
    conn: ConnRef,
    source: &ScriptSource,
    target_driver: &Arc<dyn DbDriver>,
) -> Result<Arc<dyn DbDriver>, EluEtlError> {
    match conn {
        ConnRef::Target => Ok(target_driver.clone()),
        ConnRef::Source => match source {
            ScriptSource::Database { driver, .. } => Ok(driver.clone()),
            ScriptSource::File { .. } => Err(EluEtlError::Etl(
                "JOIN 於 SOURCE，但來源是檔案，無法當查表（請改用 TARGET 連線）".into(),
            )),
        },
    }
}

/// 從一張 JOIN 的 ON 條件拆出：本表鍵欄（lookup 側）與每列 probe 運算式（來源側）。
fn split_join_keys<'a>(
    join: &'a Join,
) -> Result<(Vec<String>, Vec<&'a Expr>), EluEtlError> {
    let eqs = join.on.as_eq_conjunction().ok_or_else(|| {
        EluEtlError::Etl(format!(
            "第 {} 行：JOIN ON 僅支援等值比較（&& 串接的 ==），不支援 OR / NOT / IN / LIKE / BETWEEN / IS EMPTY",
            join.binding.line
        ))
    })?;
    let alias = join.binding.alias.to_lowercase();
    let mut keys = Vec::new();
    let mut probes = Vec::new();
    for c in eqs {
        let left_is_self = matches!(&c.left, Expr::Col(r) if r.alias.eq_ignore_ascii_case(&alias));
        let right_is_self =
            matches!(&c.right, Expr::Col(r) if r.alias.eq_ignore_ascii_case(&alias));
        match (left_is_self, right_is_self) {
            (true, false) => {
                let Expr::Col(r) = &c.left else { unreachable!() };
                keys.push(r.column.clone());
                probes.push(&c.right);
            }
            (false, true) => {
                let Expr::Col(r) = &c.right else { unreachable!() };
                keys.push(r.column.clone());
                probes.push(&c.left);
            }
            _ => {
                return Err(EluEtlError::Etl(format!(
                    "第 {} 行：JOIN ON 的每個等式需一邊是查表 [{}] 的欄位、另一邊是來源值",
                    c.line, join.binding.alias
                )))
            }
        }
    }
    if keys.is_empty() {
        return Err(EluEtlError::Etl(format!(
            "第 {} 行：JOIN [{}] 缺少 ON 條件",
            join.binding.line, join.binding.alias
        )));
    }
    Ok((keys, probes))
}

/// 收集某別名在整個 work 中被引用的欄位（供 JOIN 的值欄位 SELECT）。
fn referenced_columns(work: &Work, alias_lower: &str) -> Vec<String> {
    let mut cols: Vec<String> = Vec::new();
    let mut push = |c: &crate::etl::script::ast::ColRef| {
        if c.alias.eq_ignore_ascii_case(alias_lower)
            && !cols.iter().any(|x| x.eq_ignore_ascii_case(&c.column))
        {
            cols.push(c.column.clone());
        }
    };
    for j in &work.joins {
        for r in j.on.col_refs() {
            push(r);
        }
    }
    if let Some(w) = &work.where_ {
        for r in w.col_refs() {
            push(r);
        }
    }
    if let Some(m) = &work.merge {
        for r in m.on.col_refs() {
            push(r);
        }
    }
    for a in work.actions() {
        for asn in a.assignments() {
            for r in asn.value.col_refs() {
                push(r);
            }
        }
    }
    cols
}

/// 本 work 的寫入動作計畫。
struct WorkPlan<'a> {
    /// NOT MATCHED → ADD，或無 merge 的頂層 ADD
    insert: Option<&'a [Assignment]>,
    /// MATCHED → UPDATE 的 SET 指派
    update: Option<&'a [Assignment]>,
    /// MATCHED → DELETE
    delete: bool,
}

/// 解析 work 的動作：merge 分支（MATCHED：SKIP/UPDATE/DELETE；NOT MATCHED：ADD/SKIP）
/// 或無 merge 的頂層動作（ADD）。UPDATE/DELETE 需 merge ON 比對鍵。
fn plan_work(work: &Work) -> Result<WorkPlan<'_>, EluEtlError> {
    match &work.merge {
        Some(m) => {
            let insert = match &m.not_matched {
                Some(Action::Add(a)) => Some(a.as_slice()),
                Some(Action::Skip) | None => None,
                Some(Action::Update(_)) => {
                    return Err(EluEtlError::Etl(format!(
                        "第 {} 行：NOT MATCHED 不支援 UPDATE（目標尚無此列可更新）",
                        work.line
                    )))
                }
                Some(Action::Delete) => {
                    return Err(EluEtlError::Etl(format!(
                        "第 {} 行：NOT MATCHED 不支援 DELETE",
                        work.line
                    )))
                }
            };
            let (mut update, mut delete) = (None, false);
            match &m.matched {
                Some(Action::Update(a)) => update = Some(a.as_slice()),
                Some(Action::Delete) => delete = true,
                Some(Action::Add(_)) => {
                    return Err(EluEtlError::Etl(format!(
                        "第 {} 行：MATCHED 不支援 ADD（目標已存在）",
                        work.line
                    )))
                }
                Some(Action::Skip) | None => {}
            }
            Ok(WorkPlan {
                insert,
                update,
                delete,
            })
        }
        None => match &work.action {
            Some(Action::Add(a)) => Ok(WorkPlan {
                insert: Some(a),
                update: None,
                delete: false,
            }),
            Some(Action::Update(_) | Action::Delete) => Err(EluEtlError::Etl(format!(
                "第 {} 行：UPDATE / DELETE 需搭配頂層 ON 比對鍵（合併模式）",
                work.line
            ))),
            Some(Action::Skip) | None => Ok(WorkPlan {
                insert: None,
                update: None,
                delete: false,
            }),
        },
    }
}

/// 依目標型別逐欄轉換；失敗回 (欄索引, 原因)。
fn convert_row(cells: &[CellValue], types: &[DataType]) -> Result<Vec<CellValue>, (usize, String)> {
    let mut out = Vec::with_capacity(cells.len());
    for (i, (c, ty)) in cells.iter().zip(types).enumerate() {
        match c.convert_to(*ty) {
            Ok(v) => out.push(v),
            Err(reason) => return Err((i, reason)),
        }
    }
    Ok(out)
}

/// 執行整份腳本。回傳彙總摘要（total = 來源行數；success = 寫入總行數）。
pub async fn run<F>(
    driver: Arc<dyn DbDriver>,
    dialect: Dialect,
    job: ResolvedScriptJob,
    script: Script,
    emit: F,
    cancel: CancellationToken,
) -> Result<EtlSummary, EluEtlError>
where
    F: Fn(EtlProgress) + Send + Sync,
{
    let started = Instant::now();

    // 讀取來源（檔案或資料庫查詢）
    emit(progress(&job, "read", 0, 0, 0, 0));
    let data = match &job.source {
        ScriptSource::File {
            path,
            sheet,
            has_header,
            encoding,
        } => source_input::read_file(path, sheet, encoding.as_deref(), *has_header).await?,
        ScriptSource::Database {
            driver: src_driver,
            query,
        } => source_input::read_database(src_driver.clone(), query).await?,
    };
    let (header, rows, first_data_row) = (data.header, data.rows, data.first_data_row);

    let col_index: HashMap<String, usize> = header
        .iter()
        .enumerate()
        .map(|(i, name)| (name.to_lowercase(), i))
        .collect();

    let total_rows = rows.len() as u64;
    let gen_ctx = gen::GenContext::new();
    let batch_size = job.batch_size.max(1);
    let total_batches = rows.len().div_ceil(batch_size).max(1) * script.works.len();

    let mut success_rows: u64 = 0;
    let mut error_rows: u64 = 0;
    let mut errors: Vec<RowError> = Vec::new();
    let mut done_batches = 0usize;

    for work in &script.works {
        if cancel.is_cancelled() {
            return Ok(summary(
                &job, "cancelled", total_rows, success_rows, error_rows, started, None, errors,
            ));
        }

        let from_alias = work.from.alias.to_lowercase();

        // ---- 動作計畫（INSERT / UPDATE / DELETE） ----
        let plan = plan_work(work)?;
        if plan.insert.is_none() && plan.update.is_none() && !plan.delete {
            // 無任何寫入動作（純 SKIP）→ 推進進度後跳過
            done_batches += rows.len().div_ceil(batch_size).max(1);
            emit(progress(
                &job, "load", done_batches.min(total_batches), total_batches, success_rows,
                error_rows,
            ));
            continue;
        }
        if matches!(plan.insert, Some(a) if a.is_empty()) {
            return Err(EluEtlError::Etl(format!(
                "第 {} 行：作業「{}」的 ADD 沒有任何欄位指派，至少需要一個",
                work.line,
                work.name.as_deref().unwrap_or("-")
            )));
        }
        if matches!(plan.update, Some(a) if a.is_empty()) {
            return Err(EluEtlError::Etl(format!(
                "第 {} 行：作業「{}」的 UPDATE 沒有任何欄位指派，至少需要一個",
                work.line,
                work.name.as_deref().unwrap_or("-")
            )));
        }

        // ---- 目標表與欄位型別 ----
        let target = table_ref_to_string(&work.into.table, dialect)?;
        let target_cols = driver.get_columns(&target).await.map_err(|e| {
            EluEtlError::Etl(format!("第 {} 行：無法讀取目標表 {target}：{e}", work.into.line))
        })?;
        let target_types: HashMap<String, DataType> = target_cols
            .iter()
            .map(|c| (c.name.to_lowercase(), DataType::from_db_type(&c.db_type)))
            .collect();

        // INSERT / UPDATE 的欄位與目標型別
        let mut insert_cols: Vec<String> = Vec::new();
        let mut insert_types: Vec<DataType> = Vec::new();
        let mut update_cols: Vec<String> = Vec::new();
        let mut update_types: Vec<DataType> = Vec::new();
        for (asg, cols, types) in [
            (plan.insert, &mut insert_cols, &mut insert_types),
            (plan.update, &mut update_cols, &mut update_types),
        ] {
            if let Some(xs) = asg {
                for x in xs {
                    let ty = target_types
                        .get(&x.target_column.to_lowercase())
                        .copied()
                        .ok_or_else(|| {
                            EluEtlError::Etl(format!(
                                "第 {} 行：目標表 {target} 沒有欄位 {}",
                                x.line, x.target_column
                            ))
                        })?;
                    cols.push(x.target_column.clone());
                    types.push(ty);
                }
            }
        }

        // ---- 載入查表（依序，後者可引用前者） ----
        emit(progress(
            &job, "lookup", done_batches, total_batches, success_rows, error_rows,
        ));
        let mut join_metas: Vec<JoinMeta> = Vec::new();
        let mut joins_rt: Vec<JoinRuntime> = Vec::new();
        for join in &work.joins {
            let jdriver = join_driver(join.binding.conn, &job.source, &driver)?;
            let (key_cols, probe_exprs) = split_join_keys(join)?;
            let value_cols = referenced_columns(work, &join.binding.alias.to_lowercase());

            // SELECT 鍵欄 + 值欄（去重，鍵欄在前）
            let mut select_cols = key_cols.clone();
            for c in &value_cols {
                if !select_cols.iter().any(|s| s.eq_ignore_ascii_case(c)) {
                    select_cols.push(c.clone());
                }
            }
            let lookup_table = table_ref_to_string(&join.binding.table, dialect)?;
            let sql = format!(
                "SELECT {} FROM {}",
                quote_columns(dialect, &select_cols)?,
                quote_table(dialect, &lookup_table)?
            );
            let result = jdriver.query_all(&sql, None).await.map_err(|e| {
                EluEtlError::Etl(format!(
                    "第 {} 行：讀取查表 {lookup_table} 失敗：{e}",
                    join.binding.line
                ))
            })?;

            let key_n = key_cols.len();
            let mut map: HashMap<String, Vec<CellValue>> =
                HashMap::with_capacity(result.rows.len());
            let mut dup = 0usize;
            for mut row in result.rows {
                let values = row.split_off(key_n); // 值欄位（鍵欄之後）
                if let Some(key) = compose_key(&row[..key_n.min(row.len())]) {
                    if map.insert(key, values).is_some() {
                        dup += 1;
                    }
                }
            }
            if dup > 0 {
                tracing::warn!(table = %lookup_table, duplicates = dup, "查表鍵值重複，以最後一筆為準");
            }

            // probe 繫結（以「目前為止」的別名為環境：from + 已載入的 joins）
            let probe_ctx = BindCtx {
                from_alias: &from_alias,
                col_index: &col_index,
                header: &header,
                joins: &join_metas,
            };
            let probes = probe_exprs
                .iter()
                .map(|e| build_valbind(e, &probe_ctx))
                .collect::<Result<Vec<_>, _>>()?;

            // value_cols → 位置對照（給後續取值繫結）
            let col_pos: HashMap<String, usize> = value_cols
                .iter()
                .enumerate()
                .map(|(i, c)| (c.to_lowercase(), i))
                .collect();
            join_metas.push(JoinMeta {
                alias_lower: join.binding.alias.to_lowercase(),
                col_pos,
            });
            joins_rt.push(JoinRuntime {
                alias: join.binding.alias.clone(),
                policy: join.policy,
                line: join.binding.line,
                probes,
                map,
            });
        }

        // ---- 繫結環境（全部別名已知） ----
        let bind_ctx = BindCtx {
            from_alias: &from_alias,
            col_index: &col_index,
            header: &header,
            joins: &join_metas,
        };
        let bind_all = |asg: Option<&[Assignment]>| -> Result<Vec<ValBind>, EluEtlError> {
            match asg {
                Some(xs) => xs.iter().map(|a| build_valbind(&a.value, &bind_ctx)).collect(),
                None => Ok(Vec::new()),
            }
        };
        let insert_binds = bind_all(plan.insert)?;
        let update_binds = bind_all(plan.update)?;
        let where_plan = match &work.where_ {
            Some(c) => Some(build_cond_plan(c, &bind_ctx)?),
            None => None,
        };

        // ---- merge：拆鍵 + 預載目標既有鍵集合 ----
        let merge_plan = match &work.merge {
            Some(m) => Some(
                build_merge_plan(m, &bind_ctx, &target, dialect, &driver, work.into.line).await?,
            ),
            None => None,
        };
        // merge 比對鍵欄的目標型別（UPDATE/DELETE 的 WHERE 綁定用）
        let key_types: Vec<DataType> = match &merge_plan {
            Some(mp) => mp
                .key_cols
                .iter()
                .map(|c| {
                    target_types.get(&c.to_lowercase()).copied().ok_or_else(|| {
                        EluEtlError::Etl(format!(
                            "第 {} 行：目標表 {target} 沒有比對鍵欄位 {c}",
                            work.into.line
                        ))
                    })
                })
                .collect::<Result<_, _>>()?,
            None => Vec::new(),
        };

        // ---- 行迴圈：查表 + 過濾 + merge 分支 + 組裝 + 型別轉換 ----
        emit(progress(
            &job, "transform", done_batches, total_batches, success_rows, error_rows,
        ));
        let mut out_rows: Vec<Vec<CellValue>> = Vec::new();
        let mut update_rows: Vec<Vec<CellValue>> = Vec::new();
        let mut delete_rows: Vec<Vec<CellValue>> = Vec::new();
        // 來源端去重（§5.3）：同批已插入的 merge 鍵，後續同鍵列略過
        let mut seen: HashSet<String> = HashSet::new();
        'row: for (i, row) in rows.iter().enumerate() {
            let row_no = first_data_row + i;

            // 解析查表（依序），inner 未命中 → 錯誤報告；left → NULL
            let mut resolved: Vec<Option<&Vec<CellValue>>> = Vec::with_capacity(joins_rt.len());
            for jrt in &joins_rt {
                let key_cells: Vec<CellValue> = jrt
                    .probes
                    .iter()
                    .map(|p| eval_valbind(p, row, &resolved, &gen_ctx))
                    .collect();
                let hit = compose_key(&key_cells).and_then(|k| jrt.map.get(&k));
                match (hit, jrt.policy) {
                    (Some(vals), _) => resolved.push(Some(vals)),
                    (None, JoinPolicy::Left) => resolved.push(None),
                    (None, JoinPolicy::Inner) => {
                        error_rows += 1;
                        let key_disp = key_cells
                            .iter()
                            .map(|c| c.to_display_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                        push_error(
                            &mut errors,
                            row_no,
                            &jrt.alias,
                            format!("查表 [{}] 查無對應：{key_disp}", jrt.alias),
                        );
                        let _ = jrt.line;
                        continue 'row;
                    }
                }
            }

            // WHERE 過濾（未過關 → 略過，不算錯誤）
            if let Some(wp) = &where_plan {
                if !eval_cond_plan(wp, row, &resolved, &gen_ctx) {
                    continue 'row;
                }
            }

            // merge：判斷目標是否已存在，命中→MATCHED（UPDATE/DELETE/SKIP），未命中→NOT MATCHED（ADD/SKIP）
            if let Some(mp) = &merge_plan {
                let key_cells: Vec<CellValue> = mp
                    .key_src
                    .iter()
                    .map(|b| eval_valbind(b, row, &resolved, &gen_ctx))
                    .collect();
                let key = compose_key(&key_cells);
                let matched = key.as_ref().map(|k| mp.existing.contains(k)).unwrap_or(false);

                if matched {
                    if !update_binds.is_empty() {
                        // MATCHED → UPDATE：SET 值 ++ WHERE 鍵值
                        let mut setv = Vec::with_capacity(update_binds.len());
                        let mut bad = false;
                        for (b, (col, ty)) in
                            update_binds.iter().zip(update_cols.iter().zip(&update_types))
                        {
                            match eval_valbind(b, row, &resolved, &gen_ctx).convert_to(*ty) {
                                Ok(v) => setv.push(v),
                                Err(reason) => {
                                    error_rows += 1;
                                    push_error(&mut errors, row_no, col, reason);
                                    bad = true;
                                    break;
                                }
                            }
                        }
                        if bad {
                            continue 'row;
                        }
                        match convert_row(&key_cells, &key_types) {
                            Ok(kv) => {
                                setv.extend(kv);
                                update_rows.push(setv);
                            }
                            Err((idx, reason)) => {
                                error_rows += 1;
                                push_error(&mut errors, row_no, &mp.key_cols[idx], reason);
                            }
                        }
                    } else if plan.delete {
                        // MATCHED → DELETE：WHERE 鍵值
                        match convert_row(&key_cells, &key_types) {
                            Ok(kv) => delete_rows.push(kv),
                            Err((idx, reason)) => {
                                error_rows += 1;
                                push_error(&mut errors, row_no, &mp.key_cols[idx], reason);
                            }
                        }
                    }
                    // MATCHED 且非 UPDATE/DELETE → SKIP
                    continue 'row;
                }

                // NOT MATCHED
                if plan.insert.is_none() {
                    continue 'row;
                }
                // 來源端去重：同批已見過同鍵 → 略過（鍵含 NULL 段時不去重）
                if let Some(k) = key {
                    if !seen.insert(k) {
                        continue 'row;
                    }
                }
            } else if plan.insert.is_none() {
                continue 'row;
            }

            // 組裝 INSERT 列
            let mut out = Vec::with_capacity(insert_binds.len());
            let mut bad = false;
            for (b, (col, ty)) in insert_binds.iter().zip(insert_cols.iter().zip(&insert_types)) {
                match eval_valbind(b, row, &resolved, &gen_ctx).convert_to(*ty) {
                    Ok(v) => out.push(v),
                    Err(reason) => {
                        error_rows += 1;
                        push_error(&mut errors, row_no, col, reason);
                        bad = true;
                        break;
                    }
                }
            }
            if bad {
                continue 'row;
            }
            out_rows.push(out);
        }

        // ---- 批次寫入（INSERT） ----
        for chunk in out_rows.chunks(batch_size) {
            if cancel.is_cancelled() {
                return Ok(summary(
                    &job, "cancelled", total_rows, success_rows, error_rows, started, None, errors,
                ));
            }
            emit(progress(
                &job, "load", done_batches, total_batches, success_rows, error_rows,
            ));
            match driver.write_batch(&target, &insert_cols, &insert_types, chunk).await {
                Ok(n) => success_rows += n,
                Err(e) => {
                    return Ok(summary(
                        &job, "failed", total_rows, success_rows, error_rows, started,
                        Some(format!("寫入 {target} 失敗：{e}")), errors,
                    ));
                }
            }
            done_batches += 1;
            emit(progress(
                &job, "load", done_batches, total_batches, success_rows, error_rows,
            ));
        }

        // ---- MATCHED → UPDATE ----
        if !update_rows.is_empty() {
            let mp = merge_plan.as_ref().expect("UPDATE 僅在 merge 下產生");
            let (set_clause, next) = equality_clause(dialect, &update_cols, 1, ", ")?;
            let (where_clause, _) = equality_clause(dialect, &mp.key_cols, next, " AND ")?;
            let sql = format!(
                "UPDATE {} SET {} WHERE {}",
                quote_table(dialect, &target)?,
                set_clause,
                where_clause
            );
            let mut ptypes = update_types.clone();
            ptypes.extend(key_types.iter().copied());
            match driver.execute_batch(&sql, &ptypes, &update_rows).await {
                Ok(n) => success_rows += n,
                Err(e) => {
                    return Ok(summary(
                        &job, "failed", total_rows, success_rows, error_rows, started,
                        Some(format!("更新 {target} 失敗：{e}")), errors,
                    ));
                }
            }
        }

        // ---- MATCHED → DELETE ----
        if !delete_rows.is_empty() {
            let mp = merge_plan.as_ref().expect("DELETE 僅在 merge 下產生");
            let (where_clause, _) = equality_clause(dialect, &mp.key_cols, 1, " AND ")?;
            let sql = format!(
                "DELETE FROM {} WHERE {}",
                quote_table(dialect, &target)?,
                where_clause
            );
            match driver.execute_batch(&sql, &key_types, &delete_rows).await {
                Ok(n) => success_rows += n,
                Err(e) => {
                    return Ok(summary(
                        &job, "failed", total_rows, success_rows, error_rows, started,
                        Some(format!("刪除 {target} 失敗：{e}")), errors,
                    ));
                }
            }
        }

        if out_rows.is_empty() {
            done_batches += rows.len().div_ceil(batch_size).max(1);
            emit(progress(
                &job, "load", done_batches.min(total_batches), total_batches, success_rows,
                error_rows,
            ));
        }

        tracing::info!(
            target: "audit",
            job_id = %job.job_id,
            work = work.name.as_deref().unwrap_or("-"),
            statement_line = work.line,
            table = %target,
            inserted = out_rows.len(),
            updated = update_rows.len(),
            deleted = delete_rows.len(),
            "工作項目完成"
        );
    }

    Ok(summary(
        &job, "completed", total_rows, success_rows, error_rows, started, None, errors,
    ))
}

/// merge 執行計畫：目標鍵欄 + 每列來源鍵取值 + 目標既有鍵集合。
struct MergePlan {
    /// 目標鍵欄（INTO 別名側欄位，順序對齊 key_src）
    key_cols: Vec<String>,
    /// 每列來源鍵取值（literal 或 source/join 欄位）
    key_src: Vec<ValBind>,
    existing: HashSet<String>,
}

async fn build_merge_plan(
    merge: &crate::etl::script::ast::Merge,
    ctx: &BindCtx<'_>,
    target: &str,
    dialect: Dialect,
    driver: &Arc<dyn DbDriver>,
    line: usize,
) -> Result<MergePlan, EluEtlError> {
    let eqs = merge.on.as_eq_conjunction().ok_or_else(|| {
        EluEtlError::Etl(format!(
            "第 {line} 行：merge ON 僅支援等值比較（&& 串接的 ==），不支援 OR / NOT / IN / LIKE / BETWEEN / IS EMPTY"
        ))
    })?;
    let into_alias = into_alias_from_eqs(&eqs, ctx)?;
    let mut key_cols = Vec::new();
    let mut key_src = Vec::new();
    for c in eqs {
        let left_is_target =
            matches!(&c.left, Expr::Col(r) if r.alias.eq_ignore_ascii_case(&into_alias));
        let right_is_target =
            matches!(&c.right, Expr::Col(r) if r.alias.eq_ignore_ascii_case(&into_alias));
        let (target_col, src_expr) = match (left_is_target, right_is_target) {
            (true, false) => {
                let Expr::Col(r) = &c.left else { unreachable!() };
                (r.column.clone(), &c.right)
            }
            (false, true) => {
                let Expr::Col(r) = &c.right else { unreachable!() };
                (r.column.clone(), &c.left)
            }
            _ => {
                return Err(EluEtlError::Etl(format!(
                    "第 {} 行：merge ON 每個等式需一邊是目標欄位（INTO 別名）、另一邊是來源值",
                    c.line
                )))
            }
        };
        key_cols.push(target_col);
        key_src.push(build_valbind(src_expr, ctx)?);
    }
    if key_cols.is_empty() {
        return Err(EluEtlError::Etl(format!("第 {line} 行：merge ON 缺少比對鍵")));
    }

    // 預載目標既有鍵集合（避免每列 EXISTS 的 N+1）
    let sql = format!(
        "SELECT DISTINCT {} FROM {}",
        quote_columns(dialect, &key_cols)?,
        quote_table(dialect, target)?
    );
    let result = driver.query_all(&sql, None).await.map_err(|e| {
        EluEtlError::Etl(format!("第 {line} 行：預載目標鍵集合失敗：{e}"))
    })?;
    let key_n = key_cols.len();
    let mut existing = HashSet::with_capacity(result.rows.len());
    for row in result.rows {
        if let Some(k) = compose_key(&row[..key_n.min(row.len())]) {
            existing.insert(k);
        }
    }

    Ok(MergePlan {
        key_cols,
        key_src,
        existing,
    })
}

/// merge ON 等式中的 INTO 別名 = 既非 from、亦非任何 join 的別名（即目標側）。
fn into_alias_from_eqs(
    eqs: &[&crate::etl::script::ast::Comparison],
    ctx: &BindCtx<'_>,
) -> Result<String, EluEtlError> {
    for c in eqs {
        for e in [&c.left, &c.right] {
            if let Expr::Col(r) = e {
                let a = r.alias.to_lowercase();
                let is_source =
                    a == ctx.from_alias || ctx.joins.iter().any(|m| m.alias_lower == a);
                if !is_source {
                    return Ok(r.alias.clone());
                }
            }
        }
    }
    Err(EluEtlError::Etl(
        "merge ON 找不到目標欄位（INTO 別名）；請以 別名.[欄位] 比對目標".into(),
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
    job: &ResolvedScriptJob,
    phase: &str,
    batch: usize,
    total_batches: usize,
    success_rows: u64,
    error_rows: u64,
) -> EtlProgress {
    EtlProgress {
        job_id: job.job_id,
        phase: phase.to_string(),
        batch,
        total_batches,
        success_rows,
        error_rows,
    }
}

#[allow(clippy::too_many_arguments)]
fn summary(
    job: &ResolvedScriptJob,
    status: &str,
    total_rows: u64,
    success_rows: u64,
    error_rows: u64,
    started: Instant,
    failure: Option<String>,
    errors: Vec<RowError>,
) -> EtlSummary {
    EtlSummary {
        job_id: job.job_id,
        status: status.to_string(),
        total_rows,
        success_rows,
        error_rows,
        elapsed_ms: started.elapsed().as_millis() as u64,
        failure,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::like_match;

    #[test]
    fn like_wildcards_case_insensitive() {
        assert!(like_match("hello", "h%o"));
        assert!(like_match("hello", "%ell%"));
        assert!(like_match("hello", "h_llo"));
        assert!(like_match("Hello", "h%")); // 不分大小寫
        assert!(like_match("abc", "abc"));
        assert!(like_match("abc", "%"));
        assert!(like_match("abc", "a%c"));
        assert!(!like_match("hello", "h_o"));
        assert!(!like_match("abc", "abcd"));
        assert!(!like_match("abc", "b%"));
    }
}
