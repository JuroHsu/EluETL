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
        sheet: None,
        encoding: None,
        has_header: None,
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
    let driver = db::create_driver(&config, &SecretString::new(String::new())).unwrap();

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
    )
    .unwrap();

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
async fn wizard_database_source_end_to_end() {
    // 資料庫 → 資料庫：來源 SQLite 的 staging 表經型別轉換寫入另一個 SQLite
    use elu_etl_lib::db::pool::AppState;
    use elu_etl_lib::etl::executor;
    use elu_etl_lib::etl::mapping::{ErrorPolicy, EtlJobConfig, SourceSpec, WriteMode};
    use elu_etl_lib::state::store::StateStore;
    use tokio_util::sync::CancellationToken;

    let dir = tempfile::tempdir().unwrap();
    let state = AppState::default();
    state.set_store(StateStore::init(dir.path()).await.unwrap());

    let src_path = dir.path().join("source.db");
    std::fs::File::create(&src_path).unwrap();
    let src_config = sqlite_config(src_path.to_str().unwrap());
    state
        .store()
        .unwrap()
        .upsert_connection(&src_config)
        .await
        .unwrap();

    let dst_path = dir.path().join("target.db");
    std::fs::File::create(&dst_path).unwrap();
    let dst_config = sqlite_config(dst_path.to_str().unwrap());
    state
        .store()
        .unwrap()
        .upsert_connection(&dst_config)
        .await
        .unwrap();

    let src_driver = state.get_or_create_driver(src_config.id).await.unwrap();
    src_driver
        .query_all("CREATE TABLE staging (code TEXT, amount TEXT)", None)
        .await
        .unwrap();
    src_driver
        .query_all(
            "INSERT INTO staging (code, amount) VALUES ('1', '10.5'), ('x', '2')",
            None,
        )
        .await
        .unwrap();

    let dst_driver = state.get_or_create_driver(dst_config.id).await.unwrap();
    dst_driver
        .query_all("CREATE TABLE target_rows (id INTEGER, amount REAL)", None)
        .await
        .unwrap();

    let job = EtlJobConfig {
        job_id: Uuid::new_v4(),
        conn_id: dst_config.id,
        source: SourceSpec::Database {
            conn_id: src_config.id,
            query: "SELECT code, amount FROM staging ORDER BY code".into(),
        },
        target_table: "target_rows".into(),
        rules: vec![
            rule(0, "code", "id", DataType::Integer, NullPolicy::Error),
            rule(1, "amount", "amount", DataType::Float, NullPolicy::Allow),
        ],
        write_mode: WriteMode::BatchCommit,
        error_policy: ErrorPolicy::SkipAndReport,
        batch_size: 5000,
    };

    let summary = executor::run(&state, job, |_| {}, CancellationToken::new(), 0)
        .await
        .unwrap();
    assert_eq!(summary.status, "completed");
    assert_eq!(summary.total_rows, 2);
    assert_eq!(summary.success_rows, 1); // 'x' 無法轉整數 → 錯誤報告
    assert_eq!(summary.error_rows, 1);
    assert_eq!(summary.errors[0].row, 2); // 資料庫來源行號自結果集第 1 行起算

    let rows = dst_driver
        .query_all("SELECT id, amount FROM target_rows", None)
        .await
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], CellValue::Int(1));
    assert_eq!(rows[0][1], CellValue::Float(10.5));
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
