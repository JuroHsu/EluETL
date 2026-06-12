use tauri::ipc::Channel;
use uuid::Uuid;

use crate::db::pool::AppState;
use crate::etl::executor::{self, EtlProgress, EtlSummary};
use crate::etl::mapping::EtlJobConfig;
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
