use std::time::Duration;

use futures::TryStreamExt;
use rust_decimal::prelude::ToPrimitive;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgRow};
use sqlx::{Column, Row, TypeInfo};
use tokio::sync::OnceCell;

use crate::bind_cell;
use crate::db::driver::{DbDriver, QueryResult};
use crate::db::{
    placeholders_numbered, quote_columns, quote_table, rows_per_statement, split_table, Dialect,
};
use crate::models::connection::ConnectionConfig;
use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};
use crate::models::value::{CellValue, DataType};
use crate::security::secrets::SecretString;

/// PG 參數上限 65535，保守取 60000。
const PARAM_LIMIT: usize = 60_000;

/// PostgreSQL 驅動（sqlx）。
/// 以 ConnectOptions 組態而非 URL 字串，避免密碼特殊字元的轉義問題。
pub struct PostgresDriver {
    options: PgConnectOptions,
    pool: OnceCell<PgPool>,
}

impl PostgresDriver {
    pub fn new(config: &ConnectionConfig, password: &SecretString) -> Self {
        let options = PgConnectOptions::new()
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

    async fn pool(&self) -> Result<&PgPool, EluEtlError> {
        self.pool
            .get_or_try_init(|| async {
                Ok::<_, EluEtlError>(
                    PgPoolOptions::new()
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

fn decode_cell(row: &PgRow, idx: usize) -> Result<CellValue, EluEtlError> {
    let type_name = row.column(idx).type_info().name().to_uppercase();
    let v = match type_name.as_str() {
        "INT2" => row
            .try_get::<Option<i16>, _>(idx)?
            .map(|v| CellValue::Int(v as i64)),
        "INT4" => row
            .try_get::<Option<i32>, _>(idx)?
            .map(|v| CellValue::Int(v as i64)),
        "INT8" => row.try_get::<Option<i64>, _>(idx)?.map(CellValue::Int),
        "FLOAT4" => row
            .try_get::<Option<f32>, _>(idx)?
            .map(|v| CellValue::Float(v as f64)),
        "FLOAT8" => row.try_get::<Option<f64>, _>(idx)?.map(CellValue::Float),
        "NUMERIC" => row
            .try_get::<Option<rust_decimal::Decimal>, _>(idx)?
            .map(|d| {
                d.to_f64()
                    .map_or(CellValue::Text(d.to_string()), CellValue::Float)
            }),
        "BOOL" => row.try_get::<Option<bool>, _>(idx)?.map(CellValue::Bool),
        "DATE" => row
            .try_get::<Option<chrono::NaiveDate>, _>(idx)?
            .map(CellValue::Date),
        "TIMESTAMP" => row
            .try_get::<Option<chrono::NaiveDateTime>, _>(idx)?
            .map(CellValue::DateTime),
        "TIMESTAMPTZ" => row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(idx)?
            .map(|v| CellValue::DateTime(v.naive_utc())),
        "TIME" => row
            .try_get::<Option<chrono::NaiveTime>, _>(idx)?
            .map(|v| CellValue::Text(v.format("%H:%M:%S").to_string())),
        "UUID" => row
            .try_get::<Option<uuid::Uuid>, _>(idx)?
            .map(|v| CellValue::Text(v.to_string())),
        "JSON" | "JSONB" => row
            .try_get::<Option<serde_json::Value>, _>(idx)?
            .map(|v| CellValue::Text(v.to_string())),
        "VARCHAR" | "TEXT" | "BPCHAR" | "CHAR" | "NAME" => {
            row.try_get::<Option<String>, _>(idx)?.map(CellValue::Text)
        }
        // 其他型別嘗試以文字取出，失敗則回 NULL（不中斷整批查詢）
        _ => row
            .try_get::<Option<String>, _>(idx)
            .unwrap_or(None)
            .map(CellValue::Text),
    };
    Ok(v.unwrap_or(CellValue::Null))
}

#[async_trait::async_trait]
impl DbDriver for PostgresDriver {
    async fn test_connection(&self) -> Result<(), EluEtlError> {
        sqlx::query("SELECT 1").execute(self.pool().await?).await?;
        Ok(())
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError> {
        let rows = sqlx::query(
            "SELECT table_schema, table_name FROM information_schema.tables \
             WHERE table_type = 'BASE TABLE' \
               AND table_schema NOT IN ('pg_catalog', 'information_schema') \
             ORDER BY table_schema, table_name",
        )
        .fetch_all(self.pool().await?)
        .await?;
        Ok(rows
            .iter()
            .map(|r| TableInfo {
                schema: Some(r.get::<String, _>(0)),
                name: r.get::<String, _>(1),
            })
            .collect())
    }

    async fn get_columns(&self, table: &str) -> Result<Vec<ColumnInfo>, EluEtlError> {
        let (schema, name) = split_table(table);
        let rows = sqlx::query(
            "SELECT column_name, data_type, is_nullable, ordinal_position \
             FROM information_schema.columns \
             WHERE table_name = $1 AND table_schema = COALESCE($2, current_schema()) \
             ORDER BY ordinal_position",
        )
        .bind(&name)
        .bind(schema)
        .fetch_all(self.pool().await?)
        .await?;
        Ok(rows
            .iter()
            .map(|r| ColumnInfo {
                name: r.get::<String, _>(0),
                db_type: r.get::<String, _>(1),
                nullable: r.get::<String, _>(2) == "YES",
                ordinal: r.get::<i32, _>(3) as u32,
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
        let table_q = quote_table(Dialect::Postgres, table)?;
        let cols_q = quote_columns(Dialect::Postgres, columns)?;
        let chunk_size = rows_per_statement(columns.len(), PARAM_LIMIT);

        let mut tx = self.pool().await?.begin().await?;
        for chunk in rows.chunks(chunk_size) {
            let sql = format!(
                "INSERT INTO {table_q} ({cols_q}) VALUES {}",
                placeholders_numbered(chunk.len(), columns.len())
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
}
