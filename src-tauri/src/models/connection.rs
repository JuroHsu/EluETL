use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 連線種類：四種資料庫 + 檔案來源（Excel / CSV）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbKind {
    SqlServer,
    Postgres,
    MySql,
    Sqlite,
    /// 檔案來源連線：`database` 為檔案路徑，搭配 `sheet` / `encoding` /
    /// `has_header`；僅可作為 ETL 來源，不可作為目標
    File,
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
    /// 檔案連線：工作表名稱（None = 第一個工作表 / CSV）
    #[serde(default)]
    pub sheet: Option<String>,
    /// 檔案連線：CSV 編碼覆寫（None = 自動偵測）
    #[serde(default)]
    pub encoding: Option<String>,
    /// 檔案連線：首列是否為欄名（None = true）
    #[serde(default)]
    pub has_header: Option<bool>,
}

impl ConnectionConfig {
    pub fn default_port(&self) -> u16 {
        match self.kind {
            DbKind::SqlServer => 1433,
            DbKind::Postgres => 5432,
            DbKind::MySql => 3306,
            DbKind::Sqlite | DbKind::File => 0,
        }
    }

    pub fn port_or_default(&self) -> u16 {
        self.port.unwrap_or_else(|| self.default_port())
    }
}
