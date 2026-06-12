use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::db::driver::DbDriver;
use crate::models::connection::ConnectionConfig;
use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};
use crate::security::secrets::SecretString;

/// SQL Server 驅動（tiberius，TDS over rustls）。
///
/// 目前每次操作建立新連線；Week 2 後段改接 deadpool 連線池。
pub struct MssqlDriver {
    config: ConnectionConfig,
    password: SecretString,
}

impl MssqlDriver {
    pub fn new(config: ConnectionConfig, password: SecretString) -> Self {
        Self { config, password }
    }

    fn tiberius_config(&self) -> Config {
        let mut c = Config::new();
        c.host(&self.config.host);
        c.port(self.config.port_or_default());
        c.database(&self.config.database);
        c.authentication(AuthMethod::sql_server(
            &self.config.username,
            self.password.expose(),
        ));
        if self.config.trust_server_certificate {
            // 安全政策：信任自簽憑證需使用者明確 opt-in，並記入審計日誌
            tracing::warn!(
                target: "audit",
                conn_id = %self.config.id,
                "trust_server_certificate 已啟用（自簽憑證）"
            );
            c.trust_cert();
        }
        c
    }

    async fn connect(&self) -> Result<Client<Compat<TcpStream>>, EluEtlError> {
        let cfg = self.tiberius_config();
        let tcp = TcpStream::connect(cfg.get_addr()).await?;
        tcp.set_nodelay(true)?;
        Ok(Client::connect(cfg, tcp.compat_write()).await?)
    }
}

#[async_trait::async_trait]
impl DbDriver for MssqlDriver {
    async fn test_connection(&self) -> Result<(), EluEtlError> {
        let mut client = self.connect().await?;
        client
            .simple_query("SELECT 1")
            .await?
            .into_results()
            .await?;
        Ok(())
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError> {
        Err(EluEtlError::NotImplemented("mssql::list_tables"))
    }

    async fn get_columns(&self, _table: &str) -> Result<Vec<ColumnInfo>, EluEtlError> {
        Err(EluEtlError::NotImplemented("mssql::get_columns"))
    }
}
