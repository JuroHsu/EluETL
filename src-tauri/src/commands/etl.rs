use serde::Serialize;
use tauri::ipc::Channel;
use uuid::Uuid;

use crate::db;
use crate::db::pool::AppState;
use crate::etl::executor::{self, EtlProgress, EtlSummary};
use crate::etl::mapping::{EtlJobConfig, SourceSpec};
use crate::etl::script::ast::{ColRef, Expr, ScriptHeader, ScriptIssue, SourceDecl, Statement};
use crate::etl::script::executor::{ResolvedScriptJob, ScriptJobParams, ScriptSource};
use crate::etl::script::{self, parser};
use crate::etl::source_input;
use crate::models::connection::{ConnectionConfig, DbKind};
use crate::models::errors::EluEtlError;

/// 執行 ETL 任務（進度經 Channel 即時推送）。
#[tauri::command]
pub async fn execute_etl(
    state: tauri::State<'_, AppState>,
    job: EtlJobConfig,
    on_progress: Channel<EtlProgress>,
) -> Result<EtlSummary, EluEtlError> {
    let job_id = job.job_id;
    let cancel = state.register_job(job_id);
    let emit = move |p: EtlProgress| {
        let _ = on_progress.send(p);
    };
    let result = executor::run(state.inner(), job, emit, cancel, 0).await;
    state.finish_job(&job_id);
    result
}

/// 取消執行中的任務（協作式：當前批次完成或回滾後停止）。
#[tauri::command]
pub async fn cancel_etl(
    state: tauri::State<'_, AppState>,
    job_id: Uuid,
) -> Result<bool, EluEtlError> {
    let cancelled = state.cancel_job(&job_id);
    if cancelled {
        tracing::info!(target: "audit", job_id = %job_id, "收到取消請求");
    }
    Ok(cancelled)
}

/// 腳本驗證結果（編輯器診斷用）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptCheck {
    pub ok: bool,
    pub statement_count: usize,
    pub issues: Vec<ScriptIssue>,
}

/// 驗證 ETL 腳本：語法解析 + （若已載入來源檔）來源欄位存在性檢查。
/// 資料表 / 欄位的 DB 端驗證在執行時進行。
#[tauri::command]
pub fn validate_etl_script(script: String, source_columns: Option<Vec<String>>) -> ScriptCheck {
    let parsed = match parser::parse(&script) {
        Ok(p) => p,
        Err(issue) => {
            return ScriptCheck {
                ok: false,
                statement_count: 0,
                issues: vec![issue],
            }
        }
    };

    let mut issues = Vec::new();
    for stmt in &parsed.statements {
        if stmt.assignments.is_empty() {
            issues.push(ScriptIssue {
                line: stmt.line,
                message: format!(
                    "作業「{}」沒有任何欄位指派，至少需要一個",
                    stmt.name.as_deref().unwrap_or("-")
                ),
            });
        }
    }
    if let Some(cols) = &source_columns {
        let lower: Vec<String> = cols.iter().map(|c| c.to_lowercase()).collect();
        let mut check = |name: &str, line: usize| {
            if !lower.contains(&name.to_lowercase()) {
                issues.push(ScriptIssue {
                    line,
                    message: format!("來源沒有欄位 {name}（可用：{}）", cols.join(", ")),
                });
            }
        };
        for stmt in &parsed.statements {
            let (source_prefix, lookup_prefix) = match &stmt.condition {
                Some(c) => {
                    check(&c.left.column, c.line);
                    (c.left.prefix_key(), Some(c.right.prefix_key()))
                }
                None => (String::new(), None),
            };
            for a in &stmt.assignments {
                for r in a.value.col_refs() {
                    let key = r.prefix_key();
                    let is_lookup = lookup_prefix.as_deref() == Some(key.as_str());
                    if !is_lookup
                        && (key.is_empty() || key == source_prefix || stmt.condition.is_none())
                    {
                        check(&r.column, r.line);
                    }
                }
            }
        }
    }

    ScriptCheck {
        ok: issues.is_empty(),
        statement_count: parsed.statements.len(),
        issues,
    }
}

