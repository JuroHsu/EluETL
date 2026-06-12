use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{OnceCell, RwLock};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db;
use crate::db::driver::DbDriver;
use crate::models::connection::DbKind;
use crate::models::errors::EluEtlError;
use crate::security::{keychain, secrets::SecretString};
use crate::state::store::StateStore;

/// 全域應用狀態：
/// - 驅動實例（內含連線池）以 ConnectionId 快取，跨 command 重用
/// - 本地狀態庫（連線設定 / 任務 checkpoint）
/// - 執行中任務的取消權杖
#[derive(Default)]
pub struct AppState {
    drivers: RwLock<HashMap<Uuid, Arc<dyn DbDriver>>>,
    store: OnceCell<StateStore>,
    jobs: Mutex<HashMap<Uuid, CancellationToken>>,
}

impl AppState {
    // ---- 狀態庫 ----

    pub fn set_store(&self, store: StateStore) {
        let _ = self.store.set(store);
    }

    pub fn store(&self) -> Result<&StateStore, EluEtlError> {
        self.store
            .get()
            .ok_or_else(|| EluEtlError::Config("狀態庫尚未初始化".into()))
    }

    // ---- 驅動快取 ----

    pub async fn insert_driver(&self, id: Uuid, driver: Arc<dyn DbDriver>) {
        self.drivers.write().await.insert(id, driver);
    }

    pub async fn evict_driver(&self, id: &Uuid) {
        self.drivers.write().await.remove(id);
    }

    /// 取得驅動：快取命中直接回傳；否則從狀態庫載入設定、
    /// 自 OS keychain 取密碼（spawn_blocking），建立後快取。
    pub async fn get_or_create_driver(&self, id: Uuid) -> Result<Arc<dyn DbDriver>, EluEtlError> {
        if let Some(d) = self.drivers.read().await.get(&id) {
            return Ok(d.clone());
        }
        let config = self.store()?.get_connection(&id).await?;
        let password = if config.kind == DbKind::Sqlite {
            None
        } else {
            tokio::task::spawn_blocking(move || keychain::load_password(&id)).await??
        };
        let password = password.unwrap_or_else(|| SecretString::new(String::new()));
        let driver = db::create_driver(&config, &password);
        self.insert_driver(id, driver.clone()).await;
        Ok(driver)
    }

    // ---- 任務取消權杖 ----

    pub fn register_job(&self, job_id: Uuid) -> CancellationToken {
        let token = CancellationToken::new();
        self.jobs
            .lock()
            .expect("jobs lock poisoned")
            .insert(job_id, token.clone());
        token
    }

    pub fn cancel_job(&self, job_id: &Uuid) -> bool {
        match self.jobs.lock().expect("jobs lock poisoned").get(job_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    pub fn finish_job(&self, job_id: &Uuid) {
        self.jobs.lock().expect("jobs lock poisoned").remove(job_id);
    }
}
