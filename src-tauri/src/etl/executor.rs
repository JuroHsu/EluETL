use std::time::Instant;

use serde::Serialize;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::pool::AppState;
use crate::etl::mapping::{ErrorPolicy, EtlJobConfig, SourceSpec, WriteMode};
use crate::etl::source_input;
use crate::etl::transform::{transform_rows, RowError};
use crate::models::errors::EluEtlError;
use crate::models::value::{CellValue, DataType};

/// 摘要中保留的錯誤明細上限（總數另計於 errorRows）。
const ERROR_DETAIL_CAP: usize = 1_000;

/// 進度事件（Tauri Channel 推送至前端）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EtlProgress {
    pub job_id: Uuid,
    pub phase: String,
    pub batch: usize,
    pub total_batches: usize,
    pub success_rows: u64,
    pub error_rows: u64,
}

/// 任務結果摘要。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EtlSummary {
    pub job_id: Uuid,
    pub status: String,
    pub total_rows: u64,
    pub success_rows: u64,
    pub error_rows: u64,
    pub elapsed_ms: u64,
    /// 失敗 / 中止原因（status = failed / aborted 時）
    pub failure: Option<String>,
    /// 錯誤明細（最多 1000 筆）
    pub errors: Vec<RowError>,
}

/// 執行期可變狀態（集中管理，避免散落的計數器）。
struct RunState {
    job_id: Uuid,
    total_rows: u64,
    success_rows: u64,
    error_rows: u64,
    started: Instant,
    errors: Vec<RowError>,
}

impl RunState {
    fn push_errors(&mut self, errs: &[RowError]) {
        self.error_rows += errs.len() as u64;
        let room = ERROR_DETAIL_CAP.saturating_sub(self.errors.len());
        self.errors.extend(errs.iter().take(room).cloned());
    }

    fn summary(mut self, status: &str, failure: Option<String>) -> EtlSummary {
        self.errors.truncate(ERROR_DETAIL_CAP);
        EtlSummary {
            job_id: self.job_id,
            status: status.to_string(),
            total_rows: self.total_rows,
            success_rows: self.success_rows,
            error_rows: self.error_rows,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            failure,
            errors: self.errors,
        }
    }

    fn progress(&self, phase: &str, batch: usize, total_batches: usize) -> EtlProgress {
        EtlProgress {
            job_id: self.job_id,
            phase: phase.to_string(),
            batch,
            total_batches,
            success_rows: self.success_rows,
            error_rows: self.error_rows,
        }
    }
}

