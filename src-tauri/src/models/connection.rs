use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 支援的資料庫種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbKind {
    SqlServer,
    Postgres,
    MySql,
    Sqlite,
}

/// 連線設定（不含密碼）。
///
/// 密碼一律走 OS keychain（Week 2 `security::secrets` 接 keyring），
/// 或在測試連線時由前端以 `SecretString` 暫時傳入，不落地、不記錄。
/// SQLite 時 `database` 為檔案路徑，`host` / `port` / `username` 不使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    pub id: Uuid,
    pub name: String,
    pub kind: DbKind,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    pub database: String,
    #[serde(default)]
    pub username: String,
    /// 信任自簽憑證（僅 SQL Server）。安全政策：預設 false，
    /// 開啟需使用者明確勾選，且必須記入審計日誌。
    #[serde(default)]
    pub trust_server_certificate: bool,
}

impl ConnectionConfig {
    pub fn default_port(&self) -> u16 {
        match self.kind {
            DbKind::SqlServer => 1433,
            DbKind::Postgres => 5432,
            DbKind::MySql => 3306,
            DbKind::Sqlite => 0,
        }
    }

    pub fn port_or_default(&self) -> u16 {
        self.port.unwrap_or_else(|| self.default_port())
    }
}
