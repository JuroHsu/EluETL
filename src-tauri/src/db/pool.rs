use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::db::driver::DbDriver;
use crate::models::errors::EluEtlError;

/// 全域應用狀態：以 ConnectionId 快取驅動實例（內含連線池），
/// 跨 command 重用。禁止在 command 內臨時建池（見開發計畫 §2.2.1）。
#[derive(Default)]
pub struct AppState {
    drivers: RwLock<HashMap<Uuid, Arc<dyn DbDriver>>>,
}

impl AppState {
    pub async fn insert_driver(&self, id: Uuid, driver: Arc<dyn DbDriver>) {
        self.drivers.write().await.insert(id, driver);
    }

    pub async fn driver(&self, id: &Uuid) -> Result<Arc<dyn DbDriver>, EluEtlError> {
        self.drivers
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| EluEtlError::NotFound(format!("連線 {id} 不存在，請先測試連線")))
    }

    pub async fn remove_driver(&self, id: &Uuid) {
        self.drivers.write().await.remove(id);
    }
}