/// ETL 主流程：讀取 → 轉換（rayon）→ 批次寫入 → checkpoint → 進度事件。
/// `skip_batches` > 0 時為續跑（自最後成功批次的下一批開始）。
pub async fn run<F>(
    state: &AppState,
    job: EtlJobConfig,
    emit: F,
    cancel: CancellationToken,
    skip_batches: usize,
) -> Result<EtlSummary, EluEtlError>
where
    F: Fn(EtlProgress) + Send + Sync,
{
    let driver = state.get_or_create_driver(job.conn_id).await?;
    let store = state.store()?;

    let mut rs = RunState {
        job_id: job.job_id,
        total_rows: 0,
        success_rows: 0,
        error_rows: 0,
        started: Instant::now(),
        errors: Vec::new(),
    };

    // 讀取來源（檔案經 spawn_blocking；資料庫經來源連線的驅動）
    emit(rs.progress("read", 0, 0));
    let data = match &job.source {
        SourceSpec::File {
            path,
            sheet,
            encoding,
            has_header,
        } => source_input::read_file(path, sheet, encoding.as_deref(), *has_header).await?,
        SourceSpec::Database { conn_id, query } => {
            let src_driver = state.get_or_create_driver(*conn_id).await?;
            source_input::read_database(src_driver, query).await?
        }
    };
    let sha = {
        let spec = job.source.clone();
        tokio::task::spawn_blocking(move || source_input::fingerprint(&spec)).await??
    };
    let mut rows = data.rows;
    rs.total_rows = rows.len() as u64;
    // 來源行號：1-based；檔案表頭佔第 1 行，資料庫自結果集第 1 行起
    let first_data_row = data.first_data_row;

    store.upsert_job(&job, &sha, "running").await?;
    tracing::info!(
        target: "audit",
        job_id = %job.job_id,
        table = %job.target_table,
        rows = rs.total_rows,
        resume_from = skip_batches,
        "ETL 任務開始"
    );

    let columns: Vec<String> = job.rules.iter().map(|r| r.target_column.clone()).collect();
    let types: Vec<DataType> = job.rules.iter().map(|r| r.target_type).collect();
    let batch_size = job.batch_size.max(1);
    let total_batches = rows.len().div_ceil(batch_size).max(1);

    match job.write_mode {
        // ---- 全有全無：整批轉換、單一交易寫入 ----
        WriteMode::AllOrNothing => {
            let (ok_rows, errs) = {
                let rules = job.rules.clone();
                let rows_owned = std::mem::take(&mut rows);
                tokio::task::spawn_blocking(move || {
                    transform_rows(&rows_owned, &rules, first_data_row)
                })
                .await?
            };
            rs.push_errors(&errs);

            let abort = match job.error_policy {
                ErrorPolicy::AbortOnFirst => rs.error_rows > 0,
                ErrorPolicy::AbortOnErrorRate { max_percent } => {
                    rs.total_rows > 0
                        && (rs.error_rows as f32 / rs.total_rows as f32) * 100.0 > max_percent
                }
                ErrorPolicy::SkipAndReport => false,
            };
            if abort {
                store.set_job_status(&job.job_id, "aborted").await?;
                return Ok(rs.summary(
                    "aborted",
                    Some("轉換錯誤超出政策容許，全有全無模式未寫入任何資料".into()),
                ));
            }
            if cancel.is_cancelled() {
                store.set_job_status(&job.job_id, "cancelled").await?;
                return Ok(rs.summary("cancelled", None));
            }

            emit(rs.progress("load", 0, 1));
            match driver
                .write_batch(&job.target_table, &columns, &types, &ok_rows)
                .await
            {
                Ok(n) => {
                    rs.success_rows = n;
                    store
                        .update_job_progress(
                            &job.job_id,
                            1,
                            rs.success_rows as i64,
                            rs.error_rows as i64,
                        )
                        .await?;
                    store.set_job_status(&job.job_id, "completed").await?;
                }
                Err(e) => {
                    store.set_job_status(&job.job_id, "failed").await?;
                    return Ok(rs.summary("failed", Some(e.to_string())));
                }
            }
            emit(rs.progress("load", 1, 1));
        }

        // ---- 批次提交：每批一個交易 + checkpoint（可續跑）----
        WriteMode::BatchCommit => {
            for (bi, chunk) in rows.chunks(batch_size).enumerate() {
                if bi < skip_batches {
                    continue;
                }
                if cancel.is_cancelled() {
                    store.set_job_status(&job.job_id, "cancelled").await?;
                    tracing::info!(target: "audit", job_id = %job.job_id, batch = bi, "ETL 任務已取消");
                    return Ok(rs.summary("cancelled", None));
                }

                emit(rs.progress("transform", bi, total_batches));
                let (ok_rows, errs) = {
                    let rules = job.rules.clone();
                    let chunk_owned: Vec<Vec<CellValue>> = chunk.to_vec();
                    let offset = first_data_row + bi * batch_size;
                    tokio::task::spawn_blocking(move || {
                        transform_rows(&chunk_owned, &rules, offset)
                    })
                    .await?
                };
                rs.push_errors(&errs);

                let abort_reason = match job.error_policy {
                    ErrorPolicy::AbortOnFirst if !errs.is_empty() => {
                        Some("首錯即停：偵測到轉換錯誤".to_string())
                    }
                    ErrorPolicy::AbortOnErrorRate { max_percent } => {
                        let processed = (rs.success_rows + rs.error_rows).max(1);
                        let rate = (rs.error_rows as f32 / processed as f32) * 100.0;
                        (rate > max_percent)
                            .then(|| format!("錯誤率 {rate:.1}% 超過上限 {max_percent}%"))
                    }
                    _ => None,
                };
                if let Some(reason) = abort_reason {
                    store.set_job_status(&job.job_id, "aborted").await?;
                    return Ok(rs.summary("aborted", Some(reason)));
                }

                emit(rs.progress("load", bi, total_batches));
                if !ok_rows.is_empty() {
                    match driver
                        .write_batch(&job.target_table, &columns, &types, &ok_rows)
                        .await
                    {
                        Ok(n) => rs.success_rows += n,
                        Err(e) => {
                            store.set_job_status(&job.job_id, "failed").await?;
                            return Ok(rs.summary("failed", Some(e.to_string())));
                        }
                    }
                }
                // 先 commit、後記 checkpoint（§4.4 冪等性界限）
                store
                    .update_job_progress(
                        &job.job_id,
                        (bi + 1) as i64,
                        rs.success_rows as i64,
                        rs.error_rows as i64,
                    )
                    .await?;
                emit(rs.progress("load", bi + 1, total_batches));
            }
            store.set_job_status(&job.job_id, "completed").await?;
        }
    }

    tracing::info!(
        target: "audit",
        job_id = %job.job_id,
        success = rs.success_rows,
        errors = rs.error_rows,
        elapsed_ms = rs.started.elapsed().as_millis() as u64,
        "ETL 任務完成"
    );
    Ok(rs.summary("completed", None))
}
