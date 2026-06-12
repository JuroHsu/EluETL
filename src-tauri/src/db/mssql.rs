use std::borrow::Cow;

use deadpool::managed::{Manager, Metrics, Pool, RecycleError, RecycleResult};
use tiberius::{AuthMethod, Client, ColumnData, Config, FromSql, ToSql};
use tokio::net::TcpStream;
use tokio::sync::OnceCell;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::db::driver::{DbDriver, QueryResult};
use crate::db::{
    placeholders_mssql, quote_columns, quote_table, rows_per_statement, split_table, Dialect,
};
use crate::models::connection::ConnectionConfig;
use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};
use crate::models::value::{CellValue, DataType};
use crate::security::secrets::SecretString;

/// MSSQL 參數上限 2100，保守取 2000。
const PARAM_LIMIT: usize = 2_000;

type MssqlClient = Client<Compat<TcpStream>>;

/// deadpool 連線管理器：建立 / 回收 tiberius 連線。
pub struct MssqlManager {
    config: Config,
}

impl Manager for MssqlManager {
    type Type = MssqlClient;
    type Error = EluEtlError;

    async fn create(&self) -> Result<Self::Type, Self::Error> {
        let tcp = TcpStream::connect(self.config.get_addr()).await?;
        tcp.set_nodelay(true)?;
        Ok(Client::connect(self.config.clone(), tcp.compat_write()).await?)
    }

    async fn recycle(&self, conn: &mut Self::Type, _: &Metrics) -> RecycleResult<Self::Error> {
        conn.simple_query("SELECT 1")
            .await
            .map_err(|e| RecycleError::Backend(EluEtlError::from(e)))?;
        Ok(())
    }
}

/// SQL Server 驅動（tiberius，TDS over rustls + deadpool 連線池）。
pub struct MssqlDriver {
    pool: OnceCell<Pool<MssqlManager>>,
    config: Config,
}

impl MssqlDriver {
    pub fn new(config: ConnectionConfig, password: SecretString) -> Self {
        let mut c = Config::new();
        c.host(&config.host);
        c.port(config.port_or_default());
        c.database(&config.database);
        c.authentication(AuthMethod::sql_server(&config.username, password.expose()));
        if config.trust_server_certificate {
            // 安全政策：信任自簽憑證需使用者明確 opt-in，並記入審計日誌
            tracing::warn!(
                target: "audit",
                conn_id = %config.id,
                "trust_server_certificate 已啟用（自簽憑證）"
            );
            c.trust_cert();
        }
        Self {
            pool: OnceCell::new(),
            config: c,
        }
    }

    async fn get_conn(&self) -> Result<deadpool::managed::Object<MssqlManager>, EluEtlError> {
        let pool = self
            .pool
            .get_or_try_init(|| async {
                Pool::builder(MssqlManager {
                    config: self.config.clone(),
                })
                .max_size(10)
                .build()
                .map_err(|e| EluEtlError::Db(e.to_string()))
            })
            .await?;
        pool.get()
            .await
            .map_err(|e| EluEtlError::Db(format!("取得連線失敗: {e}")))
    }
}

/// tiberius 動態參數：NULL 依目標型別綁定，其餘依值綁定。
struct MssqlParam<'a> {
    cell: &'a CellValue,
    ty: DataType,
}

impl ToSql for MssqlParam<'_> {
    fn to_sql(&self) -> ColumnData<'_> {
        match self.cell {
            CellValue::Null => match self.ty {
                DataType::Integer => ColumnData::I64(None),
                DataType::Float => ColumnData::F64(None),
                DataType::Bool => ColumnData::Bit(None),
                DataType::Text => ColumnData::String(None),
                DataType::DateTime => ColumnData::DateTime2(None),
                DataType::Date => ColumnData::Date(None),
            },
            CellValue::Int(v) => ColumnData::I64(Some(*v)),
            CellValue::Float(v) => ColumnData::F64(Some(*v)),
            CellValue::Bool(v) => ColumnData::Bit(Some(*v)),
            CellValue::Text(v) => ColumnData::String(Some(Cow::from(v.as_str()))),
            CellValue::DateTime(v) => v.to_sql(),
            CellValue::Date(v) => v.to_sql(),
        }
    }
}

/// tiberius ColumnData → CellValue（時間型別委派給 tiberius 的 chrono FromSql）。
fn columndata_to_cell(cd: ColumnData<'static>) -> Result<CellValue, EluEtlError> {
    let v = match &cd {
        ColumnData::U8(v) => v.map(|x| CellValue::Int(x as i64)),
        ColumnData::I16(v) => v.map(|x| CellValue::Int(x as i64)),
        ColumnData::I32(v) => v.map(|x| CellValue::Int(x as i64)),
        ColumnData::I64(v) => v.map(CellValue::Int),
        ColumnData::F32(v) => v.map(|x| CellValue::Float(x as f64)),
        ColumnData::F64(v) => v.map(CellValue::Float),
        ColumnData::Bit(v) => v.map(CellValue::Bool),
        ColumnData::String(v) => v.as_ref().map(|s| CellValue::Text(s.to_string())),
        ColumnData::Guid(v) => v.map(|g| CellValue::Text(g.to_string())),
        ColumnData::Numeric(v) => {
            v.map(|n| CellValue::Float(n.value() as f64 / 10f64.powi(n.scale() as i32)))
        }
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => {
            chrono::NaiveDateTime::from_sql(&cd)?.map(CellValue::DateTime)
        }
        ColumnData::DateTimeOffset(_) => chrono::DateTime::<chrono::Utc>::from_sql(&cd)?
            .map(|v| CellValue::DateTime(v.naive_utc())),
        ColumnData::Date(_) => chrono::NaiveDate::from_sql(&cd)?.map(CellValue::Date),
        ColumnData::Time(_) => chrono::NaiveTime::from_sql(&cd)?
            .map(|v| CellValue::Text(v.format("%H:%M:%S").to_string())),
        ColumnData::Xml(v) => v.as_ref().map(|x| CellValue::Text(x.to_string())),
        // BINARY / IMAGE 等不支援的型別
        ColumnData::Binary(_) => None,
    };
    Ok(v.unwrap_or(CellValue::Null))
}

