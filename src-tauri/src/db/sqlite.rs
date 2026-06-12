use std::time::Duration;

use futures::TryStreamExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{Column, Row};
use tokio::sync::OnceCell;

use crate::bind_cell;
use crate::db::driver::{DbDriver, QueryResult};
use crate::db::{placeholders_question, quote_columns, quote_table, rows_per_statement, Dialect};
use crate::models::connection::ConnectionConfig;
use crate::models::errors::EluEtlError;
use crate::models::schema::{ColumnInfo, TableInfo};
use crate::models::value::{CellValue, DataType};

/// SQLite 參數上限 32766（3.32+），保守取 30000。
const PARAM_LIMIT: usize = 30_000;

/// SQLite 驅動（sqlx）。`ConnectionConfig.database` 為檔案路徑。
pub struct SqliteDriver {
    options: SqliteConnectOptions,
    pool: OnceCell<SqlitePool>,
}

impl SqliteDriver {
    pub fn new(config: &ConnectionConfig) -> Self {
        let options = SqliteConnectOptions::new()
            .filename(&config.database)
            // 不憑空建檔；建檔行為留給使用者明確操作
            .create_if_missing(false)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);
        Self {
            options,
            pool: OnceCell::new(),
        }
    }

    async fn pool(&self) -> Result<&SqlitePool, EluEtlError> {
        self.pool
            .get_or_try_init(|| async {
                Ok::<_, EluEtlError>(
                    SqlitePoolOptions::new()
                        .max_connections(4)
                        .acquire_timeout(Duration::from_secs(30))
                        .connect_with(self.options.clone())
                        .await?,
                )
            })
            .await
    }
}

/// SQLite 為動態型別，依實際儲存類別嘗試解碼（i64 → f64 → String）。
fn decode_cell(row: &SqliteRow, idx: usize) -> CellValue {
    if let Ok(v) = row.try_get::<Option<i64>, _>(idx) {
        return v.map_or(CellValue::Null, CellValue::Int);
    }
    if let Ok(v) = row.try_get::<Option<f64>, _>(idx) {
        return v.map_or(CellValue::Null, CellValue::Float);
    }
    if let Ok(v) = row.try_get::<Option<String>, _>(idx) {
        return v.map_or(CellValue::Null, CellValue::Text);
    }
    // BLOB 等不支援的儲存類別
    CellValue::Null
}

#[async_trait::async_trait]
impl DbDriver for SqliteDriver {
    async fn test_connection(&self) -> Result<(), EluEtlError> {
        sqlx::query("SELECT 1").execute(self.pool().await?).await?;
        Ok(())
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError> {
        let rows = sqlx::query(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
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
            "SELECT name, type, \"notnull\", cid FROM pragma_table_info(?1) ORDER BY cid",
        )
        .bind(table)
        .fetch_all(self.pool().await?)
        .await?;
        if rows.is_empty() {
            return Err(EluEtlError::NotFound(format!("資料表 {table} 不存在")));
        }
        Ok(rows
            .iter()
            .map(|r| ColumnInfo {
                name: r.get::<String, _>(0),
                db_type: r.get::<String, _>(1),
                nullable: r.get::<i64, _>(2) == 0,
                ordinal: r.get::<i64, _>(3) as u32 + 1,
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
            let cells = (0..result.columns.len())
                .map(|i| decode_cell(&row, i))
                .collect();
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
        let table_q = quote_table(Dialect::Sqlite, table)?;
        let cols_q = quote_columns(Dialect::Sqlite, columns)?;
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
}
