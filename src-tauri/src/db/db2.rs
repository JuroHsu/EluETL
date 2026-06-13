//! IBM DB2（LUW）驅動。
//!
//! 與其餘四種資料庫不同，DB2 沒有成熟的純 Rust 驅動（DRDA 協定無成熟實作），
//! 連線必須倚賴 IBM 的原生 CLI / ODBC 驅動（IBM Data Server Driver，俗稱 clidriver）。
//! 為了不破壞「核心純 Rust、零驅動依賴」的預設建置，本檔分成兩部分：
//!
//! 1. **永遠編譯**：[`status`] / [`detect_driver_present`] — 不相依 `ibm_db`，
//!    供前端在使用者選擇 DB2 時偵測環境是否就緒（未就緒則提示安裝驅動）。
//! 2. **`db2` feature gate**：[`Db2Driver`] — 以 `ibm_db` crate（底層 IBM CLI/ODBC）
//!    實作 [`DbDriver`]。預設不編譯；需 `cargo build --features db2` 且系統備有
//!    clidriver 才會納入。此路徑在無 clidriver 的環境（含本專案 CI）無法編譯／測試，
//!    實際連線行為須於備妥 DB2 驅動的環境驗證。

use serde::Serialize;

/// IBM Data Server Driver 下載頁（前端提示「需安裝驅動」時的超連結來源亦同）。
pub const DRIVER_DOWNLOAD_URL: &str =
    "https://www.ibm.com/support/pages/db2-data-server-driver-package-ds-driver";

/// DB2 驅動就緒狀態（回傳前端）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Db2DriverStatus {
    /// 真正可連線（= 本版本含 `db2` feature 且系統偵測到 IBM 驅動）。
    pub available: bool,
    /// 本版本是否以 `db2` feature 編譯（含 DB2 連線實作）。
    pub feature_built: bool,
    /// 是否在系統環境偵測到 IBM Data Server Driver。
    pub driver_present: bool,
    /// 人類可讀說明（前端直接顯示）。
    pub message: String,
    /// 驅動下載頁。
    pub download_url: String,
}

