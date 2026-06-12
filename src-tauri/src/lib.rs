pub mod commands;
pub mod db;
pub mod models;
pub mod security;
pub mod telemetry;

/// Phase 0 煙霧測試用 command：驗證 Angular ↔ Rust IPC 通路。
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {name}! You've been greeted from Rust!")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    telemetry::init();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(db::pool::AppState::default())
        .invoke_handler(tauri::generate_handler![
            greet,
            commands::database::test_connection,
            commands::database::get_tables,
            commands::database::get_columns,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