#[async_trait::async_trait]
impl DbDriver for MssqlDriver {
    async fn test_connection(&self) -> Result<(), EluEtlError> {
        let mut conn = self.get_conn().await?;
        conn.simple_query("SELECT 1").await?.into_results().await?;
        Ok(())
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError> {
        let mut conn = self.get_conn().await?;
        let rows = conn
            .query(
                "SELECT TABLE_SCHEMA, TABLE_NAME FROM INFORMATION_SCHEMA.TABLES \
                 WHERE TABLE_TYPE = 'BASE TABLE' ORDER BY TABLE_SCHEMA, TABLE_NAME",
                &[],
            )
            .await?
            .into_first_result()
            .await?;
        rows.iter()
            .map(|r| {
                Ok(TableInfo {
                    schema: r.try_get::<&str, _>(0)?.map(|s| s.to_string()),
                    name: r
                        .try_get::<&str, _>(1)?
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    async fn get_columns(&self, table: &str) -> Result<Vec<ColumnInfo>, EluEtlError> {
        let (schema, name) = split_table(table);
        let mut conn = self.get_conn().await?;
        let rows = match &schema {
            Some(s) => {
                conn.query(
                    "SELECT COLUMN_NAME, DATA_TYPE, IS_NULLABLE, ORDINAL_POSITION \
                     FROM INFORMATION_SCHEMA.COLUMNS \
                     WHERE TABLE_NAME = @P1 AND TABLE_SCHEMA = @P2 ORDER BY ORDINAL_POSITION",
                    &[&name, s],
                )
                .await?
            }
            None => {
                conn.query(
                    "SELECT COLUMN_NAME, DATA_TYPE, IS_NULLABLE, ORDINAL_POSITION \
                     FROM INFORMATION_SCHEMA.COLUMNS \
                     WHERE TABLE_NAME = @P1 ORDER BY ORDINAL_POSITION",
                    &[&name],
                )
                .await?
            }
        }
        .into_first_result()
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ColumnInfo {
                    name: r
                        .try_get::<&str, _>(0)?
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    db_type: r
                        .try_get::<&str, _>(1)?
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    nullable: r.try_get::<&str, _>(2)? == Some("YES"),
                    ordinal: r.try_get::<i32, _>(3)?.unwrap_or(0) as u32,
                })
            })
            .collect()
    }

    async fn query_all(
        &self,
        sql: &str,
        max_rows: Option<usize>,
    ) -> Result<QueryResult, EluEtlError> {
        let mut conn = self.get_conn().await?;
        let rows = conn.query(sql, &[]).await?.into_first_result().await?;
        let mut result = QueryResult::default();
        for row in rows {
            if result.columns.is_empty() {
                result.columns = row.columns().iter().map(|c| c.name().to_string()).collect();
            }
            let cells: Result<Vec<CellValue>, EluEtlError> =
                row.into_iter().map(columndata_to_cell).collect();
            result.rows.push(cells?);
            if max_rows.is_some_and(|m| result.rows.len() >= m) {
                break;
            }
        }
        Ok(result)
    }

    async fn write_batch(
        &self,
        table: &str,
        columns: &[String],
        types: &[DataType],
        rows: &[Vec<CellValue>],
    ) -> Result<u64, EluEtlError> {
        let table_q = quote_table(Dialect::Mssql, table)?;
        let cols_q = quote_columns(Dialect::Mssql, columns)?;
        let chunk_size = rows_per_statement(columns.len(), PARAM_LIMIT);

        let mut conn = self.get_conn().await?;
        conn.simple_query("BEGIN TRAN")
            .await?
            .into_results()
            .await?;

        let write = async {
            for chunk in rows.chunks(chunk_size) {
                let sql = format!(
                    "INSERT INTO {table_q} ({cols_q}) VALUES {}",
                    placeholders_mssql(chunk.len(), columns.len())
                );
                let params: Vec<MssqlParam> = chunk
                    .iter()
                    .flat_map(|row| {
                        row.iter()
                            .zip(types)
                            .map(|(cell, ty)| MssqlParam { cell, ty: *ty })
                    })
                    .collect();
                let refs: Vec<&dyn ToSql> = params.iter().map(|p| p as &dyn ToSql).collect();
                conn.execute(sql, &refs).await?;
            }
            Ok::<(), EluEtlError>(())
        };

        match write.await {
            Ok(()) => {
                conn.simple_query("COMMIT").await?.into_results().await?;
                Ok(rows.len() as u64)
            }
            Err(e) => {
                // rollback 失敗不掩蓋原始錯誤
                let _ = conn.simple_query("ROLLBACK").await;
                Err(e)
            }
        }
    }
}