/// 偵測系統是否備有 IBM Data Server Driver（clidriver）。
///
/// 純環境變數 / 路徑啟發式偵測，不相依 `ibm_db`，因此預設建置即可呼叫。
/// `ibm_db` 在建置與執行期讀取 `IBM_DB_HOME`；DB2 client 安裝通常會設定
/// `DB2_HOME` / `IBM_DB_DIR` / `DB2INSTANCE`。任一指向存在的目錄即視為就緒。
pub fn detect_driver_present() -> bool {
    use std::path::Path;
    ["IBM_DB_HOME", "IBM_DB_DIR", "DB2_HOME", "CLIDRIVER_HOME"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .any(|home| !home.is_empty() && Path::new(&home).exists())
}

/// 組合 DB2 驅動就緒狀態。
pub fn status() -> Db2DriverStatus {
    let feature_built = cfg!(feature = "db2");
    let driver_present = detect_driver_present();
    let available = feature_built && driver_present;
    let message = match (feature_built, driver_present) {
        (true, true) => "IBM DB2 驅動已就緒。".to_string(),
        (true, false) => {
            "找不到 IBM Data Server Driver（clidriver）。請安裝後設定 \
             IBM_DB_HOME（或 DB2_HOME）環境變數，再重新啟動本程式。"
                .to_string()
        }
        (false, _) => {
            "此版本未編譯 IBM DB2 支援。請於備有 IBM Data Server Driver 的環境以 \
             `cargo build --features db2` 重新建置；DB2 連線方可使用。"
                .to_string()
        }
    };
    Db2DriverStatus {
        available,
        feature_built,
        driver_present,
        message,
        download_url: DRIVER_DOWNLOAD_URL.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 以下為實際 DB2 連線實作，僅在 `db2` feature 開啟時編譯。
// ---------------------------------------------------------------------------

#[cfg(feature = "db2")]
mod imp {
    use super::*;
    use ibm_db::{create_environment_v3, ResultSetState, Statement};

    use crate::db::driver::{DbDriver, QueryResult};
    use crate::db::{quote_columns, quote_table, Dialect};
    use crate::models::connection::ConnectionConfig;
    use crate::models::errors::EluEtlError;
    use crate::models::schema::{ColumnInfo, TableInfo};
    use crate::models::value::{CellValue, DataType};
    use crate::security::secrets::SecretString;

    /// 單一 INSERT 陳述式最多容納的列數（DB2 以行內字面值組裝，受陳述式長度約束）。
    const MAX_ROWS_PER_INSERT: usize = 500;

    /// IBM DB2 驅動（`ibm_db` / CLI 驅動）。
    ///
    /// 設計取捨：`ibm_db`（底層 odbc 風格 API）的 handle 並非 `Send`，且綁定參數採
    /// typestate API、不利於動態參數數量。為配合 async `DbDriver: Send + Sync`，
    /// 本驅動將所有 ODBC 工作關進 [`tokio::task::spawn_blocking`] 內就地建立連線、
    /// 物化為 owned 結果後回傳；寫入則以「行內字面值（嚴格轉義）」組裝多列 INSERT，
    /// 識別字仍經 [`quote_table`] / [`quote_columns`] 白名單驗證。
    pub struct Db2Driver {
        /// CLI 連線字串（含密碼）。僅存於記憶體、不落地、不寫入錯誤訊息。
        conn_str: SecretString,
    }

    impl Db2Driver {
        pub fn new(config: &ConnectionConfig, password: &SecretString) -> Self {
            // DB2 CLI 連線字串。HOSTNAME/PORT/PROTOCOL 走 TCP/IP。
            let conn_str = format!(
                "DRIVER={{IBM DB2 ODBC DRIVER}};DATABASE={};HOSTNAME={};PORT={};\
                 PROTOCOL=TCPIP;UID={};PWD={};",
                config.database,
                config.host,
                config.port_or_default(),
                config.username,
                password.expose(),
            );
            Self {
                conn_str: SecretString::new(conn_str),
            }
        }

        /// 在 blocking 執行緒建立連線、執行查詢並物化結果。
        /// 所有欄位以字串取出（DB2 來源值多用於 DSL lookup，比對採文字正規化；
        /// 目標寫入型別另由 [`get_columns`](Db2Driver::get_columns) 提供）。
        fn run_query_blocking(
            conn_str: &str,
            sql: &str,
            max_rows: Option<usize>,
        ) -> Result<QueryResult, EluEtlError> {
            let env = create_environment_v3().map_err(db2_err)?;
            let conn = env
                .connect_with_connection_string(conn_str)
                .map_err(db2_err)?;
            let stmt = Statement::with_parent(&conn).map_err(db2_err)?;

            let mut result = QueryResult::default();
            match stmt.exec_direct(sql).map_err(db2_err)? {
                ResultSetState::Data(mut stmt) => {
                    let ncols = stmt.num_result_cols().map_err(db2_err)? as usize;
                    for i in 1..=ncols {
                        let desc = stmt.describe_col(i as u16).map_err(db2_err)?;
                        result.columns.push(desc.name);
                    }
                    while let Some(mut cursor) = stmt.fetch().map_err(db2_err)? {
                        let mut cells = Vec::with_capacity(ncols);
                        for i in 1..=ncols {
                            let v: Option<String> =
                                cursor.get_data(i as u16).map_err(db2_err)?;
                            cells.push(v.map_or(CellValue::Null, CellValue::Text));
                        }
                        result.rows.push(cells);
                        if max_rows.is_some_and(|m| result.rows.len() >= m) {
                            break;
                        }
                    }
                }
                ResultSetState::NoData(_) => {}
            }
            Ok(result)
        }

        async fn run_query(
            &self,
            sql: String,
            max_rows: Option<usize>,
        ) -> Result<QueryResult, EluEtlError> {
            let conn_str = self.conn_str.expose().to_string();
            tokio::task::spawn_blocking(move || {
                Self::run_query_blocking(&conn_str, &sql, max_rows)
            })
            .await?
        }
    }

    /// 將 DB2 / ODBC 診斷紀錄轉為統一錯誤；不含連線字串等機密。
    fn db2_err<E: std::fmt::Display>(e: E) -> EluEtlError {
        EluEtlError::Db(format!("DB2: {e}"))
    }

    /// 字串字面值：標準 SQL 轉義（單引號加倍），並剔除 NUL。
    fn quote_str(s: &str) -> String {
        let escaped = s.replace('\0', "").replace('\'', "''");
        format!("'{escaped}'")
    }

    /// `CellValue` → DB2 SQL 字面值（型別化、嚴格轉義）。
    fn sql_literal(cell: &CellValue) -> String {
        match cell {
            CellValue::Null => "NULL".to_string(),
            CellValue::Int(v) => v.to_string(),
            CellValue::Float(v) if v.is_finite() => v.to_string(),
            CellValue::Float(_) => "NULL".to_string(),
            CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            CellValue::Text(s) => quote_str(s),
            CellValue::DateTime(dt) => {
                format!("TIMESTAMP('{}')", dt.format("%Y-%m-%d %H:%M:%S"))
            }
            CellValue::Date(d) => format!("DATE('{}')", d.format("%Y-%m-%d")),
        }
    }

    #[async_trait::async_trait]
    impl DbDriver for Db2Driver {
        async fn test_connection(&self) -> Result<(), EluEtlError> {
            self.run_query("SELECT 1 FROM SYSIBM.SYSDUMMY1".to_string(), Some(1))
                .await
                .map(|_| ())
        }

        async fn list_tables(&self) -> Result<Vec<TableInfo>, EluEtlError> {
            // 使用者資料表，排除系統 schema。
            let sql = "SELECT TRIM(TABSCHEMA) AS S, TABNAME AS T FROM SYSCAT.TABLES \
                       WHERE TYPE = 'T' AND TABSCHEMA NOT LIKE 'SYS%' \
                       ORDER BY TABSCHEMA, TABNAME";
            let r = self.run_query(sql.to_string(), None).await?;
            Ok(r.rows
                .iter()
                .map(|row| TableInfo {
                    schema: row.first().and_then(cell_text),
                    name: row.get(1).and_then(cell_text).unwrap_or_default(),
                })
                .collect())
        }

        async fn get_columns(&self, table: &str) -> Result<Vec<ColumnInfo>, EluEtlError> {
            let (schema, name) = crate::db::split_table(table);
            crate::db::validate_ident(&name)?;
            let mut sql = format!(
                "SELECT COLNAME, TYPENAME, NULLS, COLNO FROM SYSCAT.COLUMNS \
                 WHERE TABNAME = {}",
                quote_str(&name)
            );
            if let Some(s) = &schema {
                crate::db::validate_ident(s)?;
                sql.push_str(&format!(" AND TABSCHEMA = {}", quote_str(s)));
            }
            sql.push_str(" ORDER BY COLNO");
            let r = self.run_query(sql, None).await?;
            Ok(r.rows
                .iter()
                .map(|row| ColumnInfo {
                    name: row.first().and_then(cell_text).unwrap_or_default(),
                    db_type: row.get(1).and_then(cell_text).unwrap_or_default(),
                    nullable: row.get(2).and_then(cell_text).as_deref() == Some("Y"),
                    ordinal: row
                        .get(3)
                        .and_then(cell_text)
                        .and_then(|s| s.trim().parse::<u32>().ok())
                        .unwrap_or(0),
                })
                .collect())
        }

        async fn query_all(
            &self,
            sql: &str,
            max_rows: Option<usize>,
        ) -> Result<QueryResult, EluEtlError> {
            self.run_query(sql.to_string(), max_rows).await
        }

        async fn write_batch(
            &self,
            table: &str,
            columns: &[String],
            _types: &[DataType],
            rows: &[Vec<CellValue>],
        ) -> Result<u64, EluEtlError> {
            let table_q = quote_table(Dialect::Db2, table)?;
            let cols_q = quote_columns(Dialect::Db2, columns)?;

            // 預先在 blocking 執行緒外組好 SQL 字串（避免把 CellValue 搬進閉包）。
            // 註：以行內字面值組裝多列 INSERT；原子性為「每個 chunk 一次自動提交」，
            // 全有全無模式的跨 chunk 原子性弱於 sqlx/tiberius 路徑（已知限制）。
            let mut statements: Vec<String> = Vec::new();
            for chunk in rows.chunks(MAX_ROWS_PER_INSERT) {
                let values = chunk
                    .iter()
                    .map(|row| {
                        let cells = row.iter().map(sql_literal).collect::<Vec<_>>().join(",");
                        format!("({cells})")
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                statements.push(format!(
                    "INSERT INTO {table_q} ({cols_q}) VALUES {values}"
                ));
            }

            let conn_str = self.conn_str.expose().to_string();
            let total = rows.len() as u64;
            tokio::task::spawn_blocking(move || -> Result<(), EluEtlError> {
                let env = create_environment_v3().map_err(db2_err)?;
                let conn = env
                    .connect_with_connection_string(&conn_str)
                    .map_err(db2_err)?;
                for sql in &statements {
                    let stmt = Statement::with_parent(&conn).map_err(db2_err)?;
                    stmt.exec_direct(sql).map_err(db2_err)?;
                }
                Ok(())
            })
            .await??;
            Ok(total)
        }
    }

    /// `run_query` 一律以字串取出，故僅需處理 Text / Null。
    fn cell_text(c: &CellValue) -> Option<String> {
        match c {
            CellValue::Text(s) => Some(s.clone()),
            _ => None,
        }
    }
}

#[cfg(feature = "db2")]
pub use imp::Db2Driver;
