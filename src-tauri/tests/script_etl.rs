//! ETL 腳本端對端整合測試：CSV 來源 → lookup join（email 比對）→ SQLite 寫入。

use elu_etl_lib::db::{self, Dialect};
use elu_etl_lib::etl::script::executor::{run, ResolvedScriptJob, ScriptSource};
use elu_etl_lib::etl::script::parser;
use elu_etl_lib::models::connection::{ConnectionConfig, DbKind};
use elu_etl_lib::models::value::CellValue;
use elu_etl_lib::security::secrets::SecretString;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[tokio::test]
async fn script_lookup_join_end_to_end() {
    let dir = tempfile::tempdir().unwrap();

    // 來源 CSV：3 行（2 行可比對、1 行查無對應）；email 大小寫故意不一致
    let csv_path = dir.path().join("users.csv");
    std::fs::write(
        &csv_path,
        "Id,email\nEXT-001,Alice@example.com\nEXT-002,bob@example.com\nEXT-003,nobody@example.com\n",
    )
    .unwrap();

    // 目標 SQLite：Account（既有帳號）+ ExternalIdentityMappings（待寫入）
    let db_path = dir.path().join("admin.db");
    std::fs::File::create(&db_path).unwrap();
    let config = ConnectionConfig {
        id: Uuid::new_v4(),
        name: "test".into(),
        kind: DbKind::Sqlite,
        host: String::new(),
        port: None,
        database: db_path.to_str().unwrap().into(),
        username: String::new(),
        trust_server_certificate: false,
        sheet: None,
        encoding: None,
        has_header: None,
    };
    let driver = db::create_driver(&config, &SecretString::new(String::new())).unwrap();
    driver
        .query_all(
            "CREATE TABLE Account (Id INTEGER PRIMARY KEY, email TEXT NOT NULL)",
            None,
        )
        .await
        .unwrap();
    driver
        .query_all(
            "CREATE TABLE ExternalIdentityMappings (
                AccountId INTEGER NOT NULL,
                ExternalId TEXT NOT NULL,
                ExternalSystemType TEXT NOT NULL
            )",
            None,
        )
        .await
        .unwrap();
    driver
        .query_all(
            "INSERT INTO Account (Id, email) VALUES
             (1, 'alice@example.com'), (2, 'BOB@example.com')",
            None,
        )
        .await
        .unwrap();

    // 使用者範例語法（SQLite 無 schema，前綴用單段表名）
    let script_text = r#"
If [users].[CSV].email == [Account].email
[ExternalIdentityMappings] ADD
{
 AccountId = [Account].Id
,ExternalId = [users].[CSV].Id
,ExternalSystemType = N'MICROSOFT_ENTRA_ID'
}
GO
"#;
    let script = parser::parse(script_text).unwrap();

    let params = ResolvedScriptJob {
        job_id: Uuid::new_v4(),
        source: ScriptSource::File {
            path: csv_path.to_str().unwrap().into(),
            sheet: "CSV".into(),
            has_header: true,
            encoding: None,
        },
        batch_size: 5000,
    };

    let summary = run(
        driver.clone(),
        Dialect::Sqlite,
        params,
        script,
        |_| {},
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(summary.status, "completed");
    assert_eq!(summary.total_rows, 3);
    assert_eq!(summary.success_rows, 2); // alice + bob（大小寫不敏感比對）
    assert_eq!(summary.error_rows, 1); // nobody 查無對應
    assert!(summary.errors[0].reason.contains("查無對應"));

    let result = driver
        .query_all(
            "SELECT AccountId, ExternalId, ExternalSystemType
             FROM ExternalIdentityMappings ORDER BY AccountId",
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0][0], CellValue::Int(1));
    assert_eq!(result.rows[0][1], CellValue::Text("EXT-001".into()));
    assert_eq!(
        result.rows[0][2],
        CellValue::Text("MICROSOFT_ENTRA_ID".into())
    );
    assert_eq!(result.rows[1][0], CellValue::Int(2));
    assert_eq!(result.rows[1][1], CellValue::Text("EXT-002".into()));
}

