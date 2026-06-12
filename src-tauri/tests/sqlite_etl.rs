//! SQLite 端對端整合測試：模擬「CSV 解析 → 型別轉換 → 批次寫入 → 查回驗證」
//! 完整匯入流程（驅動層級，不需 OS keychain / Tauri runtime）。

use elu_etl_lib::db;
use elu_etl_lib::etl::mapping::{MappingRule, NullPolicy};
use elu_etl_lib::etl::transform::transform_rows;
use elu_etl_lib::models::connection::{ConnectionConfig, DbKind};
use elu_etl_lib::models::value::{CellValue, DataType};
use elu_etl_lib::security::secrets::SecretString;
use uuid::Uuid;

fn sqlite_config(path: &str) -> ConnectionConfig {
    ConnectionConfig {
        id: Uuid::new_v4(),
        name: "整合測試".into(),
        kind: DbKind::Sqlite,
        host: String::new(),
        port: None,
        database: path.to_string(),
        username: String::new(),
        trust_server_certificate: false,
    }
}

fn rule(idx: usize, name: &str, target: &str, ty: DataType, np: NullPolicy) -> MappingRule {
    MappingRule {
        source_index: idx,
        source_name: name.into(),
        target_column: target.into(),
        target_type: ty,
        null_policy: np,
    }
}

#[tokio::test]
async fn sqlite_end_to_end_import_and_export_query() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("etl_test.db");
    std::fs::File::create(&db_path).unwrap(); // 空檔即合法 SQLite DB

    let config = sqlite_config(db_path.to_str().unwrap());
    let driver = db::create_driver(&config, &SecretString::new(String::new()));

    driver.test_connection().await.unwrap();
    driver
        .query_all(
            "CREATE TABLE employees (
                emp_id INTEGER NOT NULL,
                emp_name TEXT NOT NULL,
                salary REAL,
                active INTEGER,
                hired_at TEXT
            )",
            None,
        )
        .await
        .unwrap();

    // 模擬 CSV 解析後的來源資料（全文字 + 一筆含 NULL、一筆型別錯誤）
    let source_rows = vec![
        vec![
            CellValue::Text("1".into()),
            CellValue::Text("王小明".into()),
            CellValue::Text("50000.5".into()),
            CellValue::Text("true".into()),
            CellValue::Text("2026-01-15".into()),
        ],
        vec![
            CellValue::Text("2".into()),
            CellValue::Text("李大華".into()),
            CellValue::Null, // salary 允許 NULL
            CellValue::Text("false".into()),
            CellValue::Text("2025/12/01".into()),
        ],
        vec![
            CellValue::Text("not-a-number".into()), // 轉換錯誤 → 進錯誤報告
            CellValue::Text("壞資料".into()),
            CellValue::Text("1".into()),
            CellValue::Text("true".into()),
            CellValue::Text("2026-02-01".into()),
        ],
    ];

    let rules = vec![
        rule(0, "編號", "emp_id", DataType::Integer, NullPolicy::Error),
        rule(1, "姓名", "emp_name", DataType::Text, NullPolicy::Error),
        rule(2, "薪資", "salary", DataType::Float, NullPolicy::Allow),
        rule(3, "在職", "active", DataType::Bool, NullPolicy::Allow),
        rule(4, "到職日", "hired_at", DataType::Date, NullPolicy::Allow),
    ];

    let (ok_rows, errors) = transform_rows(&source_rows, &rules, 2);
    assert_eq!(ok_rows.len(), 2);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].row, 4); // 第 3 筆資料 = 來源檔第 4 行（表頭佔第 1 行）
    assert_eq!(errors[0].column, "編號");

    let columns: Vec<String> = rules.iter().map(|r| r.target_column.clone()).collect();
    let types: Vec<DataType> = rules.iter().map(|r| r.target_type).collect();
    let written = driver
        .write_batch("employees", &columns, &types, &ok_rows)
        .await
        .unwrap();
    assert_eq!(written, 2);

    // 查回驗證型別與值
    let result = driver
        .query_all(
            "SELECT emp_id, emp_name, salary, active, hired_at FROM employees ORDER BY emp_id",
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0][0], CellValue::Int(1));
    assert_eq!(result.rows[0][1], CellValue::Text("王小明".into()));
    assert_eq!(result.rows[0][2], CellValue::Float(50000.5));
    assert_eq!(result.rows[0][3], CellValue::Int(1)); // SQLite 布林存 INTEGER
    assert_eq!(result.rows[0][4], CellValue::Text("2026-01-15".into()));
    assert_eq!(result.rows[1][2], CellValue::Null); // NULL 落地

    // metadata API
    let tables = driver.list_tables().await.unwrap();
    assert!(tables.iter().any(|t| t.name == "employees"));
    let cols = driver.get_columns("employees").await.unwrap();
    assert_eq!(cols.len(), 5);
    assert_eq!(cols[0].name, "emp_id");
    assert!(!cols[0].nullable);
    assert!(cols[2].nullable);

    // max_rows 上限（預覽路徑）
    let preview = driver
        .query_all("SELECT * FROM employees", Some(1))
        .await
        .unwrap();
    assert_eq!(preview.rows.len(), 1);
}

#[tokio::test]
async fn sqlite_rejects_dangerous_table_name() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("inj.db");
    std::fs::File::create(&db_path).unwrap();
    let driver = db::create_driver(
        &sqlite_config(db_path.to_str().unwrap()),
        &SecretString::new(String::new()),
    );

    let err = driver
        .write_batch(
            "x\"; DROP TABLE y; --",
            &["a".into()],
            &[DataType::Text],
            &[vec![CellValue::Text("v".into())]],
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("不合法"));
}

#[tokio::test]
async fn xlsx_export_roundtrip() {
    // 匯出 xlsx 後以 calamine 讀回驗證（writer ↔ reader golden 測試）
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("export.xlsx");

    let columns = vec!["id".to_string(), "name".to_string(), "when".to_string()];
    let rows = vec![
        vec![
            CellValue::Int(1),
            CellValue::Text("測試".into()),
            CellValue::DateTime(
                chrono::NaiveDate::from_ymd_opt(2026, 6, 12)
                    .unwrap()
                    .and_hms_opt(10, 30, 0)
                    .unwrap(),
            ),
        ],
        vec![CellValue::Int(2), CellValue::Null, CellValue::Null],
    ];
    let n = elu_etl_lib::excel::writer::write_xlsx(out.to_str().unwrap(), &columns, &rows).unwrap();
    assert_eq!(n, 2);

    let sheets = elu_etl_lib::excel::reader::list_sheets(out.to_str().unwrap()).unwrap();
    let back = elu_etl_lib::excel::reader::read_rows(out.to_str().unwrap(), &sheets[0]).unwrap();
    assert_eq!(back[0][0], CellValue::Text("id".into())); // 表頭
    assert_eq!(back[1][0], CellValue::Float(1.0)); // Excel 數字一律 f64
    assert_eq!(back[1][1], CellValue::Text("測試".into()));
    assert!(matches!(back[1][2], CellValue::DateTime(_)));
}
