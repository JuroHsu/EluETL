pub mod driver;
pub mod pool;

mod mssql;
mod mysql;
mod postgres;
mod sqlite;

use std::sync::Arc;

use crate::models::connection::{ConnectionConfig, DbKind};
use crate::models::errors::EluEtlError;
use crate::security::secrets::SecretString;
use driver::DbDriver;

/// 驅動工廠：依資料庫種類建立對應驅動（tiberius 或 sqlx）。
pub fn create_driver(config: &ConnectionConfig, password: &SecretString) -> Arc<dyn DbDriver> {
    match config.kind {
        DbKind::SqlServer => Arc::new(mssql::MssqlDriver::new(config.clone(), password.clone())),
        DbKind::Postgres => Arc::new(postgres::PostgresDriver::new(config, password)),
        DbKind::MySql => Arc::new(mysql::MySqlDriver::new(config, password)),
        DbKind::Sqlite => Arc::new(sqlite::SqliteDriver::new(config)),
    }
}

/// SQL 方言（identifier 引用規則）。
#[derive(Debug, Clone, Copy)]
pub enum Dialect {
    Mssql,
    Postgres,
    MySql,
    Sqlite,
}

/// 拒絕含引號 / 分號 / 控制字元的 identifier，防止注入與引用逃逸。
pub fn validate_ident(name: &str) -> Result<(), EluEtlError> {
    let bad = name.is_empty()
        || name.len() > 128
        || name
            .chars()
            .any(|c| "\"'`;[]\\\0\n\r\t".contains(c) || c.is_control());
    if bad {
        return Err(EluEtlError::Config(format!("不合法的識別字: {name:?}")));
    }
    Ok(())
}

fn quote_ident(dialect: Dialect, name: &str) -> String {
    match dialect {
        Dialect::Mssql => format!("[{name}]"),
        Dialect::Postgres | Dialect::Sqlite => format!("\"{name}\""),
        Dialect::MySql => format!("`{name}`"),
    }
}

/// 引用表名（支援 `schema.table` 形式，逐段驗證後引用）。
pub fn quote_table(dialect: Dialect, table: &str) -> Result<String, EluEtlError> {
    let parts: Vec<&str> = table.split('.').collect();
    if parts.len() > 2 {
        return Err(EluEtlError::Config(format!("不合法的表名: {table:?}")));
    }
    for p in &parts {
        validate_ident(p)?;
    }
    Ok(parts
        .iter()
        .map(|p| quote_ident(dialect, p))
        .collect::<Vec<_>>()
        .join("."))
}

/// 引用欄位清單。
pub fn quote_columns(dialect: Dialect, columns: &[String]) -> Result<String, EluEtlError> {
    for c in columns {
        validate_ident(c)?;
    }
    Ok(columns
        .iter()
        .map(|c| quote_ident(dialect, c))
        .collect::<Vec<_>>()
        .join(", "))
}

/// 拆解 `schema.table` → (schema, table)。
pub fn split_table(table: &str) -> (Option<String>, String) {
    match table.split_once('.') {
        Some((s, t)) => (Some(s.to_string()), t.to_string()),
        None => (None, table.to_string()),
    }
}

/// 每個 INSERT 陳述式可容納的行數（受 DB 參數上限約束，另設 1000 行上限）。
pub fn rows_per_statement(ncols: usize, param_limit: usize) -> usize {
    (param_limit / ncols.max(1)).clamp(1, 1000)
}

/// 產生多列 VALUES placeholder：PG `($1,$2),($3,$4)`。
pub fn placeholders_numbered(nrows: usize, ncols: usize) -> String {
    let mut s = String::new();
    let mut n = 1;
    for r in 0..nrows {
        if r > 0 {
            s.push(',');
        }
        s.push('(');
        for c in 0..ncols {
            if c > 0 {
                s.push(',');
            }
            s.push_str(&format!("${n}"));
            n += 1;
        }
        s.push(')');
    }
    s
}

/// 產生多列 VALUES placeholder：MySQL / SQLite `(?,?),(?,?)`。
pub fn placeholders_question(nrows: usize, ncols: usize) -> String {
    let row = format!("({})", vec!["?"; ncols].join(","));
    vec![row; nrows].join(",")
}

/// 產生多列 VALUES placeholder：MSSQL `(@P1,@P2),(@P3,@P4)`。
pub fn placeholders_mssql(nrows: usize, ncols: usize) -> String {
    let mut s = String::new();
    let mut n = 1;
    for r in 0..nrows {
        if r > 0 {
            s.push(',');
        }
        s.push('(');
        for c in 0..ncols {
            if c > 0 {
                s.push(',');
            }
            s.push_str(&format!("@P{n}"));
            n += 1;
        }
        s.push(')');
    }
    s
}

/// sqlx 動態綁定：依 CellValue 綁定原生型別；NULL 依目標型別綁定，
/// 確保 DB 端拿到正確型別的 NULL（PG 對 text NULL → date 欄位會報錯）。
#[macro_export]
macro_rules! bind_cell {
    ($query:expr, $cell:expr, $ty:expr) => {{
        use $crate::models::value::{CellValue, DataType};
        match $cell {
            CellValue::Null => match $ty {
                DataType::Integer => $query.bind(None::<i64>),
                DataType::Float => $query.bind(None::<f64>),
                DataType::Bool => $query.bind(None::<bool>),
                DataType::DateTime => $query.bind(None::<chrono::NaiveDateTime>),
                DataType::Date => $query.bind(None::<chrono::NaiveDate>),
                DataType::Text => $query.bind(None::<String>),
            },
            CellValue::Int(v) => $query.bind(*v),
            CellValue::Float(v) => $query.bind(*v),
            CellValue::Text(v) => $query.bind(v.clone()),
            CellValue::Bool(v) => $query.bind(*v),
            CellValue::DateTime(v) => $query.bind(*v),
            CellValue::Date(v) => $query.bind(*v),
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dangerous_idents() {
        assert!(validate_ident("users").is_ok());
        assert!(validate_ident("訂單明細").is_ok());
        assert!(validate_ident("a]; DROP TABLE x; --").is_err());
        assert!(validate_ident("a\"b").is_err());
        assert!(validate_ident("").is_err());
    }

    #[test]
    fn quotes_per_dialect() {
        assert_eq!(
            quote_table(Dialect::Mssql, "dbo.users").unwrap(),
            "[dbo].[users]"
        );
        assert_eq!(
            quote_table(Dialect::Postgres, "users").unwrap(),
            "\"users\""
        );
        assert_eq!(quote_table(Dialect::MySql, "users").unwrap(), "`users`");
    }

    #[test]
    fn placeholder_generation() {
        assert_eq!(placeholders_numbered(2, 2), "($1,$2),($3,$4)");
        assert_eq!(placeholders_question(2, 2), "(?,?),(?,?)");
        assert_eq!(placeholders_mssql(1, 3), "(@P1,@P2,@P3)");
        assert_eq!(rows_per_statement(10, 2000), 200);
        assert_eq!(rows_per_statement(1, 60000), 1000);
    }
}