// ---- 結構化腳本模型（「遷移作業」頁的視覺化編輯器） ----

/// 欄位參照：prefix 為 0..3 段（[dbo].[Account].Id → prefix=["dbo","Account"]）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColRefModel {
    pub prefix: Vec<String>,
    pub column: String,
}

impl From<&ColRef> for ColRefModel {
    fn from(r: &ColRef) -> Self {
        ColRefModel {
            prefix: r.prefix.clone(),
            column: r.column.clone(),
        }
    }
}

/// 指派值（discriminated union，前端依 kind 分流）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ExprModel {
    Col {
        prefix: Vec<String>,
        column: String,
    },
    Text {
        value: String,
    },
    Int {
        value: i64,
    },
    Float {
        value: f64,
    },
    Bool {
        value: bool,
    },
    Null,
    /// 產生器；name 為正規名稱（如 "ULID"、"GUID(Text)"）
    Gen {
        name: String,
    },
    /// 合成欄位；expr 為正規 DSL 文字（如 `N'前綴:' + [SOURCE].[Name]`）
    Concat {
        expr: String,
    },
}

impl From<&Expr> for ExprModel {
    fn from(e: &Expr) -> Self {
        match e {
            Expr::Col(r) => ExprModel::Col {
                prefix: r.prefix.clone(),
                column: r.column.clone(),
            },
            Expr::Text(s) => ExprModel::Text { value: s.clone() },
            Expr::Int(v) => ExprModel::Int { value: *v },
            Expr::Float(v) => ExprModel::Float { value: *v },
            Expr::Bool(v) => ExprModel::Bool { value: *v },
            Expr::Null => ExprModel::Null,
            Expr::Gen(k) => ExprModel::Gen {
                name: k.label().to_string(),
            },
            Expr::Concat(_) => ExprModel::Concat { expr: e.to_dsl() },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConditionModel {
    pub left: ColRefModel,
    pub right: ColRefModel,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentModel {
    pub target_column: String,
    pub value: ExprModel,
}

/// 單一工作項目（WORK 區塊或舊式裸陳述式）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkModel {
    pub name: Option<String>,
    pub condition: Option<ConditionModel>,
    /// 目標資料表的各段（前端以 `.` 連接顯示）
    pub target_table: Vec<String>,
    pub assignments: Vec<AssignmentModel>,
}

impl From<&Statement> for WorkModel {
    fn from(s: &Statement) -> Self {
        WorkModel {
            name: s.name.clone(),
            condition: s.condition.as_ref().map(|c| ConditionModel {
                left: (&c.left).into(),
                right: (&c.right).into(),
            }),
            target_table: s.target_table.clone(),
            assignments: s
                .assignments
                .iter()
                .map(|a| AssignmentModel {
                    target_column: a.target_column.clone(),
                    value: (&a.value).into(),
                })
                .collect(),
        }
    }
}

/// SOURCE 標頭（round-trip 用：視覺模式重新產生腳本時保留）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum SourceModel {
    #[serde(rename_all = "camelCase")]
    File {
        path: String,
        sheet: Option<String>,
        encoding: Option<String>,
        has_header: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    Connection {
        name: String,
        table: Option<String>,
        query: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptModel {
    pub source: Option<SourceModel>,
    pub target_connection: Option<String>,
    pub works: Vec<WorkModel>,
}

/// 解析腳本為結構化模型，供「遷移作業」頁的三欄視覺編輯器使用。
#[tauri::command]
pub fn parse_etl_script(script: String) -> Result<ScriptModel, EluEtlError> {
    let parsed = script::executor::run_blocking_parse(&script)?;
    Ok(ScriptModel {
        source: parsed.header.source.as_ref().map(|s| match s {
            SourceDecl::File(f) => SourceModel::File {
                path: f.path.clone(),
                sheet: f.sheet.clone(),
                encoding: f.encoding.clone(),
                has_header: f.has_header,
            },
            SourceDecl::Connection(c) => SourceModel::Connection {
                name: c.name.clone(),
                table: c.table.clone(),
                query: c.query.clone(),
            },
        }),
        target_connection: parsed.header.target_connection.clone(),
        works: parsed.statements.iter().map(WorkModel::from).collect(),
    })
}

/// 工作表名稱解析：未指定時取第一個工作表（CSV 為 "CSV"）。
async fn resolve_sheet(path: &str, sheet: Option<String>) -> Result<String, EluEtlError> {
    match sheet {
        Some(s) => Ok(s),
        None => {
            let p = path.to_string();
            tokio::task::spawn_blocking(move || crate::excel::source::list_sheets(&p))
                .await??
                .into_iter()
                .next()
                .ok_or_else(|| EluEtlError::Excel("來源檔沒有工作表".into()))
        }
    }
}

/// 解析腳本標頭 + 工作區回退，產出目標連線與具體任務參數。
/// 優先序：腳本 SOURCE/TARGET > 工作區選擇（params 內的值）。
async fn resolve_script_job(
    state: &AppState,
    params: &ScriptJobParams,
    header: &ScriptHeader,
) -> Result<(ConnectionConfig, ResolvedScriptJob), EluEtlError> {
    let store = state.store()?;

    // 目標連線
    let target = match &header.target_connection {
        Some(name) => store.get_connection_by_name(name).await?,
        None => {
            let id = params.conn_id.ok_or_else(|| {
                EluEtlError::Config(
                    "未指定目標：請在腳本加入 TARGET = CONNECTION('連線名稱')，\
                     或於上方工具列選擇目標連線"
                        .into(),
                )
            })?;
            store.get_connection(&id).await?
        }
    };
    if target.kind == DbKind::File {
        return Err(EluEtlError::Config(format!(
            "「{}」是檔案連線，不能作為目標",
            target.name
        )));
    }

    // 來源：檔案（inline / 檔案連線）或資料庫連線（TABLE / QUERY）
    let source = match &header.source {
        Some(SourceDecl::File(f)) => ScriptSource::File {
            path: f.path.clone(),
            sheet: resolve_sheet(&f.path, f.sheet.clone()).await?,
            has_header: f.has_header.unwrap_or(true),
            encoding: f.encoding.clone(),
        },
        Some(SourceDecl::Connection(c)) => {
            let conn = store.get_connection_by_name(&c.name).await?;
            if conn.kind == DbKind::File {
                if c.table.is_some() || c.query.is_some() {
                    return Err(EluEtlError::Config(format!(
                        "「{}」是檔案連線，不支援 TABLE / QUERY 參數",
                        conn.name
                    )));
                }
                ScriptSource::File {
                    sheet: resolve_sheet(&conn.database, conn.sheet.clone()).await?,
                    path: conn.database,
                    has_header: conn.has_header.unwrap_or(true),
                    encoding: conn.encoding,
                }
            } else {
                let query = match (&c.table, &c.query) {
                    (Some(t), None) => format!(
                        "SELECT * FROM {}",
                        db::quote_table(db::dialect_for(conn.kind)?, t)?
                    ),
                    (None, Some(q)) => q.clone(),
                    _ => {
                        return Err(EluEtlError::Config(format!(
                            "資料庫來源「{}」需指定 TABLE='schema.table' 或 \
                             QUERY='SELECT ...'（擇一）",
                            conn.name
                        )))
                    }
                };
                ScriptSource::Database {
                    driver: state.get_or_create_driver(conn.id).await?,
                    query,
                }
            }
        }
        // 工作區回退：「匯入資料」載入的資料庫查詢優先，否則檔案路徑
        None => {
            if let Some(conn_id) = params.source_conn_id {
                let query = params.source_query.clone().ok_or_else(|| {
                    EluEtlError::Config(
                        "資料庫來源缺少查詢：請先於「匯入資料」選擇資料表或執行 SQL 預覽".into(),
                    )
                })?;
                ScriptSource::Database {
                    driver: state.get_or_create_driver(conn_id).await?,
                    query,
                }
            } else {
                let p = params.source_path.clone().ok_or_else(|| {
                    EluEtlError::Config(
                        "未指定來源：請在腳本加入 SOURCE = FILE(PATH='...') 或 \
                         SOURCE = CONNECTION('連線名稱')，或先於「匯入資料」載入來源"
                            .into(),
                    )
                })?;
                ScriptSource::File {
                    sheet: resolve_sheet(&p, params.sheet.clone()).await?,
                    path: p,
                    has_header: params.has_header.unwrap_or(true),
                    encoding: params.encoding.clone(),
                }
            }
        }
    };

    Ok((
        target,
        ResolvedScriptJob {
            job_id: params.job_id,
            source,
            batch_size: params.batch_size.max(1),
        },
    ))
}

/// 執行 ETL 腳本（lookup join + 批次寫入；進度經 Channel 推送）。
#[tauri::command]
pub async fn execute_etl_script(
    state: tauri::State<'_, AppState>,
    params: ScriptJobParams,
    on_progress: Channel<EtlProgress>,
) -> Result<EtlSummary, EluEtlError> {
    let parsed = script::executor::run_blocking_parse(&params.script)?;
    let (target, resolved) = resolve_script_job(&state, &params, &parsed.header).await?;
    let dialect = db::dialect_for(target.kind)?;
    let driver = state.get_or_create_driver(target.id).await?;

    let job_id = params.job_id;
    tracing::info!(
        target: "audit",
        job_id = %job_id,
        target_conn = %target.name,
        source = %resolved.source.label(),
        statements = parsed.statements.len(),
        "腳本任務開始"
    );
    let cancel = state.register_job(job_id);
    let emit = move |p: EtlProgress| {
        let _ = on_progress.send(p);
    };
    let result = script::executor::run(driver, dialect, resolved, parsed, emit, cancel).await;
    state.finish_job(&job_id);
    result
}

/// 讀取 .etl 腳本檔。
#[tauri::command]
pub async fn load_etl_file(path: String) -> Result<String, EluEtlError> {
    Ok(tokio::task::spawn_blocking(move || std::fs::read_to_string(path)).await??)
}

/// 儲存 .etl 腳本檔。
#[tauri::command]
pub async fn save_etl_file(path: String, content: String) -> Result<(), EluEtlError> {
    let log_path = path.clone();
    tokio::task::spawn_blocking(move || std::fs::write(&path, content)).await??;
    tracing::info!(target: "audit", path = %log_path, "已儲存 ETL 腳本");
    Ok(())
}

/// 續跑：驗證來源檔 SHA-256 未變更後，自最後成功批次的下一批開始。
/// 僅批次提交模式可續跑（開發計畫 §4.4）。
#[tauri::command]
pub async fn resume_etl(
    state: tauri::State<'_, AppState>,
    job_id: Uuid,
    on_progress: Channel<EtlProgress>,
) -> Result<EtlSummary, EluEtlError> {
    let job_row = state.store()?.get_job(&job_id).await?;
    if matches!(job_row.status.as_str(), "running" | "completed") {
        return Err(EluEtlError::Etl(format!(
            "任務狀態為 {}，無法續跑",
            job_row.status
        )));
    }

    if matches!(job_row.config.source, SourceSpec::Database { .. }) {
        return Err(EluEtlError::Etl(
            "資料庫來源無法保證資料列順序不變，不支援續跑；請重新執行任務".into(),
        ));
    }
    let spec = job_row.config.source.clone();
    let current_sha =
        tokio::task::spawn_blocking(move || source_input::fingerprint(&spec)).await??;
    if current_sha != job_row.source_sha256 {
        return Err(EluEtlError::Etl(
            "來源檔內容已變更，無法安全續跑；請重新執行任務".into(),
        ));
    }

    let cancel = state.register_job(job_id);
    let emit = move |p: EtlProgress| {
        let _ = on_progress.send(p);
    };
    let result = executor::run(
        state.inner(),
        job_row.config,
        emit,
        cancel,
        job_row.last_batch as usize,
    )
    .await;
    state.finish_job(&job_id);
    result
}
