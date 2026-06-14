use std::time::Duration;

use futures::TryStreamExt;
use rust_decimal::prelude::ToPrimitive;
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlRow};
use sqlx::{Column, Row, TypeInfo};
use tokio::sync::OnceCell;

use crate::bind_cell;
use crate::db::driver::{DbDriver, QueryResult};
use crate::db::{placeholders_question, quote_columns, quote_table, rows_per_statement, Dialect};
use crate::models::connection::ConnectionConfig;
use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};
use crate::models::value::{CellValue, DataType};
use crate::security::secrets::SecretString;

/// MySQL placeholder 上限 65535，保守取 60000（亦受 max_allowed_packet 約束，
/// rows_per_statement 另有 1000 行上限）。
const PARAM_LIMIT: usize = 60_000;

/// MySQL 驅動（sqlx）。
pub struct MySqlDriver {
    options: MySqlConnectOptions,
    pool: OnceCell<MySqlPool>,
}

impl MySqlDriver {
    pub fn new(config: &ConnectionConfig, password: &SecretString) -> Self {
        let options = MySqlConnectOptions::new()
            .host(&config.host)
            .port(config.port_or_default())
            .username(&config.username)
            .password(password.expose())
            .database(&config.database);
        Self {
            options,
            pool: OnceCell::new(),
        }
    }

    async fn pool(&self) -> Result<&MySqlPool, EluEtlError> {
        self.pool
            .get_or_try_init(|| async {
                Ok::<_, EluEtlError>(
                    MySqlPoolOptions::new()
                        .max_connections(10)
                        .acquire_timeout(Duration::from_secs(30))
                        .idle_timeout(Duration::from_secs(300))
                        .connect_with(self.options.clone())
                        .await?,
                )
            })
            .await
    }
}

fn decode_cell(row: &MySqlRow, idx: usize) -> Result<CellValue, EluEtlError> {
    let type_name = row.column(idx).type_info().name().to_uppercase();
    let v = match type_name.as_str() {
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" | "YEAR" => row
            .try_get::<Option<i64>, _>(idx)
            .or_else(|_| {
                row.try_get::<Option<u64>, _>(idx)
                    .map(|o| o.map(|v| v as i64))
            })?
            .map(CellValue::Int),
        "BOOLEAN" => row.try_get::<Option<bool>, _>(idx)?.map(CellValue::Bool),
        "FLOAT" => row
            .try_get::<Option<f32>, _>(idx)?
            .map(|v| CellValue::Float(v as f64)),
        "DOUBLE" => row.try_get::<Option<f64>, _>(idx)?.map(CellValue::Float),
        "DECIMAL" => row
            .try_get::<Option<rust_decimal::Decimal>, _>(idx)?
            .map(|d| {
                d.to_f64()
                    .map_or(CellValue::Text(d.to_string()), CellValue::Float)
            }),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(idx)?
            .map(CellValue::Date),
        "DATETIME" | "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(idx)?
            .map(CellValue::DateTime),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(idx)?
            .map(|v| CellValue::Text(v.format("%H:%M:%S").to_string())),
        "JSON" => row
            .try_get::<Option<serde_json::Value>, _>(idx)?
            .map(|v| CellValue::Text(v.to_string())),
        // 文字與其他型別嘗試以字串取出，失敗（如 BLOB 非 UTF-8）回 NULL
        _ => row
            .try_get::<Option<String>, _>(idx)
            .unwrap_or(None)
            .map(CellValue::Text),
    };
    Ok(v.unwrap_or(CellValue::Null))
}

#[async_trait::async_trait]
impl DbDriver for MySqlDriver {
    async fn test_connection(&self) -> Result<(), EluEtlError> {
        sqlx::query("SELECT 1").execute(self.pool().await?).await?;
        Ok(())
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError> {
        let rows = sqlx::query(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE' \
             ORDER BY table_name",
        )
        .fetch_all(self.pool().await?)
        .await?;
        Ok(rows
            .iter()
            .map(|r| TableInfo {
                schema: None,
                name: r.get::<String, _>(0),
            })
            .collect())
    }

    async fn get_columns(&self, table: &str) -> Result<Vec<ColumnInfo>, EluEtlError> {
        let rows = sqlx::query(
            "SELECT column_name, data_type, is_nullable, ordinal_position \
             FROM information_schema.columns \
             WHERE table_schema = DATABASE() AND table_name = ? \
             ORDER BY ordinal_position",
        )
        .bind(table)
        .fetch_all(self.pool().await?)
        .await?;
        Ok(rows
            .iter()
            .map(|r| ColumnInfo {
                name: r.get::<String, _>(0),
                db_type: r.get::<String, _>(1),
                nullable: r.get::<String, _>(2) == "YES",
                ordinal: r.get::<u32, _>(3),
            })
            .collect())
    }

    async fn query_all(
        &self,
        sql: &str,
        max_rows: Option<usize>,
    ) -> Result<QueryResult, EluEtlError> {
        let pool = self.pool().await?;
        let mut stream = sqlx::query(sqlx::AssertSqlSafe(sql.to_string())).fetch(pool);
        let mut result = QueryResult::default();
        while let Some(row) = stream.try_next().await? {
            if result.columns.is_empty() {
                result.columns = row.columns().iter().map(|c| c.name().to_string()).collect();
            }
            let mut cells = Vec::with_capacity(result.columns.len());
            for i in 0..result.columns.len() {
                cells.push(decode_cell(&row, i)?);
            }
            result.rows.push(cells);
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
        let table_q = quote_table(Dialect::MySql, table)?;
        let cols_q = quote_columns(Dialect::MySql, columns)?;
        let chunk_size = rows_per_statement(columns.len(), PARAM_LIMIT);

        let mut tx = self.pool().await?.begin().await?;
        for chunk in rows.chunks(chunk_size) {
            let sql = format!(
                "INSERT INTO {table_q} ({cols_q}) VALUES {}",
                placeholders_question(chunk.len(), columns.len())
            );
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
            for row in chunk {
                for (cell, ty) in row.iter().zip(types) {
                    q = bind_cell!(q, cell, *ty);
                }
            }
            q.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(rows.len() as u64)
    }

    async fn execute_batch(
        &self,
        sql: &str,
        param_types: &[DataType],
        param_rows: &[Vec<CellValue>],
    ) -> Result<u64, EluEtlError> {
        if param_rows.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool().await?.begin().await?;
        let mut affected = 0u64;
        for row in param_rows {
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()));
            for (cell, ty) in row.iter().zip(param_types) {
                q = bind_cell!(q, cell, *ty);
            }
            affected += q.execute(&mut *tx).await?.rows_affected();
        }
        tx.commit().await?;
        Ok(affected)
    }
}
