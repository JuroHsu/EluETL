use serde::Serialize;
use tauri::ipc::Channel;
use uuid::Uuid;

use crate::db;
use crate::db::pool::AppState;
use crate::etl::executor::{self, EtlProgress, EtlSummary};
use crate::etl::mapping::EtlJobConfig;
use crate::etl::script::ast::{Expr, ScriptIssue};
use crate::etl::script::executor::ScriptJobParams;
use crate::etl::script::{self, parser};
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
    if let Some(cols) = &source_columns {
        let lower: Vec<String> = cols.iter().map(|c| c.to_lowercase()).collect();
        let mut check = |name: &str, line: usize| {
            if !lower.contains(&name.to_lowercase()) {
                issues.push(ScriptIssue {
                    line,
                    message: format!("來源檔沒有欄位 {name}（可用：{}）", cols.join(", ")),
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
                if let Expr::Col(r) = &a.value {
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

/// 執行 ETL 腳本（lookup join + 批次寫入；進度經 Channel 推送）。
#[tauri::command]
pub async fn execute_etl_script(
    state: tauri::State<'_, AppState>,
    params: ScriptJobParams,
    on_progress: Channel<EtlProgress>,
) -> Result<EtlSummary, EluEtlError> {
    let parsed = script::executor::run_blocking_parse(&params.script)?;
    let config = state.store()?.get_connection(&params.conn_id).await?;
    let dialect = db::dialect_for(config.kind);
    let driver = state.get_or_create_driver(params.conn_id).await?;

    let job_id = params.job_id;
    tracing::info!(
        target: "audit",
        job_id = %job_id,
        conn_id = %params.conn_id,
        statements = parsed.statements.len(),
        "腳本任務開始"
    );
    let cancel = state.register_job(job_id);
    let emit = move |p: EtlProgress| {
        let _ = on_progress.send(p);
    };
    let result = script::executor::run(driver, dialect, params, parsed, emit, cancel).await;
    state.finish_job(&job_id);
    result
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

    let path = job_row.config.source_path.clone();
    let current_sha = tokio::task::spawn_blocking(move || executor::file_sha256(&path)).await??;
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