#[tokio::test]
async fn script_without_condition_inserts_all() {
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("d.csv");
    std::fs::write(&csv_path, "name,qty\nA,1\nB,2\n").unwrap();

    let db_path = dir.path().join("t.db");
    std::fs::File::create(&db_path).unwrap();
    let config = ConnectionConfig {
        id: Uuid::new_v4(),
        name: "t".into(),
        kind: DbKind::Sqlite,
        host: String::new(),
        port: None,
        database: db_path.to_str().unwrap().into(),
        username: String::new(),
        trust_server_certificate: false,
        sheet: None,
        encoding: None,
        has_header: None,
    };
    let driver = db::create_driver(&config, &SecretString::new(String::new())).unwrap();
    driver
        .query_all(
            "CREATE TABLE items (name TEXT, qty INTEGER, src TEXT)",
            None,
        )
        .await
        .unwrap();

    let script_text = "[items] ADD { name = name, qty = qty, src = N'import' } GO";
    let script = parser::parse(script_text).unwrap();
    let summary = run(
        driver.clone(),
        Dialect::Sqlite,
        ResolvedScriptJob {
            job_id: Uuid::new_v4(),
            source: ScriptSource::File {
                path: csv_path.to_str().unwrap().into(),
                sheet: "CSV".into(),
                has_header: true,
                encoding: None,
            },
            batch_size: 5000,
        },
        script,
        |_| {},
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(summary.success_rows, 2);
    assert_eq!(summary.error_rows, 0);

    let rows = driver
        .query_all("SELECT name, qty, src FROM items ORDER BY name", None)
        .await
        .unwrap()
        .rows;
    assert_eq!(rows[0][1], CellValue::Int(1));
    assert_eq!(rows[1][2], CellValue::Text("import".into()));
}

#[tokio::test]
async fn script_generators_produce_values() {
    // Gen.ULID / Gen.GUID / Gen.SHA256 / Gen.DateTime(Text)：每列產生值寫入目標
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("g.csv");
    std::fs::write(&csv_path, "name\nA\nB\n").unwrap();

    let db_path = dir.path().join("g.db");
    std::fs::File::create(&db_path).unwrap();
    let config = ConnectionConfig {
        id: Uuid::new_v4(),
        name: "g".into(),
        kind: DbKind::Sqlite,
        host: String::new(),
        port: None,
        database: db_path.to_str().unwrap().into(),
        username: String::new(),
        trust_server_certificate: false,
        sheet: None,
        encoding: None,
        has_header: None,
    };
    let driver = db::create_driver(&config, &SecretString::new(String::new())).unwrap();
    driver
        .query_all(
            "CREATE TABLE rows (id TEXT, guid TEXT, name TEXT, hash TEXT, created TEXT)",
            None,
        )
        .await
        .unwrap();

    let script = parser::parse(
        "WORK '產生器' {
         [rows] ADD {
           id = Gen.ULID,
           guid = Gen.GUID,
           name = name,
           hash = Gen.SHA256,
           created = Gen.DateTime(Text)
         }
         }",
    )
    .unwrap();
    let summary = run(
        driver.clone(),
        Dialect::Sqlite,
        ResolvedScriptJob {
            job_id: Uuid::new_v4(),
            source: ScriptSource::File {
                path: csv_path.to_str().unwrap().into(),
                sheet: "CSV".into(),
                has_header: true,
                encoding: None,
            },
            batch_size: 5000,
        },
        script,
        |_| {},
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(summary.success_rows, 2);
    assert_eq!(summary.error_rows, 0);

    let rows = driver
        .query_all(
            "SELECT id, guid, name, hash, created FROM rows ORDER BY name",
            None,
        )
        .await
        .unwrap()
        .rows;
    let text = |c: &CellValue| match c {
        CellValue::Text(s) => s.clone(),
        other => panic!("預期文字，得到 {other:?}"),
    };
    // ULID：26 字元且兩列不同
    assert_eq!(text(&rows[0][0]).len(), 26);
    assert_ne!(text(&rows[0][0]), text(&rows[1][0]));
    // GUID：36 字元
    assert_eq!(text(&rows[0][1]).len(), 36);
    // SHA256：64 hex，且不同來源列雜湊不同
    assert_eq!(text(&rows[0][3]).len(), 64);
    assert_ne!(text(&rows[0][3]), text(&rows[1][3]));
    // DateTime(Text)：同一次執行內兩列相同
    assert_eq!(text(&rows[0][4]), text(&rows[1][4]));
}

#[tokio::test]
async fn script_concat_composes_values() {
    // 合成欄位：常值 + 比對表欄位、來源欄位 + 常值；NULL 項視為空字串
    let dir = tempfile::tempdir().unwrap();
    let csv_path = dir.path().join("c.csv");
    std::fs::write(
        &csv_path,
        "code,nick\nA1,alice\nB2,\n", // 第 2 列 nick 為空
    )
    .unwrap();

    let db_path = dir.path().join("c.db");
    std::fs::File::create(&db_path).unwrap();
    let config = ConnectionConfig {
        id: Uuid::new_v4(),
        name: "c".into(),
        kind: DbKind::Sqlite,
        host: String::new(),
        port: None,
        database: db_path.to_str().unwrap().into(),
        username: String::new(),
        trust_server_certificate: false,
        sheet: None,
        encoding: None,
        has_header: None,
    };
    let driver = db::create_driver(&config, &SecretString::new(String::new())).unwrap();
    driver
        .query_all("CREATE TABLE Ref (code TEXT, label TEXT)", None)
        .await
        .unwrap();
    driver
        .query_all(
            "INSERT INTO Ref (code, label) VALUES ('A1', '甲'), ('B2', '乙')",
            None,
        )
        .await
        .unwrap();
    driver
        .query_all("CREATE TABLE out (tag TEXT, who TEXT)", None)
        .await
        .unwrap();

    let script = parser::parse(
        "WORK '合成' {
         If [SOURCE].code == [Ref].code
         [out] ADD {
           tag = N'ENTRA:' + [Ref].label + N'/' + [SOURCE].code,
           who = [SOURCE].nick + N'@linkcom'
         }
         }",
    )
    .unwrap();
    let summary = run(
        driver.clone(),
        Dialect::Sqlite,
        ResolvedScriptJob {
            job_id: Uuid::new_v4(),
            source: ScriptSource::File {
                path: csv_path.to_str().unwrap().into(),
                sheet: "CSV".into(),
                has_header: true,
                encoding: None,
            },
            batch_size: 5000,
        },
        script,
        |_| {},
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(summary.success_rows, 2);
    assert_eq!(summary.error_rows, 0);

    let rows = driver
        .query_all("SELECT tag, who FROM out ORDER BY tag", None)
        .await
        .unwrap()
        .rows;
    assert_eq!(rows[0][0], CellValue::Text("ENTRA:乙/B2".into()));
    assert_eq!(rows[0][1], CellValue::Text("@linkcom".into())); // NULL 項 → 空字串
    assert_eq!(rows[1][0], CellValue::Text("ENTRA:甲/A1".into()));
    assert_eq!(rows[1][1], CellValue::Text("alice@linkcom".into()));
}

#[tokio::test]
async fn script_database_source_inserts_all() {
    // 資料庫作為來源：staging 表 → 腳本 → items 表（同一 SQLite 充當來源與目標）
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("dbsrc.db");
    std::fs::File::create(&db_path).unwrap();
    let config = ConnectionConfig {
        id: Uuid::new_v4(),
        name: "dbsrc".into(),
        kind: DbKind::Sqlite,
        host: String::new(),
        port: None,
        database: db_path.to_str().unwrap().into(),
        username: String::new(),
        trust_server_certificate: false,
        sheet: None,
        encoding: None,
        has_header: None,
    };
    let driver = db::create_driver(&config, &SecretString::new(String::new())).unwrap();
    driver
        .query_all("CREATE TABLE staging (name TEXT, qty INTEGER)", None)
        .await
        .unwrap();
    driver
        .query_all(
            "INSERT INTO staging (name, qty) VALUES ('A', 1), ('B', 2)",
            None,
        )
        .await
        .unwrap();
    driver
        .query_all(
            "CREATE TABLE items (name TEXT, qty INTEGER, src TEXT)",
            None,
        )
        .await
        .unwrap();

    let script = parser::parse("[items] ADD { name = name, qty = qty, src = N'db' } GO").unwrap();
    let summary = run(
        driver.clone(),
        Dialect::Sqlite,
        ResolvedScriptJob {
            job_id: Uuid::new_v4(),
            source: ScriptSource::Database {
                driver: driver.clone(),
                query: "SELECT name, qty FROM staging ORDER BY name".into(),
            },
            batch_size: 5000,
        },
        script,
        |_| {},
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(summary.status, "completed");
    assert_eq!(summary.total_rows, 2);
    assert_eq!(summary.success_rows, 2);
    assert_eq!(summary.error_rows, 0);

    let rows = driver
        .query_all("SELECT name, qty, src FROM items ORDER BY name", None)
        .await
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], CellValue::Text("A".into()));
    assert_eq!(rows[0][1], CellValue::Int(1));
    assert_eq!(rows[1][2], CellValue::Text("db".into()));
}
