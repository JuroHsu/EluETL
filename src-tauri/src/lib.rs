pub mod commands;
pub mod db;
pub mod etl;
pub mod excel;
pub mod models;
pub mod security;
pub mod state;
pub mod telemetry;

use tauri::Manager;

use db::pool::AppState;
use state::store::StateStore;

/// Phase 0 煙霧測試用 command：驗證 Angular ↔ Rust IPC 通路。
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {name}! You've been greeted from Rust!")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    telemetry::init();
    if let Err(e) = security::keychain::init() {
        tracing::warn!(error = %e, "OS keychain 初始化失敗，密碼儲存功能不可用");
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(AppState::default())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let store = tauri::async_runtime::block_on(StateStore::init(&data_dir))?;
            app.state::<AppState>().set_store(store);
            tracing::info!(dir = %data_dir.display(), "狀態庫初始化完成");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            commands::database::test_connection,
            commands::database::save_connection,
            commands::database::list_connections,
            commands::database::delete_connection,
            commands::database::get_tables,
            commands::database::get_columns,
            commands::database::query_preview,
            commands::database::export_query_to_excel,
            commands::excel::list_sheets,
            commands::excel::read_preview,
            commands::etl::execute_etl,
            commands::etl::cancel_etl,
            commands::etl::resume_etl,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
